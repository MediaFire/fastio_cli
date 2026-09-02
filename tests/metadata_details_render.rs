//! End-to-end render locks for `fastio metadata details`.
//!
//! WHY THIS FILE EXISTS: the declared-type strip is a helper in the library,
//! but the guarantee lives at the four places it is WIRED IN — and those are
//! in the binary crate, where the in-crate tests only ever call the helpers
//! directly. Deleting either CLI call site left the whole suite green, and the
//! `--format json` passthrough was not locked on either production path.
//!
//! These tests drive the real `fastio` binary against a loopback stub serving
//! the documented metadata-details envelope, so what is asserted is the wiring:
//! human formats must not render the template-declared type beside the stored
//! value, and `--format json` must still carry it verbatim.
//!
//! The stub binds `127.0.0.1:0` and `--api-base` is the only base URL the
//! child is given. That alone is NOT enough to keep the traffic local: reqwest
//! discovers system proxies by default and hyper-util's interception has no
//! implicit loopback bypass, so a developer's `HTTP_PROXY` would divert even a
//! `127.0.0.1` request — carrying the `Authorization` header — to the proxy
//! host, and the stub would receive nothing. The `env_remove`s in
//! `details_stdout` are what do the real work; the `NO_PROXY` value it sets
//! carries EXPLICIT loopback entries because a bare `*` cannot bypass an
//! IP-literal host (see the call site).

use serde_json::{Value, json};

/// The template-DECLARED type. The strip must remove it from human output.
const DECLARED_TYPE: &str = "float";
/// The STORED value of the first entry. It disagrees with the declared type —
/// which is exactly why the CLI will not render that type beside it.
const STORED_VALUE: &str = "FIRST-NOT-A-NUMBER";
/// The STORED value of the second entry in the bulk fixture. It must not be a
/// substring of (or contain) [`STORED_VALUE`]: if one contained the other, the
/// bulk positive control would be satisfied by a single object, and the bulk
/// test would still pass with the other object dropped entirely.
const STORED_VALUE_2: &str = "SECOND-NOPE";
/// Workspace id passed to the CLI (validated client-side, never dereferenced).
const WORKSPACE: &str = "4687730903718774523";
/// Human-facing output formats: every one of these must drop the declared type.
const HUMAN_FORMATS: [&str; 3] = ["markdown", "table", "csv"];

/// One metadata entry in the documented shape
/// `{key, description, type, value, is_auto, updated}`.
fn entry(key: &str, value: &str) -> Value {
    json!({
        "key": key,
        "description": "Invoice total",
        "type": DECLARED_TYPE,
        "value": value,
        "is_auto": false,
        "updated": "2026-01-28 12:30:00 UTC"
    })
}

/// The single-node `GET …/metadata/details/` envelope.
fn single_body() -> String {
    json!({
        "result": "yes",
        "response": {
            "object_id": "abc123",
            "template_id": "mtemplate1",
            "node_id": {"id": "abc123", "name": "invoice.pdf", "type": "file"},
            "template_metadata": [entry("invoice_total", STORED_VALUE)],
            "custom_metadata": [],
            "autoextractable": true
        }
    })
    .to_string()
}

/// The bulk (`format: "multi"`) envelope for two node ids.
///
/// `templates.{id}.fields[].type` is a SCHEMA declaration, not a claim about a
/// stored value, so the strip must leave it alone. Nothing in THIS file can
/// observe that: the human render path passes only `objects[]` to the
/// renderer, so `templates` never reaches stdout, and the `--format json` path
/// never calls the strip at all. The over-strip direction is locked in
/// `src/api/metadata.rs` (`strip_declared_types_descends_into_bulk_objects`),
/// against the real payload the MCP arm renders.
fn bulk_body() -> String {
    json!({
        "result": "yes",
        "response": {
            "format": "multi",
            "objects": [
                {
                    "object_id": "abc123",
                    "template_id": "mtemplate1",
                    "node_id": {"id": "abc123", "name": "invoice.pdf", "type": "file"},
                    "template_metadata": [entry("invoice_total", STORED_VALUE)],
                    "custom_metadata": []
                },
                {
                    "object_id": "def456",
                    "template_id": "mtemplate1",
                    "node_id": {"id": "def456", "name": "invoice2.pdf", "type": "file"},
                    "template_metadata": [entry("invoice_total", STORED_VALUE_2)],
                    "custom_metadata": []
                }
            ],
            "templates": {
                "mtemplate1": {
                    "template_id": "mtemplate1",
                    "name": "Invoice",
                    "fields": [{"name": "invoice_total", "type": "string"}]
                }
            },
            "errors": []
        }
    })
    .to_string()
}

/// Serve `body` as a single `200 OK` JSON response on a fresh loopback port,
/// then close. Same one-shot idiom as the client unit tests
/// (`src/client.rs::spawn_one_shot_server`). Returns `127.0.0.1:<port>`.
async fn spawn_details_stub(body: String) -> String {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback");
    let addr = listener.local_addr().expect("local addr").to_string();
    tokio::spawn(async move {
        if let Ok((mut sock, _)) = listener.accept().await {
            // Drain the request headers (read once; enough for a GET).
            let mut buf = [0u8; 4096];
            let _ = sock.read(&mut buf).await;
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = sock.write_all(header.as_bytes()).await;
            let _ = sock.write_all(body.as_bytes()).await;
            let _ = sock.flush().await;
        }
    });
    addr
}

