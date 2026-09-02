#![allow(clippy::missing_errors_doc)]

/// File locking API endpoints for the Fast.io REST API.
///
/// Acquire, check, and release exclusive locks on files in workspaces or shares.
use std::collections::HashMap;

use serde_json::Value;

use crate::client::ApiClient;
use crate::error::CliError;

/// Acquire an exclusive lock on a file.
///
/// `POST /{context_type}/{context_id}/storage/{node_id}/lock/` — optional
/// `duration` (60-3600 seconds) sets the lock lifetime and `client_info`
/// (a JSON object, e.g. `{"device_name":"…","client_version":"…"}`) records
/// client metadata.
pub async fn lock_acquire(
    client: &ApiClient,
    context_type: &str,
    context_id: &str,
    node_id: &str,
    duration: Option<u32>,
    client_info: Option<&str>,
) -> Result<Value, CliError> {
    let form = crate::api::storage::lock_acquire_form(duration, client_info);
    let path = format!(
        "/{}/{}/storage/{}/lock/",
        urlencoding::encode(context_type),
        urlencoding::encode(context_id),
        urlencoding::encode(node_id),
    );
    client.post(&path, &form).await
}

/// Check lock status for a file.
///
/// `GET /{context_type}/{context_id}/storage/{node_id}/lock/`
pub async fn lock_status(
    client: &ApiClient,
    context_type: &str,
    context_id: &str,
    node_id: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/{}/{}/storage/{}/lock/",
        urlencoding::encode(context_type),
        urlencoding::encode(context_id),
        urlencoding::encode(node_id),
    );
    client.get(&path).await
}

/// Renew (heartbeat) an existing lock on a file.
///
/// `POST /{context_type}/{context_id}/storage/{node_id}/lock/heartbeat/`
///
/// Extends the lock's expiry timer. The `lock_token` is the token returned
/// by `lock_acquire` and must be provided to prove ownership of the lock.
pub async fn lock_heartbeat(
    client: &ApiClient,
    context_type: &str,
    context_id: &str,
    node_id: &str,
    lock_token: &str,
) -> Result<Value, CliError> {
    let mut form = HashMap::new();
    form.insert("lock_token".to_owned(), lock_token.to_owned());
    let path = format!(
        "/{}/{}/storage/{}/lock/heartbeat/",
        urlencoding::encode(context_type),
        urlencoding::encode(context_id),
        urlencoding::encode(node_id),
    );
    client.post(&path, &form).await
}

/// Release a lock on a file.
///
/// `DELETE /{context_type}/{context_id}/storage/{node_id}/lock/`
///
/// The `lock_token` is the token returned by `lock_acquire` and must be
/// provided to prove ownership of the lock.
///
/// The token is delivered as a **query parameter**, not a form body: the
/// server does not read a request body on `DELETE` for this endpoint. Sending
/// it as a form rejects the release with `205516 "lock_token: This field is
/// missing."` and leaves the node locked until the lease expires.
pub async fn lock_release(
    client: &ApiClient,
    context_type: &str,
    context_id: &str,
    node_id: &str,
    lock_token: &str,
) -> Result<Value, CliError> {
    let mut params = HashMap::new();
    params.insert("lock_token".to_owned(), lock_token.to_owned());
    let path = format!(
        "/{}/{}/storage/{}/lock/",
        urlencoding::encode(context_type),
        urlencoding::encode(context_id),
        urlencoding::encode(node_id),
    );
    client.delete_with_params_scrubbed(&path, &params).await
}

#[cfg(test)]
mod tests {
    use super::{lock_release, lock_status};
    use crate::client::ApiClient;
    use std::sync::{Arc, Mutex};

