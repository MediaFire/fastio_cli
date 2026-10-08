//! Wire-level pins for the CRC-32C upload integrity parameters.
//!
//! Every assertion here is about bytes on the wire: which chunk carries which
//! query parameter, whether a retry re-sends the whole-file value, which
//! multipart fields a whole-body upload carries, and which failures are never
//! retried. A scripted loopback server captures each request and answers from a
//! fixed list of responses.

use std::sync::{Arc, Mutex};

use bytes::Bytes;
use fastio_cli::api::upload::{self, BatchUploadItem};
use fastio_cli::api::upload_integrity::{
    ChunkIntegrity, ERR_UPLOAD_CRC32C_MISMATCH, SequentialChunkCrc32c, batch_entry_hash,
    crc32c_hex, is_crc32c_mismatch,
};
use fastio_cli::error::CliError;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

/// One captured request.
#[derive(Debug, Clone)]
struct Captured {
    /// `POST /path?query HTTP/1.1`
    request_line: String,
    /// Raw body bytes.
    body: Vec<u8>,
}

impl Captured {
    /// The value of query parameter `key` on the request line, if present.
    fn query(&self, key: &str) -> Option<String> {
        let target = self.request_line.split_whitespace().nth(1)?;
        let (_, query) = target.split_once('?')?;
        query.split('&').find_map(|pair| {
            let (k, v) = pair.split_once('=')?;
            (k == key).then(|| v.to_owned())
        })
    }

    fn body_text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    /// The value of a multipart text field named `name`, if present.
    fn multipart_field(&self, name: &str) -> Option<String> {
        let body = self.body_text();
        let marker = format!("name=\"{name}\"\r\n\r\n");
        let start = body.find(&marker)? + marker.len();
        let end = body[start..].find("\r\n")? + start;
        Some(body[start..end].to_owned())
    }
}