/// A throwaway `HOME` so the child never reads the developer's real
/// `~/.fastio` profile or credentials.
fn isolated_home() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("fastio-details-render-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create isolated home");
    dir
}

/// Run `fastio metadata details` against `addr` and return its stdout.
///
/// Asserts a successful exit first, so a broken invocation fails loudly here
/// rather than as a confusing empty-output assertion downstream.
async fn details_stdout(addr: &str, format: &str, node_ids: &[&str]) -> String {
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_fastio"))
        .arg("--api-base")
        .arg(format!("http://{addr}"))
        .arg("--token")
        .arg("test-token")
        .arg("--format")
        .arg(format)
        .arg("--no-color")
        .arg("metadata")
        .arg("details")
        .arg("--workspace")
        .arg(WORKSPACE)
        .args(node_ids)
        .env("HOME", isolated_home())
        .env_remove("FASTIO_TOKEN")
        .env_remove("FASTIO_API_KEY")
        // `dirs` prefers XDG_CONFIG_HOME over $HOME on Linux, so without this
        // the HOME override above is a no-op there and the child would read —
        // and write — the developer's real config directory.
        .env_remove("XDG_CONFIG_HOME")
        // Keep the request on the loopback interface. reqwest discovers system
        // proxies by default and hyper-util's interception has NO implicit
        // loopback bypass, so an inherited HTTP_PROXY sends this request (with
        // its Authorization header) to the proxy host instead of the stub.
        // The `env_remove`s below are what do the real work: they cover every
        // env-configured proxy, which is the case that actually occurs here.
        //
        // NO_PROXY is the backstop for a proxy configured OUTSIDE the
        // environment (macOS System Settings), which hyper-util merges in
        // underneath it. The load-bearing part of that value is the EXPLICIT
        // loopback list, not the leading `*`: `NoProxy::from_string`
        // (hyper-util matcher.rs:445) sorts each entry into an IP matcher or a
        // domain matcher, and `*` parses as neither an address nor a network,
        // so it only ever lands in the DOMAIN matcher. `NoProxy::contains`
        // (:470) parses the host first, and a `127.0.0.1` host dispatches to
        // the IP matcher — which a bare `*` leaves empty — so it never reaches
        // the `d == "*"` arm at :536. `127.0.0.1`/`::1` DO populate that IP
        // matcher, and `mac::with_system` (:588) fills only `http`/`https` and
        // never touches the bypass list, so they survive the merge.
        //
        // Evidence, stated honestly: the `*`-vs-loopback difference is MEASURED
        // (this binary, an env-configured proxy, `*` proxied a `127.0.0.1`
        // request and the loopback list did not). The system-proxy leg is
        // SOURCE-VERIFIED only — no run here enabled a macOS system proxy.
        .env_remove("HTTP_PROXY")
        .env_remove("http_proxy")
        .env_remove("HTTPS_PROXY")
        .env_remove("https_proxy")
        .env_remove("ALL_PROXY")
        .env_remove("all_proxy")
        .env("NO_PROXY", "*,127.0.0.1,::1,localhost")
        .output()
        .await
        .expect("run the fastio binary");
    assert!(
        output.status.success(),
        "`metadata details --format {format}` failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("stdout is utf-8")
}

#[tokio::test]
async fn human_formats_drop_the_declared_type_on_the_single_node_path() {
    for format in HUMAN_FORMATS {
        let addr = spawn_details_stub(single_body()).await;
        let stdout = details_stdout(&addr, format, &["abc123"]).await;
        assert!(
            stdout.contains(STORED_VALUE),
            "--format {format} must render the stored value; got:\n{stdout}"
        );
        assert!(
            !stdout.contains(DECLARED_TYPE),
            "--format {format} must not render the declared type {DECLARED_TYPE:?}; got:\n{stdout}"
        );
    }
}

#[tokio::test]
async fn json_keeps_the_declared_type_on_the_single_node_path() {
    let addr = spawn_details_stub(single_body()).await;
    let stdout = details_stdout(&addr, "json", &["abc123"]).await;
    let parsed: Value = serde_json::from_str(&stdout).expect("stdout is json");
    assert_eq!(
        parsed["template_metadata"][0]["type"], DECLARED_TYPE,
        "--format json is the machine-readable contract and must keep the \
         declared type verbatim; got:\n{stdout}"
    );
}

#[tokio::test]
async fn human_formats_drop_the_declared_type_on_the_bulk_path() {
    for format in HUMAN_FORMATS {
        let addr = spawn_details_stub(bulk_body()).await;
        let stdout = details_stdout(&addr, format, &["abc123", "def456"]).await;
        for value in [STORED_VALUE, STORED_VALUE_2] {
            assert!(
                stdout.contains(value),
                "--format {format} must render every stored value; got:\n{stdout}"
            );
        }
        assert!(
            !stdout.contains(DECLARED_TYPE),
            "--format {format} must not render the declared type {DECLARED_TYPE:?}; got:\n{stdout}"
        );
    }
}

#[tokio::test]
async fn json_keeps_the_declared_type_on_the_bulk_path() {
    let addr = spawn_details_stub(bulk_body()).await;
    let stdout = details_stdout(&addr, "json", &["abc123", "def456"]).await;
    let parsed: Value = serde_json::from_str(&stdout).expect("stdout is json");
    for index in 0..2 {
        assert_eq!(
            parsed["objects"][index]["template_metadata"][0]["type"], DECLARED_TYPE,
            "--format json must keep the declared type on every bulk object; got:\n{stdout}"
        );
    }
}
