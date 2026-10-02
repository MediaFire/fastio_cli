//! RFC 8252 loopback redirect receiver for browser sign-in.
//!
//! [`bind`] claims an ephemeral port on `127.0.0.1`; the caller sends
//! [`LoopbackListener::redirect_uri`] as the OAuth `redirect_uri`, opens the
//! sign-in page, then calls [`LoopbackListener::wait`] to receive the
//! authorization code the browser is redirected back with.
//!
//! The listener is a tiny, deliberately strict HTTP/1 reader:
//!
//! - one absolute deadline for the whole wait, which no traffic can extend;
//! - every connection is handled concurrently with its own read deadline and
//!   size cap, so a stalled or oversized client never blocks a valid one;
//! - GET only, and the query is parsed and decoded rather than
//!   substring-matched — duplicate or missing parameters, a wrong `state`, or
//!   any path other than `/callback` are refused and the wait continues;
//! - exactly one connection can complete the wait (a one-shot channel), and
//!   its outcome is handed back before the page is written, so a slow write
//!   can never lose an accepted code to the deadline;
//! - the HTML it answers with is static: nothing from the request is echoed,
//!   and neither the request target nor the code is ever logged.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use secrecy::SecretString;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tokio::task::JoinSet;

/// Default absolute deadline for a sign-in: the server-side lifetime of an
/// authorization request.
pub const DEFAULT_DEADLINE: Duration = Duration::from_secs(10 * 60);

/// How long one connection may take to deliver its complete request headers.
const READ_DEADLINE: Duration = Duration::from_secs(5);

/// Upper bound on one connection's request (request line plus headers).
const MAX_REQUEST_BYTES: usize = 8 * 1024;

/// The only path the browser is redirected to.
const CALLBACK_PATH: &str = "/callback";

/// Errors from the loopback listener.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum LoopbackError {
    /// The listener could not be bound on `127.0.0.1`.
    #[error("failed to start the local sign-in listener: {0}")]
    Bind(#[source] std::io::Error),
    /// The deadline passed before the browser handed back a result.
    #[error("sign-in timed out")]
    Expired,
}

/// The terminal result of a sign-in handback.
#[derive(Debug)]
#[non_exhaustive]
pub enum LoopbackOutcome {
    /// The browser returned an authorization code with the expected `state`.
    Code(SecretString),
    /// The user declined (`error=access_denied`).
    Denied,
    /// The sign-in page reported some other error, with the expected `state`.
    Failed,
}

/// The one-shot slot the winning connection takes to hand back its outcome.
type OutcomeSlot = Arc<Mutex<Option<oneshot::Sender<LoopbackOutcome>>>>;

/// A bound loopback listener awaiting the browser redirect.
#[derive(Debug)]
pub struct LoopbackListener {
    listener: TcpListener,
    redirect_uri: String,
}

/// Bind a listener on `127.0.0.1` with an OS-assigned port.
///
/// Bind before starting the authorization request: the redirect URI sent to
/// the server must name the port that is actually listening.
///
/// # Errors
///
/// [`LoopbackError::Bind`] when the port cannot be bound.
pub async fn bind() -> Result<LoopbackListener, LoopbackError> {
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .map_err(LoopbackError::Bind)?;
    let port = listener.local_addr().map_err(LoopbackError::Bind)?.port();
    Ok(LoopbackListener {
        listener,
        redirect_uri: format!("http://127.0.0.1:{port}{CALLBACK_PATH}"),
    })
}

impl LoopbackListener {
    /// The redirect URI for this listener: `http://127.0.0.1:<port>/callback`.
    ///
    /// Send this exact string at authorize AND at token exchange — the server
    /// matches them exactly.
    #[must_use]
    pub fn redirect_uri(&self) -> &str {
        &self.redirect_uri
    }

