//! Wire-level pins for the org storage change feed.
//!
//! The feed's cursor is an opaque base64 string that routinely carries `+`,
//! `/` and `=`. The transport encodes query values itself, so a cursor that is
//! also encoded by the caller arrives double-encoded and the server refuses it
//! as invalid — which reads as a server-side cursor problem, not a client bug.
//! These tests pin the path, the single encoding, and the error hints against
//! the real response bodies.

use std::sync::{Arc, Mutex};

use fastio_cli::api::event::{
    HINT_CHANGES_CURSOR_EXPIRED, HINT_CHANGES_CURSOR_INVALID, map_org_changes_error, org_changes,
};
use fastio_cli::client::ApiClient;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

/// A one-shot server that CAPTURES the raw request and answers with `status`
/// and `body`. Returns the base URL and a handle to the captured request text.
async fn spawn_server(status: &'static str, body: &'static [u8]) -> (String, Arc<Mutex<String>>) {
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
            let header = format!(
                "{status}\r\nContent-Type: application/json\r\n\
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

const PAGE: &[u8] = br#"{"result":true,"changes":[],"cursor":"next","has_more":false,"profiles":{"version":"v1","items":[]}}"#;

/// A cursor carrying `+`, `/` and `=` arrives encoded exactly ONCE.
#[tokio::test]
async fn cursor_is_encoded_exactly_once() {
    let (base, captured) = spawn_server("HTTP/1.1 200 OK", PAGE).await;
    let client = ApiClient::new(&base, Some("tok".to_owned())).expect("client builds");

    let value = org_changes(&client, "1111111111111111111", Some("ab+c/d=="), Some(250))
        .await
        .expect("page parses");
    assert_eq!(value["cursor"], "next");

    let req = captured.lock().expect("capture lock").clone();
    assert!(
        req.contains("GET /org/1111111111111111111/events/changes/?"),
        "path must match the documented route:\n{req}"
    );
    assert!(
        req.contains("cursor=ab%2Bc%2Fd%3D%3D"),
        "cursor must be percent-encoded once:\n{req}"
    );
    assert!(
        !req.contains("%25"),
        "cursor was double-encoded (a literal % was re-encoded):\n{req}"
    );
    assert!(req.contains("limit=250"), "limit must be sent:\n{req}");
}

/// Bootstrap sends no query string at all.
#[tokio::test]
async fn bootstrap_sends_no_query() {
    let (base, captured) = spawn_server("HTTP/1.1 200 OK", PAGE).await;
    let client = ApiClient::new(&base, Some("tok".to_owned())).expect("client builds");

    let _ = org_changes(&client, "1111111111111111111", None, None).await;

    let req = captured.lock().expect("capture lock").clone();
    assert!(
        req.contains("GET /org/1111111111111111111/events/changes/ HTTP"),
        "bootstrap must carry no query:\n{req}"
    );
}

/// The measured invalid-cursor body, extracted by the real client, gets the
/// re-bootstrap hint.
#[tokio::test]
async fn invalid_cursor_body_maps_to_the_rebootstrap_hint() {
    let (base, _) = spawn_server(
        "HTTP/1.1 406 Not Acceptable",
        br#"{"result":false,"error":{"code":158008,"text":"Invalid cursor.","resource":"GET Org [param] Events Changes"}}"#,
    )
    .await;
    let client = ApiClient::new(&base, Some("tok".to_owned())).expect("client builds");

    let err = org_changes(&client, "1111111111111111111", Some("abc"), None)
        .await
        .expect_err("406 is an error");
    assert_eq!(
        map_org_changes_error(err).suggestion(),
        Some(HINT_CHANGES_CURSOR_INVALID)
    );
}

/// The expired-cursor body carries `params` as an OBJECT; it must survive the
/// client's error extraction for the reason-keyed hint to fire.
#[tokio::test]
async fn expired_cursor_body_maps_to_the_resync_hint() {
    let (base, _) = spawn_server(
        "HTTP/1.1 406 Not Acceptable",
        br#"{"result":false,"error":{"code":187907,"text":"The cursor has expired. Resynchronize, then continue from a new cursor.","params":{"reason":"cursor_expired"}}}"#,
    )
    .await;
    let client = ApiClient::new(&base, Some("tok".to_owned())).expect("client builds");

    let err = org_changes(&client, "1111111111111111111", Some("abc"), None)
        .await
        .expect_err("406 is an error");
    assert_eq!(
        map_org_changes_error(err).suggestion(),
        Some(HINT_CHANGES_CURSOR_EXPIRED)
    );
}
