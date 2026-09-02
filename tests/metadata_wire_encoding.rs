//! Wire-level encoding pins for the metadata write surface.
//!
//! **Why these exist.** Three of the four defects this branch fixed shipped for
//! the same reason: a wrong literal with no test asserting it. `/import/` vs
//! `/cloud-import/`, `state` vs `status`, and the `--fields` projection were all
//! visible only in a comment.
//!
//! The metadata family's encoding is in exactly that position. It uses **three
//! different conventions across three endpoints** — form for declare-field, JSON
//! for the node-facts write, and form-with-a-JSON-string-field for compound
//! search — and each is currently documented only in a doc comment. Nothing
//! would fail if someone "tidied" `compound_search` into `post_json`.
//!
//! That specific mistake is the one the platform docs flag in red as the most
//! common way to call compound search wrong: a JSON body does **not** populate
//! `filters` at all, and the request is refused as though no filters were sent —
//! `406`/`119701`, which reads as "my filter is invalid" when the real problem is
//! the Content-Type. These tests make that regression fail loudly and locally.

use std::sync::{Arc, Mutex};

use fastio_cli::client::ApiClient;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

/// A one-shot server that CAPTURES the raw request and answers `{"result":true}`.
///
/// Returns the base URL and a handle to the captured request text.
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
            let body = br#"{"result":true,"items":[],"scope":{}}"#;
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

/// Compound search MUST be form-encoded with `filters` as a JSON **string**
/// field. A JSON request body does not populate `filters` server-side at all.
#[tokio::test]
async fn compound_search_is_form_encoded_not_json() {
    let (base, captured) = spawn_capture_server().await;
    let client = ApiClient::new(&base, Some("tok".to_owned())).expect("client builds");

    let filters = r#"[{"field":"document_type","operator":"=","value":"contract"}]"#;
    let _ = fastio_cli::api::metadata::compound_search(
        &client,
        "4687730903718774523",
        filters,
        "early termination clause",
        Some(25),
    )
    .await;

    let req = captured.lock().expect("capture lock").clone();
    assert!(!req.is_empty(), "the server captured no request");

    assert!(
        req.contains("content-type: application/x-www-form-urlencoded")
            || req.contains("Content-Type: application/x-www-form-urlencoded"),
        "compound search must be FORM-encoded — a JSON body silently drops `filters` \
         and 406s with 119701, which reads as a bad filter rather than a bad encoding.\n\
         request was:\n{req}"
    );
    assert!(
        !req.contains("application/json"),
        "must not be sent as JSON:\n{req}"
    );
    assert!(
        req.contains("filters="),
        "`filters` must be present as a form field:\n{req}"
    );
    assert!(
        req.contains("POST /workspace/4687730903718774523/metadata/compound-search/"),
        "path must match the documented route:\n{req}"
    );
}

/// The node-facts write is the opposite convention — a JSON body. Pinned so the
/// two cannot be "made consistent" with each other.
#[tokio::test]
async fn node_facts_write_is_json_not_form() {
    let (base, captured) = spawn_capture_server().await;
    let client = ApiClient::new(&base, Some("tok".to_owned())).expect("client builds");

    let facts = serde_json::json!({"invoice_total": 1250.50});
    let _ = fastio_cli::api::metadata::write_node_facts(&client, "42", "aBcDeF", &facts).await;

    let req = captured.lock().expect("capture lock").clone();
    assert!(!req.is_empty(), "the server captured no request");
    assert!(
        req.contains("application/json"),
        "the node-facts write takes a JSON body — `facts` is a heterogeneous \
         nested map that form encoding cannot express:\n{req}"
    );
    assert!(
        req.contains(r#""facts""#),
        "the body must carry a `facts` object:\n{req}"
    );
}

/// Declaring a field is the third convention — form-encoded — and `constraints`
/// must never be sent (the route accepts it only in order to refuse it).
#[tokio::test]
async fn declare_field_is_form_encoded_and_omits_constraints() {
    let (base, captured) = spawn_capture_server().await;
    let client = ApiClient::new(&base, Some("tok".to_owned())).expect("client builds");

    let _ =
        fastio_cli::api::metadata::declare_metadata_field(&client, "42", "Invoice Total", "float")
            .await;

    let req = captured.lock().expect("capture lock").clone();
    assert!(!req.is_empty(), "the server captured no request");
    assert!(
        req.contains("x-www-form-urlencoded"),
        "declare-field is form-encoded:\n{req}"
    );
    assert!(
        !req.contains("constraints"),
        "`constraints` is refused by the route and must never be sent:\n{req}"
    );
}

/// The facts read must be immune to a `--detail` downgrade.
///
/// `client.rs` injects `?output=<detail>` on any path not in its deny list, and
/// `/metadata/facts/` is not on it. Without an explicit `output=full` a user's
/// global `--detail standard` would strip `declared_type`, `stored_type`,
/// `source`, `confidence`, `rationale` and `updated` — the provenance this
/// endpoint exists to return and which its doc comment promises a parser.
#[tokio::test]
async fn facts_read_pins_output_full_against_a_detail_downgrade() {
    use fastio_cli::output::OutputDetail;

    let (base, captured) = spawn_capture_server().await;
    // A client configured the way `fastio --detail terse …` configures one.
    let client = ApiClient::with_detail(&base, Some("tok".to_owned()), Some(OutputDetail::Terse))
        .expect("client builds");

    let _ = fastio_cli::api::metadata::get_node_facts(&client, "42", "aBcDeF").await;

    let req = captured.lock().expect("capture lock").clone();
    assert!(!req.is_empty(), "the server captured no request");
    assert!(
        req.contains("output=full"),
        "the facts read must pin output=full:\n{req}"
    );
    assert!(
        !req.contains("output=terse"),
        "a global --detail must NOT downgrade the facts read — it would empty \
         the provenance the endpoint exists to return:\n{req}"
    );
}