    /// Wait for the browser redirect, for at most `deadline` from now.
    ///
    /// Returns the first terminal result: a [`LoopbackOutcome::Code`] whose
    /// `state` equals `expected_state`, a [`LoopbackOutcome::Denied`]
    /// (`error=access_denied`, with a matching or absent `state`), or a
    /// [`LoopbackOutcome::Failed`] (any other `error=` with a matching
    /// `state`). Everything else is answered with a 4xx and the wait goes on.
    ///
    /// The listener is consumed and closed before this returns. The returned
    /// future owns all its state, so it can be spawned on a task and aborted;
    /// aborting it closes the listener and the in-flight connections. The
    /// winning connection's page write runs on a detached task bounded to
    /// five seconds.
    ///
    /// A callback that won before the deadline fired is still returned: the
    /// winner claims the slot and sends in one locked section, and expiry
    /// takes the same lock, so expiry either closes the slot first or finds
    /// the outcome already in the channel.
    ///
    /// # Errors
    ///
    /// [`LoopbackError::Expired`] when `deadline` passes first.
    pub async fn wait(
        self,
        expected_state: String,
        deadline: Duration,
    ) -> Result<LoopbackOutcome, LoopbackError> {
        let LoopbackListener { listener, .. } = self;
        let expected_state: Arc<str> = Arc::from(expected_state);
        // Only expiry ever drops the sender unsent, and the loop ends right
        // after it, so `outcome_rx` is never polled once it has completed.
        let (outcome_tx, mut outcome_rx) = oneshot::channel();
        let slot: OutcomeSlot = Arc::new(Mutex::new(Some(outcome_tx)));
        let mut connections: JoinSet<()> = JoinSet::new();
        let expiry = tokio::time::sleep(deadline);
        tokio::pin!(expiry);

        let result = loop {
            tokio::select! {
                biased;
                Ok(outcome) = &mut outcome_rx => break Ok(outcome),
                Some(_) = connections.join_next() => {}
                () = &mut expiry => break settle_expiry(&slot, &mut outcome_rx),
                accepted = listener.accept() => match accepted {
                    Ok((stream, _)) => {
                        let state = Arc::clone(&expected_state);
                        let slot = Arc::clone(&slot);
                        connections.spawn(handle_connection(stream, state, slot));
                    }
                    // A failed accept (e.g. descriptor pressure) is not fatal:
                    // pause briefly so a persistent error cannot spin, and keep
                    // waiting under the same absolute deadline.
                    Err(_) => tokio::time::sleep(Duration::from_millis(20)).await,
                },
            }
        };
        drop(listener);
        connections.abort_all();
        result
    }
}

/// Resolve the deadline against a winner that may have sent after
/// `outcome_rx` was last polled.
///
/// Taking the sender under the slot lock decides the race: if it is still
/// there, nobody won, and dropping it refuses every later callback. If it is
/// gone, a winner took it AND sent while holding that same lock, so the
/// outcome is already in the channel and is read without waiting.
///
/// Only called when `outcome_rx` has not yet completed, so `try_recv` never
/// runs on a spent receiver.
fn settle_expiry(
    slot: &OutcomeSlot,
    outcome_rx: &mut oneshot::Receiver<LoopbackOutcome>,
) -> Result<LoopbackOutcome, LoopbackError> {
    let unclaimed = slot.lock().ok().and_then(|mut s| s.take());
    if unclaimed.is_some() {
        return Err(LoopbackError::Expired);
    }
    outcome_rx.try_recv().map_err(|_| LoopbackError::Expired)
}

/// What a single request asks for, after parsing.
#[derive(Debug)]
enum Verdict {
    /// Terminal: the wait completes with this outcome (if it wins the race).
    Terminal(LoopbackOutcome),
    /// Not terminal: answer with this status and keep waiting.
    Refuse(Status),
}

/// The non-success statuses the listener answers with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    BadRequest,
    NotFound,
    MethodNotAllowed,
    TooLarge,
}

