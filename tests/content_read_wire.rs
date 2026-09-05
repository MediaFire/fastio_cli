//! Wire-level encoding pins for the extracted-text (`/content/`) routes and the
//! unified search `details` flag.
//!
//! **Why these exist.** Everything these routes get wrong is invisible from
//! inside the crate: a path assembled one segment off, a parameter spelled
//! `query` instead of `q`, a `nodes` list joined with something other than a
//! comma, or a `details` flag emitted when it should have stayed off. Each of
//! those still compiles, still type-checks, and still returns a plausible
//! `200`-shaped envelope from a stub — only the bytes on the wire distinguish
//! them, so the bytes are what gets asserted.
//!
//! The multi-file route is the sharpest case: its file ids travel in a query
//! parameter rather than the path, so the route sits at
//! `/workspace/{id}/storage/content/` beside `search/` and NOT under a
//! `{node_id}`. A refactor that "made it consistent" with the single-file route
//! would produce a request the platform does not serve.

use std::sync::{Arc, Mutex};

use fastio_cli::api::search::UnifiedSearchParams;
use fastio_cli::api::storage::{ContentManyParams, ContentReadParams};
use fastio_cli::client::ApiClient;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

/// A one-shot server that CAPTURES the raw request and answers `{"result":true}`.
///
/// Returns the base URL and a handle to the captured request text. Mirrors the
/// helper in `metadata_wire_encoding.rs`; the pattern exposes no shared helper
/// crate, so each wire test file carries its own copy.
async fn spawn_capture_server() -> (String, Arc<Mutex<String>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback");
    let addr = listener.local_addr().expect("local addr").to_string();
    let captured = Arc::new(Mutex::new(String::new()));
    let sink = Arc::clone(&captured);
    tokio::spawn(async move {
        if let Ok((mut sock, _)) = listener.accept().await {
            let mut buf = vec![0u8; 8192];
            if let Ok(n) = sock.read(&mut buf).await {
                *sink.lock().expect("capture lock") =
                    String::from_utf8_lossy(&buf[..n]).into_owned();
            }
            let body = br#"{"result":true,"chunks":[]}"#;
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = sock.write_all(header.as_bytes()).await;
            let _ = sock.write_all(body).await;
            let _ = sock.flush().await;
        }
    });
    (format!("http://{addr}"), captured)
}

/// The request line of the captured request (`GET /path?query HTTP/1.1`).
fn request_line(captured: &Arc<Mutex<String>>) -> String {
    let req = captured.lock().expect("capture lock").clone();
    assert!(!req.is_empty(), "the server captured no request");
    req.lines().next().unwrap_or_default().trim_end().to_owned()
}

/// A relevance read travels as `q`, on the `{node_id}/content/` path.
#[tokio::test]
async fn single_file_relevance_read_encodes_q_on_the_content_path() {
    let (base, captured) = spawn_capture_server().await;
    let client = ApiClient::new(&base, Some("tok".to_owned())).expect("client builds");

    let params = ContentReadParams::new()
        .query(Some("retention policy"))
        .limit(Some(3));
    let _ = fastio_cli::api::storage::read_content_chunks(
        &client,
        "workspace",
        "4687730903718774523",
        "2ltsu-q4mja-cuv7p-gc5yd-lxnsj-wee4",
        &params,
    )
    .await;

    let line = request_line(&captured);
    assert!(
        line.starts_with(
            "GET /workspace/4687730903718774523/storage/\
             2ltsu-q4mja-cuv7p-gc5yd-lxnsj-wee4/content/?"
        ),
        "the single-file read hangs off the node id:\n{line}"
    );
    assert!(
        line.contains("q=retention+policy") || line.contains("q=retention%20policy"),
        "the relevance query travels as `q`, form-encoded:\n{line}"
    );
    assert!(line.contains("limit=3"), "{line}");
    for absent in ["page=", "chunk_from=", "cursor=", "search="] {
        assert!(!line.contains(absent), "`{absent}` must be absent:\n{line}");
    }
}