    /// Serve one canned envelope and hand back the raw request text.
    ///
    /// The point is the *request*, not the response: these tests assert how a
    /// field reached the wire, which no response-shape assertion can catch.
    async fn spawn_capturing_server() -> (String, Arc<Mutex<String>>) {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback");
        let addr = listener.local_addr().expect("local addr").to_string();
        let seen = Arc::new(Mutex::new(String::new()));
        let sink = Arc::clone(&seen);
        tokio::spawn(async move {
            if let Ok((mut sock, _)) = listener.accept().await {
                let mut buf = vec![0u8; 4096];
                if let Ok(n) = sock.read(&mut buf).await {
                    *sink.lock().expect("capture lock") =
                        String::from_utf8_lossy(&buf[..n]).into_owned();
                }
                let body = br#"{"result":"yes","response":{"released":true}}"#;
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
        (addr, seen)
    }

    /// `lock_token` must ride the QUERY STRING, and the request must carry no
    /// body.
    ///
    /// The Fast.io API does not read a request body on `DELETE`: sending the
    /// token as a form returns `205516 "lock_token: This field is missing."`
    /// and the node stays locked — a release that reports nothing wrong and
    /// does not release. Verified live against the API with a positive control
    /// (query string on the same token released it).
    #[tokio::test]
    async fn lock_release_sends_token_as_query_not_body() {
        let (addr, seen) = spawn_capturing_server().await;
        let client = ApiClient::new(&format!("http://{addr}"), Some("tok".to_owned()))
            .expect("client builds");

        let _ = lock_release(&client, "workspace", "ws1", "node1", "tok-abc").await;

        let req = seen.lock().expect("capture lock").clone();
        let (head, body) = req.split_once("\r\n\r\n").unwrap_or((req.as_str(), ""));
        let request_line = head.lines().next().unwrap_or_default();

        assert!(
            request_line.starts_with("DELETE "),
            "expected a DELETE, got: {request_line}"
        );
        assert!(
            request_line.contains("lock_token=tok-abc"),
            "lock_token must be delivered in the query string, got: {request_line}"
        );
        assert!(
            !body.contains("lock_token"),
            "lock_token must NOT be sent as a request body (the server ignores it), got: {body:?}"
        );
    }

    /// A transport failure must NOT render the lock token.
    ///
    /// The token moved into the QUERY STRING (the server ignores a DELETE body),
    /// and `reqwest` attaches the full URL to a transport error — so without the
    /// scrubbed send path a refused connection prints the credential to stderr,
    /// logs, or an MCP tool result.
    #[tokio::test]
    async fn lock_release_transport_error_does_not_leak_the_token() {
        // Bind then drop, so the port is closed and the connection is refused.
        let addr = {
            let l = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind loopback");
            l.local_addr().expect("local addr").to_string()
        };
        let client = ApiClient::new(&format!("http://{addr}"), Some("tok".to_owned()))
            .expect("client builds");

        let secret = "SUPERSECRETLOCKTOKEN";
        let err = lock_release(&client, "workspace", "ws1", "node1", secret)
            .await
            .expect_err("a closed port must fail");

        let rendered = format!("{err}");
        let debugged = format!("{err:?}");
        assert!(
            !rendered.contains(secret),
            "lock token leaked via Display: {rendered}"
        );
        assert!(
            !debugged.contains(secret),
            "lock token leaked via Debug: {debugged}"
        );
    }

    /// Guard the sibling that is easy to break the same way: status is a plain
    /// GET on the same path, so a regression that moved the token into a body
    /// would leave this one silently unauthenticated for the lock.
    #[tokio::test]
    async fn lock_status_path_is_the_lock_resource() {
        let (addr, seen) = spawn_capturing_server().await;
        let client = ApiClient::new(&format!("http://{addr}"), Some("tok".to_owned()))
            .expect("client builds");

        let _ = lock_status(&client, "workspace", "ws1", "node1").await;

        let req = seen.lock().expect("capture lock").clone();
        let request_line = req.lines().next().unwrap_or_default().to_owned();
        assert!(
            request_line.starts_with("GET ") && request_line.contains("/storage/node1/lock/"),
            "unexpected status request line: {request_line}"
        );
    }
}