/// Serve one connection; only the connection that wins sends an outcome.
async fn handle_connection(mut stream: TcpStream, expected_state: Arc<str>, slot: OutcomeSlot) {
    let verdict = match tokio::time::timeout(READ_DEADLINE, read_head(&mut stream)).await {
        Ok(Ok(head)) => classify(&head, &expected_state),
        Ok(Err(HeadError::TooLarge)) => Verdict::Refuse(Status::TooLarge),
        // Closed before the headers completed, a read error, or too slow:
        // nobody is waiting for an answer worth sending.
        Ok(Err(HeadError::Incomplete)) | Err(_) => return,
    };

    match verdict {
        Verdict::Refuse(status) => respond(&mut stream, refusal(status)).await,
        Verdict::Terminal(outcome) => {
            let page = match outcome {
                LoopbackOutcome::Code(_) => PAGE_SIGNED_IN,
                LoopbackOutcome::Denied => PAGE_DENIED,
                LoopbackOutcome::Failed => PAGE_FAILED,
            };
            // One-shot: only the first terminal request completes the wait.
            // Take the sender and send in ONE locked section (a oneshot send is
            // synchronous and never blocks), so expiry, which takes the same
            // lock, either closes the slot first or finds the outcome already
            // in the channel. The outcome is handed back BEFORE the page is
            // written, so a slow write cannot drop an accepted code.
            let won = slot.lock().is_ok_and(|mut s| match s.take() {
                Some(sender) => {
                    let _ = sender.send(outcome);
                    true
                }
                None => false,
            });
            if !won {
                respond(&mut stream, refusal(Status::BadRequest)).await;
                return;
            }
            // The page is best effort on a detached task: the wait aborts this
            // connection's task as soon as it has the outcome.
            tokio::spawn(async move { respond(&mut stream, ("200 OK", page)).await });
        }
    }
}

/// Why the request head could not be read.
#[derive(Debug)]
enum HeadError {
    /// The headers exceeded [`MAX_REQUEST_BYTES`].
    TooLarge,
    /// The peer closed (or errored) before the headers were complete.
    Incomplete,
}

/// Read until the `\r\n\r\n` that ends the request headers.
async fn read_head(stream: &mut TcpStream) -> Result<Vec<u8>, HeadError> {
    let mut acc: Vec<u8> = Vec::with_capacity(1024);
    let mut buf = [0u8; 1024];
    loop {
        let n = stream
            .read(&mut buf)
            .await
            .map_err(|_| HeadError::Incomplete)?;
        if n == 0 {
            return Err(HeadError::Incomplete);
        }
        acc.extend_from_slice(&buf[..n]);
        if let Some(end) = acc.windows(4).position(|w| w == b"\r\n\r\n") {
            if end + 4 > MAX_REQUEST_BYTES {
                return Err(HeadError::TooLarge);
            }
            acc.truncate(end + 4);
            return Ok(acc);
        }
        if acc.len() >= MAX_REQUEST_BYTES {
            return Err(HeadError::TooLarge);
        }
    }
}

/// Decide what a complete request head asks for.
fn classify(head: &[u8], expected_state: &str) -> Verdict {
    let Ok(text) = std::str::from_utf8(head) else {
        return Verdict::Refuse(Status::BadRequest);
    };
    let request_line = text.split("\r\n").next().unwrap_or_default();
    let mut parts = request_line.split(' ');
    let (Some(method), Some(target), Some(version), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Verdict::Refuse(Status::BadRequest);
    };
    if !version.starts_with("HTTP/1.") || !target.starts_with('/') {
        return Verdict::Refuse(Status::BadRequest);
    }
    if method != "GET" {
        return Verdict::Refuse(Status::MethodNotAllowed);
    }
    let Ok(url) = url::Url::parse(&format!("http://127.0.0.1{target}")) else {
        return Verdict::Refuse(Status::BadRequest);
    };
    if url.path() != CALLBACK_PATH {
        return Verdict::Refuse(Status::NotFound);
    }

    let mut codes: Vec<String> = Vec::new();
    let mut states: Vec<String> = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    for (key, value) in url.query_pairs() {
        match key.as_ref() {
            "code" => codes.push(value.into_owned()),
            "state" => states.push(value.into_owned()),
            "error" => errors.push(value.into_owned()),
            _ => {}
        }
    }
    if codes.len() > 1 || states.len() > 1 || errors.len() > 1 {
        return Verdict::Refuse(Status::BadRequest);
    }
    let state = states.pop();
    let state_matches = state.as_deref() == Some(expected_state);

    if let Some(error) = errors.pop() {
        // A deny next to a code is ambiguous; refuse rather than guess.
        if !codes.is_empty() {
            return Verdict::Refuse(Status::BadRequest);
        }
        // A deny may arrive without `state`; one carrying a DIFFERENT state is
        // not ours. Any other error ends the wait only with a matching state.
        return match (error.as_str(), state.is_none() || state_matches) {
            ("access_denied", true) => Verdict::Terminal(LoopbackOutcome::Denied),
            (_, _) if state_matches => Verdict::Terminal(LoopbackOutcome::Failed),
            _ => Verdict::Refuse(Status::BadRequest),
        };
    }

    match codes.pop() {
        Some(code) if !code.is_empty() && state_matches => {
            Verdict::Terminal(LoopbackOutcome::Code(SecretString::from(code)))
        }
        _ => Verdict::Refuse(Status::BadRequest),
    }
}