type Log = Arc<Mutex<Vec<Captured>>>;

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Read one full HTTP/1.1 request (headers plus a `Content-Length` or chunked
/// body) from the socket.
async fn read_request(sock: &mut tokio::net::TcpStream) -> Option<Captured> {
    let mut buf = Vec::new();
    let mut tmp = vec![0u8; 64 * 1024];
    let header_end = loop {
        if let Some(pos) = find_subsequence(&buf, b"\r\n\r\n") {
            break pos + 4;
        }
        let n = sock.read(&mut tmp).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&tmp[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..header_end]).into_owned();
    let request_line = head.lines().next().unwrap_or_default().to_owned();
    let lower = head.to_ascii_lowercase();
    let content_length = lower.lines().find_map(|l| {
        l.strip_prefix("content-length:")
            .and_then(|v| v.trim().parse::<usize>().ok())
    });
    let chunked = lower.contains("transfer-encoding: chunked");
    loop {
        let have = buf.len() - header_end;
        let done = match content_length {
            Some(len) => have >= len,
            None if chunked => find_subsequence(&buf[header_end..], b"0\r\n\r\n").is_some(),
            None => true,
        };
        if done {
            break;
        }
        let n = sock.read(&mut tmp).await.ok()?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
    }
    Some(Captured {
        request_line,
        body: buf[header_end..].to_vec(),
    })
}

/// Spawn a server that answers request `i` with `responses[i]` (the last
/// response repeats once the list is exhausted) and logs every request.
async fn spawn_scripted(responses: Vec<(u16, &'static str)>) -> (String, Log) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback");
    let addr = listener.local_addr().expect("local addr").to_string();
    let log: Log = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&log);
    tokio::spawn(async move {
        let mut served = 0usize;
        while let Ok((mut sock, _)) = listener.accept().await {
            let Some(req) = read_request(&mut sock).await else {
                continue;
            };
            sink.lock().expect("log lock").push(req);
            let (status, body) = responses
                .get(served)
                .or_else(|| responses.last())
                .copied()
                .unwrap_or((200, r#"{"result":"yes"}"#));
            served += 1;
            let header = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = sock.write_all(header.as_bytes()).await;
            let _ = sock.write_all(body.as_bytes()).await;
            let _ = sock.flush().await;
        }
    });
    (format!("http://{addr}"), log)
}

fn requests(log: &Log) -> Vec<Captured> {
    log.lock().expect("log lock").clone()
}

const OK: (u16, &str) = (200, r#"{"result":"yes"}"#);
const UNAVAILABLE: (u16, &str) = (503, r#"{"result":"no"}"#);
const MISMATCH: (u16, &str) = (
    406,
    r#"{"result":"no","error":{"code":10778,"text":"The uploaded file did not match its whole-file CRC-32C, so it was not saved. Upload the file again in a new session."}}"#,
);
const CONFLICT: (u16, &str) = (
    409,
    r#"{"result":"no","error":{"code":10779,"text":"A different whole-file CRC-32C is already set for this upload session. Resend the value sent first."}}"#,
);

fn sample(len: usize) -> Vec<u8> {
    (0..len)
        .map(|i| u8::try_from((i * 7 + 3) % 256).unwrap_or(0))
        .collect()
}

/// Drive a sequential chunked upload of `data` in `chunk` pieces through the
/// library chunk sender, exactly as the CLI's chunk loop does.
async fn send_sequential(base: &str, data: &[u8], chunk: usize, password: bool) {
    let mut state = SequentialChunkCrc32c::new(data.len() as u64);
    for (i, piece) in data.chunks(chunk).enumerate() {
        let integrity = state.next_chunk(piece);
        let order = u32::try_from(i + 1).expect("order fits");
        let result = if password {
            upload::upload_chunk_with_password(
                "tok",
                base,
                "sess",
                order,
                piece.to_vec(),
                &integrity,
                None,
            )
            .await
        } else {
            upload::upload_chunk("tok", base, "sess", order, piece.to_vec(), &integrity).await
        };
        result.expect("chunk accepted");
    }
}

fn assert_chunk_sequence(reqs: &[Captured], data: &[u8], chunk: usize) {
    let pieces: Vec<&[u8]> = data.chunks(chunk).collect();
    assert_eq!(reqs.len(), pieces.len(), "one request per chunk");
    for (i, (req, piece)) in reqs.iter().zip(&pieces).enumerate() {
        assert_eq!(req.query("order"), Some((i + 1).to_string()));
        assert_eq!(
            req.query("hash_algo").as_deref(),
            Some("crc32c"),
            "chunk {i}"
        );
        assert_eq!(req.query("hash"), Some(crc32c_hex(piece)), "chunk {i}");
        if i + 1 == pieces.len() {
            assert_eq!(
                req.query("file_crc32c"),
                Some(crc32c_hex(data)),
                "last chunk carries the whole-file CRC"
            );
        } else {
            assert_eq!(
                req.query("file_crc32c"),
                None,
                "chunk {i} must not carry it"
            );
        }
    }
}

#[tokio::test]
async fn every_chunk_carries_its_crc_and_only_the_last_carries_file_crc32c() {
    // 3 full chunks + a 1-byte last chunk, and an exact multiple of the chunk size.
    for (len, chunk) in [(3 * 64 + 1, 64), (4 * 64, 64)] {
        let (base, log) = spawn_scripted(vec![OK]).await;
        let data = sample(len);
        send_sequential(&base, &data, chunk, false).await;
        assert_chunk_sequence(&requests(&log), &data, chunk);
    }
}

#[tokio::test]
async fn password_chunk_path_carries_the_same_parameters() {
    let (base, log) = spawn_scripted(vec![OK]).await;
    let data = sample(200);
    send_sequential(&base, &data, 64, true).await;
    assert_chunk_sequence(&requests(&log), &data, 64);
}

#[tokio::test]
async fn retry_of_the_last_chunk_resends_file_crc32c() {
    // Chunk 1 OK, chunk 2 (last) answered 503 once, then OK.
    let (base, log) = spawn_scripted(vec![OK, UNAVAILABLE, OK]).await;
    let data = sample(100);
    send_sequential(&base, &data, 64, false).await;
    let reqs = requests(&log);
    assert_eq!(reqs.len(), 3, "the last chunk was retried once");
    assert_eq!(reqs[0].query("file_crc32c"), None);
    let whole = Some(crc32c_hex(&data));
    for retry in &reqs[1..] {
        assert_eq!(retry.query("order").as_deref(), Some("2"));
        assert_eq!(retry.query("hash"), Some(crc32c_hex(&data[64..])));
        assert_eq!(retry.query("file_crc32c"), whole);
    }
}

#[tokio::test]
async fn crc32c_mismatch_on_a_chunk_is_terminal_and_not_retried() {
    let (base, log) = spawn_scripted(vec![MISMATCH, OK]).await;
    let data = sample(10);
    let err = upload::upload_chunk(
        "tok",
        &base,
        "sess",
        1,
        data.clone(),
        &ChunkIntegrity::for_only_chunk(&data),
    )
    .await
    .expect_err("mismatch surfaces as an error");
    assert_eq!(requests(&log).len(), 1, "a 406 is never retried");
    assert!(is_crc32c_mismatch(&err), "{err:?}");
    match &err {
        CliError::Api(api) => {
            assert_eq!(api.code, ERR_UPLOAD_CRC32C_MISMATCH);
            assert_eq!(api.http_status, 406);
        }
        other => panic!("expected an API error, got {other:?}"),
    }
    let hint = err.suggestion().unwrap_or_default();
    assert!(hint.contains("nothing was stored"), "{hint}");
    assert!(hint.contains("Upload the file again"), "{hint}");
}

#[tokio::test]
async fn crc32c_conflict_is_not_retried() {
    let (base, log) = spawn_scripted(vec![CONFLICT, OK]).await;
    let data = sample(10);
    let err = upload::upload_chunk_with_password(
        "tok",
        &base,
        "sess",
        1,
        data.clone(),
        &ChunkIntegrity::for_only_chunk(&data),
        None,
    )
    .await
    .expect_err("conflict surfaces as an error");
    assert_eq!(requests(&log).len(), 1, "a 409 is never retried");
    match &err {
        CliError::Api(api) => assert_eq!((api.code, api.http_status), (10779, 409)),
        other => panic!("expected an API error, got {other:?}"),
    }
    assert_eq!(
        err.suggestion(),
        Some(fastio_cli::error::HINT_UPLOAD_CRC32C_CONFLICT)
    );
}

#[tokio::test]
async fn standalone_chunk_carries_only_its_own_crc() {
    let (base, log) = spawn_scripted(vec![OK]).await;
    let data = sample(33);
    upload::upload_chunk(
        "tok",
        &base,
        "sess",
        4,
        data.clone(),
        &ChunkIntegrity::for_chunk(&data),
    )
    .await
    .expect("chunk accepted");
    let reqs = requests(&log);
    assert_eq!(reqs[0].query("hash"), Some(crc32c_hex(&data)));
    assert_eq!(reqs[0].query("hash_algo").as_deref(), Some("crc32c"));
    assert_eq!(reqs[0].query("file_crc32c"), None);
}

#[tokio::test]
async fn single_call_upload_sends_whole_body_crc32c() {
    let (base, log) = spawn_scripted(vec![(
        200,
        r#"{"result":"yes","response":{"new_file_id":"n"}}"#,
    )])
    .await;
    let data = b"123456789".to_vec();
    upload::single_call_upload("tok", &base, "1", "workspace", "root", "a.txt", data)
        .await
        .expect("upload ok");
    let reqs = requests(&log);
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].multipart_field("hash").as_deref(), Some("e3069283"));
    assert_eq!(
        reqs[0].multipart_field("hash_algo").as_deref(),
        Some("crc32c")
    );
}

#[tokio::test]
async fn single_call_crc32c_mismatch_is_not_retried() {
    let (base, log) = spawn_scripted(vec![MISMATCH, OK]).await;
    let err = upload::single_call_upload(
        "tok",
        &base,
        "1",
        "workspace",
        "root",
        "a.txt",
        b"x".to_vec(),
    )
    .await
    .expect_err("mismatch surfaces");
    assert_eq!(requests(&log).len(), 1);
    assert!(is_crc32c_mismatch(&err), "{err:?}");
}

#[tokio::test]
async fn single_shot_writeback_sends_whole_body_crc32c() {
    let (base, log) = spawn_scripted(vec![(
        200,
        r#"{"result":"yes","session":{"id":"s","status":"assemble"}}"#,
    )])
    .await;
    upload::single_shot_fileshare_writeback(
        "tok",
        &base,
        "1",
        "node",
        "a.txt",
        b"123456789".to_vec(),
        None,
        None,
    )
    .await
    .expect("write-back ok");
    let reqs = requests(&log);
    assert_eq!(reqs[0].multipart_field("hash").as_deref(), Some("e3069283"));
    assert_eq!(
        reqs[0].multipart_field("hash_algo").as_deref(),
        Some("crc32c")
    );
}

#[tokio::test]
async fn stream_upload_defaults_to_body_crc32c() {
    let (base, log) = spawn_scripted(vec![(201, r#"{"result":true}"#)]).await;
    upload::stream_upload(
        "tok",
        &base,
        "sess",
        Bytes::from_static(b"123456789"),
        None,
        None,
    )
    .await
    .expect("stream ok");
    let reqs = requests(&log);
    assert_eq!(reqs[0].query("hash").as_deref(), Some("e3069283"));
    assert_eq!(reqs[0].query("hash_algo").as_deref(), Some("crc32c"));
}

#[tokio::test]
async fn stream_upload_user_hash_wins() {
    let (base, log) = spawn_scripted(vec![(201, r#"{"result":true}"#)]).await;
    upload::stream_upload(
        "tok",
        &base,
        "sess",
        Bytes::from_static(b"123456789"),
        Some("abc123"),
        Some("sha256"),
    )
    .await
    .expect("stream ok");
    let reqs = requests(&log);
    assert_eq!(reqs[0].query("hash").as_deref(), Some("abc123"));
    assert_eq!(reqs[0].query("hash_algo").as_deref(), Some("sha256"));
}

#[tokio::test]
async fn stream_crc32c_mismatch_is_not_retried() {
    let (base, log) = spawn_scripted(vec![MISMATCH, OK]).await;
    let err = upload::stream_upload("tok", &base, "s", Bytes::from_static(b"x"), None, None)
        .await
        .expect_err("mismatch surfaces");
    assert_eq!(requests(&log).len(), 1);
    assert!(is_crc32c_mismatch(&err), "{err:?}");
}

#[tokio::test]
async fn batch_manifest_entries_carry_crc32c() {
    let (base, log) = spawn_scripted(vec![(200, r#"{"result":"yes","response":{}}"#)]).await;
    let data = b"123456789".to_vec();
    let entry_hash = batch_entry_hash(&data);
    let items = vec![BatchUploadItem {
        filename: "a.txt".to_owned(),
        relative_path: None,
        data: Bytes::from(data),
        hash: Some(entry_hash.hash),
        hash_algo: Some(entry_hash.algo.to_owned()),
    }];
    // Only the outbound manifest matters here; the stub response may not parse.
    let _ = upload::upload_batch("tok", &base, "1", None, None, &items).await;
    let reqs = requests(&log);
    assert_eq!(reqs.len(), 1);
    let body = reqs[0].body_text();
    assert!(body.contains(r#""hash":"e3069283""#), "{body}");
    assert!(body.contains(r#""hash_algo":"crc32c""#), "{body}");
}
