//! Wire pin for OAuth refresh-token revocation on sign-out.

use std::sync::{Arc, Mutex};

use fastio_cli::client::ApiClient;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

/// One-shot capture server; returns the base URL and the captured request.
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
            let body = br#"{"result":true}"#;
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

/// `POST /oauth/revoke/` takes the refresh token in a form field named
/// `token`, UNAUTHENTICATED.
///
/// Signing out must revoke the refresh token: those tokens are long-lived (the
/// published API docs' own example expires ten years out), so skipping
/// revocation would leave a decade-valid credential on the server. This is NOT
/// the same endpoint as `api::auth::oauth_revoke`, which is
/// `DELETE /oauth/sessions/{id}/` — near-identical name, different route,
/// different auth.
#[tokio::test]
async fn oauth_revoke_token_posts_the_token_unauthenticated() {
    let (base, captured) = spawn_capture_server().await;
    // A client with NO bearer: the endpoint is unauthenticated, and requiring a
    // live access token would defeat the point — the common case is revoking
    // after the access token has already lapsed.
    let client = ApiClient::new(&base, None).expect("client builds");

    let _ = fastio_cli::api::auth::oauth_revoke_token(&client, "rt-abc-123").await;

    let req = captured.lock().expect("capture lock").clone();
    assert!(!req.is_empty(), "the capture server saw no request");
    assert!(
        req.contains("POST /oauth/revoke/"),
        "must hit the RFC 7009 token-revocation route, NOT /oauth/sessions/:\n{req}"
    );
    assert!(
        req.contains("token=rt-abc-123"),
        "the refresh token must be sent in the `token` form field:\n{req}"
    );
    assert!(
        req.contains("x-www-form-urlencoded"),
        "the route is form-encoded:\n{req}"
    );
    assert!(
        !req.to_lowercase().contains("authorization: bearer"),
        "must not require a bearer — revocation has to work after the access \
         token has lapsed:\n{req}"
    );
}