/// The status line and static page for a refusal.
fn refusal(status: Status) -> (&'static str, &'static str) {
    match status {
        Status::BadRequest => ("400 Bad Request", PAGE_BAD_REQUEST),
        Status::NotFound => ("404 Not Found", PAGE_NOT_FOUND),
        Status::MethodNotAllowed => ("405 Method Not Allowed", PAGE_BAD_REQUEST),
        Status::TooLarge => ("431 Request Header Fields Too Large", PAGE_BAD_REQUEST),
    }
}

/// Write a static response and close the connection. Failures are ignored:
/// the peer may already be gone, and that must not change the outcome.
async fn respond(stream: &mut TcpStream, (status, page): (&str, &str)) {
    let response = format!(
        "HTTP/1.1 {status}\r\n\
         Content-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\n\
         Cache-Control: no-store\r\n\
         Allow: GET\r\n\
         Connection: close\r\n\r\n{page}",
        page.len()
    );
    let write = async {
        stream.write_all(response.as_bytes()).await?;
        stream.shutdown().await
    };
    let _ = tokio::time::timeout(READ_DEADLINE, write).await;
}

const PAGE_SIGNED_IN: &str = "<!doctype html><html><head><meta charset=\"utf-8\">\
<title>Fastio CLI</title></head><body><h1>Signed in</h1>\
<p>You can close this tab and return to the terminal.</p></body></html>";

const PAGE_DENIED: &str = "<!doctype html><html><head><meta charset=\"utf-8\">\
<title>Fastio CLI</title></head><body><h1>Sign-in declined</h1>\
<p>You can close this tab.</p></body></html>";

const PAGE_FAILED: &str = "<!doctype html><html><head><meta charset=\"utf-8\">\
<title>Fastio CLI</title></head><body><h1>Sign-in failed</h1>\
<p>Return to the terminal and try again.</p></body></html>";

const PAGE_BAD_REQUEST: &str = "<!doctype html><html><head><meta charset=\"utf-8\">\
<title>Fastio CLI</title></head><body><h1>Bad request</h1></body></html>";

const PAGE_NOT_FOUND: &str = "<!doctype html><html><head><meta charset=\"utf-8\">\
<title>Fastio CLI</title></head><body><h1>Not found</h1></body></html>";

#[cfg(test)]
mod tests {
    use super::{
        LoopbackError, LoopbackListener, LoopbackOutcome, OutcomeSlot, bind, settle_expiry,
    };
    use secrecy::ExposeSecret as _;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use tokio::net::TcpStream;
    use tokio::sync::oneshot;

    const STATE: &str = "s-123";
    const LONG: Duration = Duration::from_secs(10);

    fn addr(listener: &LoopbackListener) -> String {
        listener
            .redirect_uri()
            .trim_start_matches("http://")
            .trim_end_matches("/callback")
            .to_owned()
    }