/// A positional read travels as `chunk_from`/`chunk_to` — never as an
/// offset/limit pair, and never on the share route's workspace spelling.
#[tokio::test]
async fn single_file_range_read_encodes_chunk_bounds_on_the_share_route() {
    let (base, captured) = spawn_capture_server().await;
    let client = ApiClient::new(&base, Some("tok".to_owned())).expect("client builds");

    let params = ContentReadParams::new()
        .chunks(Some(4), Some(9))
        .max_bytes(Some(65536))
        .output(Some("terse"));
    let _ = fastio_cli::api::storage::read_content_chunks(
        &client,
        "share",
        "1234567890123456789",
        "2ltsu-q4mja",
        &params,
    )
    .await;

    let line = request_line(&captured);
    assert!(
        line.starts_with("GET /share/1234567890123456789/storage/2ltsu-q4mja/content/?"),
        "the share twin is the same route under /share/:\n{line}"
    );
    assert!(line.contains("chunk_from=4"), "{line}");
    assert!(line.contains("chunk_to=9"), "{line}");
    assert!(line.contains("max_bytes=65536"), "{line}");
    assert!(line.contains("output=terse"), "{line}");
    assert!(!line.contains("q="), "no relevance query was set:\n{line}");
}

/// The multi-file route puts the ids in `nodes`, NOT in the path.
///
/// The comma separator is percent-encoded to `%2C` by the query serializer;
/// the server accepts either spelling, so this pins what the crate actually
/// produces rather than a preference.
#[tokio::test]
async fn multi_file_read_joins_nodes_with_commas_beside_search() {
    let (base, captured) = spawn_capture_server().await;
    let client = ApiClient::new(&base, Some("tok".to_owned())).expect("client builds");

    let params = ContentManyParams::new(
        vec![
            "2ltsu-q4mja".to_owned(),
            String::new(),
            "2h4mq-8ktz3".to_owned(),
        ],
        "retention policy",
    )
    .limit(Some(3));
    let _ =
        fastio_cli::api::storage::read_content_many(&client, "4687730903718774523", &params).await;

    let line = request_line(&captured);
    assert!(
        line.starts_with("GET /workspace/4687730903718774523/storage/content/?"),
        "the ids travel in `nodes`, so the route sits beside search/ rather \
         than under a {{node_id}}:\n{line}"
    );
    assert!(
        line.contains("nodes=2ltsu-q4mja%2C2h4mq-8ktz3"),
        "ids are comma-joined into ONE value (the comma percent-encodes to \
         %2C) and the blank segment is dropped:\n{line}"
    );
    assert!(
        line.contains("q=retention+policy") || line.contains("q=retention%20policy"),
        "`q` is required on this route:\n{line}"
    );
    assert!(line.contains("limit=3"), "{line}");
}

/// `details` reaches the unified search only when asked for.
#[tokio::test]
async fn unified_search_emits_details_only_when_set() {
    let (base, captured) = spawn_capture_server().await;
    let client = ApiClient::new(&base, Some("tok".to_owned())).expect("client builds");
    let _ = fastio_cli::api::search::unified_search_workspace(
        &client,
        "4687730903718774523",
        "quarterly report",
        UnifiedSearchParams::new().details(true),
    )
    .await;
    let line = request_line(&captured);
    assert!(
        line.starts_with("GET /workspace/4687730903718774523/search/?"),
        "{line}"
    );
    assert!(
        line.contains("details=true"),
        "`details` must be the literal string true:\n{line}"
    );

    // The negative control: unset must leave the request byte-identical to one
    // from before the parameter existed.
    let (base, captured) = spawn_capture_server().await;
    let client = ApiClient::new(&base, Some("tok".to_owned())).expect("client builds");
    let _ = fastio_cli::api::search::unified_search_workspace(
        &client,
        "4687730903718774523",
        "quarterly report",
        UnifiedSearchParams::new(),
    )
    .await;
    let line = request_line(&captured);
    assert!(
        !line.contains("details"),
        "an unset `details` must not reach the wire:\n{line}"
    );
}

/// A window conflict is refused BEFORE any request is sent — the point of the
/// client-side pre-flight is that the user gets a readable message instead of a
/// `406`, and that a doomed request never leaves the machine.
#[tokio::test]
async fn an_invalid_window_never_reaches_the_wire() {
    let (base, captured) = spawn_capture_server().await;
    let client = ApiClient::new(&base, Some("tok".to_owned())).expect("client builds");

    let params = ContentReadParams::new().query(Some("clause")).page(Some(2));
    let err = fastio_cli::api::storage::read_content_chunks(
        &client,
        "workspace",
        "4687730903718774523",
        "2ltsu-q4mja",
        &params,
    )
    .await
    .expect_err("two window selectors must be refused");
    let msg = err.to_string();
    assert!(msg.contains('q') && msg.contains("page"), "{msg}");
    assert!(
        captured.lock().expect("capture lock").is_empty(),
        "no request may be sent for a window the server would refuse"
    );
}