    /// Send a raw request and return the full response text.
    async fn send(addr: &str, raw: &str) -> String {
        let mut sock = TcpStream::connect(addr).await.expect("connect");
        sock.write_all(raw.as_bytes()).await.expect("write");
        let mut out = String::new();
        let _ = sock.read_to_string(&mut out).await;
        out
    }

    async fn get(addr: &str, target: &str) -> String {
        send(
            addr,
            &format!("GET {target} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n"),
        )
        .await
    }

    fn status_of(resp: &str) -> &str {
        resp.split(' ').nth(1).unwrap_or_default()
    }

    async fn start(
        deadline: Duration,
    ) -> (
        String,
        tokio::task::JoinHandle<Result<LoopbackOutcome, LoopbackError>>,
    ) {
        let listener = bind().await.expect("bind");
        let a = addr(&listener);
        let handle = tokio::spawn(listener.wait(STATE.to_owned(), deadline));
        (a, handle)
    }

    async fn finish(
        handle: tokio::task::JoinHandle<Result<LoopbackOutcome, LoopbackError>>,
    ) -> Result<LoopbackOutcome, LoopbackError> {
        tokio::time::timeout(LONG, handle)
            .await
            .expect("wait completes")
            .expect("task joins")
    }

    fn code_of(outcome: Result<LoopbackOutcome, LoopbackError>) -> String {
        match outcome {
            Ok(LoopbackOutcome::Code(c)) => c.expose_secret().to_owned(),
            other => panic!("expected a code, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn binds_127_0_0_1_and_names_the_port_in_the_redirect_uri() {
        let listener = bind().await.expect("bind");
        let local = listener.listener.local_addr().expect("addr");
        assert_eq!(local.ip().to_string(), "127.0.0.1");
        assert_eq!(
            listener.redirect_uri(),
            format!("http://127.0.0.1:{}/callback", local.port())
        );
    }

    #[tokio::test]
    async fn a_valid_callback_returns_the_code_and_a_static_page() {
        let (a, handle) = start(LONG).await;
        let resp = get(&a, "/callback?code=the%2Dcode-XYZ&state=s-123").await;
        assert_eq!(status_of(&resp), "200", "{resp}");
        assert!(resp.contains("Signed in"), "{resp}");
        assert!(
            !resp.contains("the-code-XYZ"),
            "the code must not be echoed"
        );
        assert!(
            !resp.contains("the%2Dcode"),
            "the target must not be echoed"
        );
        assert_eq!(code_of(finish(handle).await), "the-code-XYZ");
    }

    #[tokio::test]
    async fn a_wrong_state_is_refused_and_a_later_valid_callback_wins() {
        let (a, handle) = start(LONG).await;
        let resp = get(&a, "/callback?code=evil&state=other").await;
        assert_eq!(status_of(&resp), "400", "{resp}");
        assert!(!handle.is_finished());
        let resp = get(&a, "/callback?code=good&state=s-123").await;
        assert_eq!(status_of(&resp), "200", "{resp}");
        assert_eq!(code_of(finish(handle).await), "good");
    }

    #[tokio::test]
    async fn a_non_callback_path_is_404_and_the_wait_continues() {
        let (a, handle) = start(LONG).await;
        for target in ["/", "/favicon.ico", "/callbackx?code=c&state=s-123"] {
            let resp = get(&a, target).await;
            assert_eq!(status_of(&resp), "404", "{target}: {resp}");
        }
        get(&a, "/callback?code=c&state=s-123").await;
        assert_eq!(code_of(finish(handle).await), "c");
    }

    #[tokio::test]
    async fn non_get_is_405() {
        let (a, handle) = start(LONG).await;
        let resp = send(
            &a,
            "POST /callback?code=c&state=s-123 HTTP/1.1\r\nHost: x\r\nContent-Length: 0\r\n\r\n",
        )
        .await;
        assert_eq!(status_of(&resp), "405", "{resp}");
        get(&a, "/callback?code=c2&state=s-123").await;
        assert_eq!(code_of(finish(handle).await), "c2");
    }

    #[tokio::test]
    async fn duplicate_and_missing_params_are_400() {
        let (a, handle) = start(LONG).await;
        for target in [
            "/callback?code=a&code=b&state=s-123",
            "/callback?code=a&state=s-123&state=s-123",
            "/callback?code=a",
            "/callback?state=s-123",
            "/callback?code=&state=s-123",
            "/callback",
            "/callback?code=a&error=access_denied&state=s-123",
        ] {
            let resp = get(&a, target).await;
            assert_eq!(status_of(&resp), "400", "{target}: {resp}");
        }
        assert!(!handle.is_finished());
        get(&a, "/callback?code=ok&state=s-123").await;
        assert_eq!(code_of(finish(handle).await), "ok");
    }

    #[tokio::test]
    async fn access_denied_without_state_is_denied() {
        let (a, handle) = start(LONG).await;
        let resp = get(&a, "/callback?error=access_denied").await;
        assert_eq!(status_of(&resp), "200", "{resp}");
        assert!(resp.contains("declined"), "{resp}");
        assert!(matches!(finish(handle).await, Ok(LoopbackOutcome::Denied)));
    }

    #[tokio::test]
    async fn access_denied_with_state_is_denied() {
        let (a, handle) = start(LONG).await;
        get(&a, "/callback?error=access_denied&state=s-123").await;
        assert!(matches!(finish(handle).await, Ok(LoopbackOutcome::Denied)));
    }

    #[tokio::test]
    async fn access_denied_with_a_wrong_state_is_refused() {
        let (a, handle) = start(LONG).await;
        let resp = get(&a, "/callback?error=access_denied&state=nope").await;
        assert_eq!(status_of(&resp), "400", "{resp}");
        get(&a, "/callback?code=c&state=s-123").await;
        assert_eq!(code_of(finish(handle).await), "c");
    }

    #[tokio::test]
    async fn another_error_fails_only_with_the_matching_state() {
        let (a, handle) = start(LONG).await;
        let resp = get(&a, "/callback?error=server_error").await;
        assert_eq!(status_of(&resp), "400", "{resp}");
        get(&a, "/callback?error=server_error&state=s-123").await;
        assert!(matches!(finish(handle).await, Ok(LoopbackOutcome::Failed)));
    }

    #[tokio::test]
    async fn the_deadline_expires_and_traffic_does_not_reset_it() {
        let (a, handle) = start(Duration::from_millis(300)).await;
        // Stray traffic inside the window must not extend it.
        for _ in 0..3 {
            get(&a, "/nope").await;
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let started = std::time::Instant::now();
        assert!(matches!(finish(handle).await, Err(LoopbackError::Expired)));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[tokio::test]
    async fn a_stalled_connection_does_not_block_a_valid_callback() {
        let (a, handle) = start(LONG).await;
        // Opens, sends a partial request line, and never finishes the headers.
        let mut stalled = TcpStream::connect(&a).await.expect("connect");
        stalled.write_all(b"GET /callback?co").await.expect("write");
        let started = std::time::Instant::now();
        get(&a, "/callback?code=fast&state=s-123").await;
        assert_eq!(code_of(finish(handle).await), "fast");
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "the valid callback waited behind the stalled one"
        );
        drop(stalled);
    }

    /// An oversized request never completes the wait, even though it carries
    /// a valid code and state. The 431 is best-effort: the listener closes
    /// with the unread excess still buffered, so the peer usually sees a reset
    /// instead — either way it is refused and never told it signed in.
    #[tokio::test]
    async fn an_oversized_request_is_refused() {
        let (a, handle) = start(LONG).await;
        let huge = format!(
            "GET /callback?code=c&state=s-123 HTTP/1.1\r\nX-Pad: {}\r\n\r\n",
            "a".repeat(9000)
        );
        let resp = send(&a, &huge).await;
        assert!(
            resp.is_empty() || status_of(&resp) == "431",
            "oversized must be refused: {resp}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(!handle.is_finished(), "an oversized request must not win");
        get(&a, "/callback?code=small&state=s-123").await;
        assert_eq!(code_of(finish(handle).await), "small");
    }

    #[tokio::test]
    async fn two_concurrent_valid_callbacks_yield_exactly_one_code() {
        let (a, handle) = start(LONG).await;
        let (r1, r2) = tokio::join!(
            get(&a, "/callback?code=one&state=s-123"),
            get(&a, "/callback?code=two&state=s-123"),
        );
        let code = code_of(finish(handle).await);
        let statuses = [status_of(&r1), status_of(&r2)];
        // The loser is either refused or never answered (the wait completed
        // and closed its connection); it is never also told "signed in".
        assert_eq!(
            statuses.iter().filter(|s| **s == "200").count(),
            1,
            "exactly one browser may be told it signed in: {r1:?} / {r2:?}"
        );
        assert!(code == "one" || code == "two");
        let winner = if status_of(&r1) == "200" {
            "one"
        } else {
            "two"
        };
        assert_eq!(code, winner);
    }

    #[tokio::test]
    async fn the_code_survives_a_browser_that_disconnects_immediately() {
        let (a, handle) = start(LONG).await;
        let mut sock = TcpStream::connect(&a).await.expect("connect");
        sock.write_all(b"GET /callback?code=gone&state=s-123 HTTP/1.1\r\n\r\n")
            .await
            .expect("write");
        drop(sock);
        assert_eq!(code_of(finish(handle).await), "gone");
    }

    /// The outcome is handed back before the page is written: a browser that
    /// keeps the connection open and never reads the response still yields
    /// the code, well inside the deadline.
    #[tokio::test]
    async fn the_code_is_delivered_before_the_page_is_written() {
        let (a, handle) = start(LONG).await;
        let mut sock = TcpStream::connect(&a).await.expect("connect");
        sock.write_all(b"GET /callback?code=early&state=s-123 HTTP/1.1\r\n\r\n")
            .await
            .expect("write");
        let started = std::time::Instant::now();
        assert_eq!(code_of(finish(handle).await), "early");
        assert!(started.elapsed() < Duration::from_secs(2));
        drop(sock);
    }

    #[tokio::test]
    async fn the_listener_is_closed_after_the_wait() {
        let (a, handle) = start(LONG).await;
        get(&a, "/callback?code=c&state=s-123").await;
        finish(handle).await.expect("outcome");
        assert!(TcpStream::connect(&a).await.is_err(), "listener still open");
    }

    /// A winner that claimed the slot and sent just before the deadline fired
    /// (after `outcome_rx` was last polled) keeps its code: expiry finds the
    /// slot empty and reads the outcome already in the channel.
    #[test]
    fn expiry_preserves_an_outcome_claimed_before_it() {
        let (tx, mut rx) = oneshot::channel();
        let slot: OutcomeSlot = Arc::new(Mutex::new(Some(tx)));
        {
            let mut guard = slot.lock().expect("lock");
            let sender = guard.take().expect("sender");
            sender
                .send(LoopbackOutcome::Code("late".to_owned().into()))
                .expect("send");
        }
        assert_eq!(code_of(settle_expiry(&slot, &mut rx)), "late");
    }

    /// With no winner, expiry closes the slot, so a callback arriving after
    /// the deadline cannot claim it.
    #[test]
    fn expiry_without_a_winner_closes_the_slot() {
        let (tx, mut rx) = oneshot::channel();
        let slot: OutcomeSlot = Arc::new(Mutex::new(Some(tx)));
        assert!(matches!(
            settle_expiry(&slot, &mut rx),
            Err(LoopbackError::Expired)
        ));
        assert!(slot.lock().expect("lock").take().is_none());
    }

    #[test]
    fn outcome_debug_never_prints_the_code() {
        let outcome = LoopbackOutcome::Code("sekrit-code".to_owned().into());
        assert!(!format!("{outcome:?}").contains("sekrit-code"));
    }
}
