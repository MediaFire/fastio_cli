//! Client-side upload integrity checksums.
//!
//! Every upload path attaches a CRC-32C (Castagnoli) checksum that the server
//! verifies against the bytes it receives:
//!
//! - **Chunked uploads** send `hash_algo=crc32c&hash={chunk crc}` with every
//!   chunk, and the whole-file value (the per-chunk CRCs combined in chunk
//!   order) as `file_crc32c` on the chunk that completes the file — the last
//!   chunk, because this client uploads chunks one after another — and on that
//!   same chunk's retries only.
//! - **Whole-body uploads** (single-call, stream, batch entries) send
//!   `hash_algo=crc32c` plus the CRC-32C of the entire body.
//!
//! A whole-file mismatch is terminal: the server stores nothing, the session
//! ends `assembly_failed`, and the request that finalized it fails with error
//! code [`ERR_UPLOAD_CRC32C_MISMATCH`]. The file must be uploaded again in a
//! new session.
//!
//! Wire format: exactly 8 lowercase hex digits in the standard big-endian
//! rendering (`crc32c("123456789") == e3069283`, empty input `00000000`).

use serde_json::Value;

use crate::error::CliError;

/// Which integrity checksum the upload paths attach.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum IntegrityMode {
    /// The behaviour before CRC-32C: no per-chunk hash, no `file_crc32c`,
    /// SHA-256 on batch entries, no hash on single-call bodies, and stream
    /// uploads carry only a caller-supplied hash.
    Legacy,
    /// CRC-32C on every chunk, the combined whole-file CRC-32C on the last
    /// chunk, and a whole-body CRC-32C on single-call, stream and batch uploads.
    Crc32c,
}

/// The integrity mode used by every upload path.
///
/// Internal fallback switch, deliberately not exposed as a user-facing flag:
/// setting it to [`IntegrityMode::Legacy`] restores the previous wire
/// behaviour exactly.
pub const UPLOAD_INTEGRITY: IntegrityMode = IntegrityMode::Crc32c;

/// The `hash_algo` value for CRC-32C.
pub const CRC32C_ALGO: &str = "crc32c";

/// The `hash_algo` value the legacy batch path sends.
const SHA256_ALGO: &str = "sha256";

/// API error code: the uploaded bytes did not match the whole-file CRC-32C,
/// so nothing was stored (HTTP 406). Terminal — never retry the session.
pub const ERR_UPLOAD_CRC32C_MISMATCH: u32 = 10778;

/// API error code: a different whole-file CRC-32C is already set for the
/// session (HTTP 409). Never retried with a different value.
pub const ERR_UPLOAD_CRC32C_CONFLICT: u32 = 10779;

/// CRC-32C (Castagnoli) of `data`, hardware-accelerated where available.
#[must_use]
pub fn crc32c(data: &[u8]) -> u32 {
    ::crc32c::crc32c(data)
}

/// Render a CRC-32C value in the wire format: 8 lowercase hex digits,
/// zero-padded.
#[must_use]
pub fn format_crc32c(crc: u32) -> String {
    format!("{crc:08x}")
}

/// CRC-32C of `data` in the wire format (8 lowercase hex digits).
#[must_use]
pub fn crc32c_hex(data: &[u8]) -> String {
    format_crc32c(crc32c(data))
}

/// Combine two CRC-32C values: given `crc1 = crc32c(A)`, `crc2 = crc32c(B)` and
/// `len2 = len(B)` in bytes, returns `crc32c(A || B)`.
///
/// Same shape as zlib's `crc32_combine` with the CRC-32C polynomial; a
/// `len2` of 0 returns `crc1` unchanged. Lengths beyond the platform's `usize`
/// are handled by folding the length in representable pieces, so any `u64`
/// length is supported on every target.
#[must_use]
pub fn crc32c_combine(crc1: u32, crc2: u32, len2: u64) -> u32 {
    combine_in_pieces(crc1, crc2, len2, max_combine_piece())
}

/// Fold `(crc, len)` pieces left to right, starting from the CRC of empty
/// input. The result is the CRC-32C of the pieces concatenated in order.
#[must_use]
pub fn crc32c_combine_ordered<I>(pieces: I) -> u32
where
    I: IntoIterator<Item = (u32, u64)>,
{
    pieces
        .into_iter()
        .fold(0, |acc, (crc, len)| crc32c_combine(acc, crc, len))
}

/// The largest length the underlying combine accepts in one call.
fn max_combine_piece() -> u64 {
    u64::try_from(usize::MAX).unwrap_or(u64::MAX)
}

/// [`crc32c_combine`] with an explicit per-call length ceiling.
///
/// Combining with a zero `crc2` applies only the length shift to `crc1`, and
/// shifts compose additively, so a long `len2` can be applied as several
/// shorter shifts followed by a final combine that XORs in `crc2`.
fn combine_in_pieces(mut crc1: u32, crc2: u32, mut len2: u64, max_piece: u64) -> u32 {
    if len2 == 0 {
        return crc1;
    }
    let max_piece = max_piece.max(1);
    while len2 > max_piece {
        crc1 = combine_usize(crc1, 0, max_piece);
        len2 -= max_piece;
    }
    combine_usize(crc1, crc2, len2)
}

/// Call the underlying combine with a length already known to fit `usize`.
fn combine_usize(crc1: u32, crc2: u32, len2: u64) -> u32 {
    match usize::try_from(len2) {
        Ok(len) => ::crc32c::crc32c_combine(crc1, crc2, len),
        // Unreachable by construction (callers cap `len2` at `usize::MAX`);
        // split once more rather than truncate the length.
        Err(_) => combine_in_pieces(crc1, crc2, len2, max_combine_piece()),
    }
}

/// Integrity query parameters attached to one chunk request.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChunkIntegrity {
    /// CRC-32C of this chunk's bytes, sent as `hash` with `hash_algo=crc32c`.
    chunk_crc32c: Option<u32>,
    /// Whole-file CRC-32C, sent as `file_crc32c` (last chunk only).
    file_crc32c: Option<u32>,
}

impl ChunkIntegrity {
    /// No integrity parameters (the legacy wire shape).
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }

    /// Per-chunk hash only, for a chunk sent outside a whole-file sequence
    /// (the caller does not know the whole-file value).
    #[must_use]
    pub fn for_chunk(data: &[u8]) -> Self {
        Self::for_chunk_with_mode(UPLOAD_INTEGRITY, data)
    }

    /// [`Self::for_chunk`] under an explicit mode.
    #[must_use]
    pub fn for_chunk_with_mode(mode: IntegrityMode, data: &[u8]) -> Self {
        match mode {
            IntegrityMode::Legacy => Self::none(),
            IntegrityMode::Crc32c => Self {
                chunk_crc32c: Some(crc32c(data)),
                file_crc32c: None,
            },
        }
    }

    /// Integrity parameters for a session uploaded as exactly one chunk: that
    /// chunk is also the last, so it carries the whole-file value too.
    #[must_use]
    pub fn for_only_chunk(data: &[u8]) -> Self {
        SequentialChunkCrc32c::new(u64::try_from(data.len()).unwrap_or(u64::MAX)).next_chunk(data)
    }

    /// The per-chunk CRC-32C, when one is sent.
    #[must_use]
    pub fn chunk_crc32c(&self) -> Option<u32> {
        self.chunk_crc32c
    }

    /// The whole-file CRC-32C, when this chunk carries it.
    #[must_use]
    pub fn file_crc32c(&self) -> Option<u32> {
        self.file_crc32c
    }

    /// The query parameters to append to the chunk URL, in a fixed order:
    /// `hash_algo`, `hash`, then `file_crc32c`. Empty in legacy mode.
    #[must_use]
    pub fn query_pairs(&self) -> Vec<(&'static str, String)> {
        let mut pairs = Vec::with_capacity(3);
        if let Some(crc) = self.chunk_crc32c {
            pairs.push(("hash_algo", CRC32C_ALGO.to_owned()));
            pairs.push(("hash", format_crc32c(crc)));
        }
        if let Some(crc) = self.file_crc32c {
            pairs.push(("file_crc32c", format_crc32c(crc)));
        }
        pairs
    }
}

/// Running CRC-32C state for a file uploaded as sequential chunks.
///
/// Feed every chunk in upload order through [`Self::next_chunk`] before
/// sending it. Each chunk's CRC is folded into the whole-file value; the chunk
/// that brings the byte count up to the planned total is the last one, and only
/// it carries `file_crc32c`. The last chunk's CRC is folded in before its
/// parameters are produced, so the value it carries covers the entire file.
#[derive(Debug, Clone)]
pub struct SequentialChunkCrc32c {
    mode: IntegrityMode,
    total_size: u64,
    bytes_seen: u64,
    file_crc32c: u32,
    carried: bool,
}

impl SequentialChunkCrc32c {
    /// Start tracking a file of `total_size` bytes (the size declared when the
    /// upload session was created).
    #[must_use]
    pub fn new(total_size: u64) -> Self {
        Self::with_mode(UPLOAD_INTEGRITY, total_size)
    }

    /// [`Self::new`] under an explicit mode.
    #[must_use]
    pub fn with_mode(mode: IntegrityMode, total_size: u64) -> Self {
        Self {
            mode,
            total_size,
            bytes_seen: 0,
            file_crc32c: 0,
            carried: false,
        }
    }

    /// Fold the next chunk (in upload order) and return the parameters to send
    /// with it. Call once per chunk; a retry of the same request re-sends the
    /// parameters already returned and must not call this again.
    #[must_use]
    pub fn next_chunk(&mut self, chunk: &[u8]) -> ChunkIntegrity {
        if self.mode == IntegrityMode::Legacy {
            return ChunkIntegrity::none();
        }
        let crc = crc32c(chunk);
        let len = u64::try_from(chunk.len()).unwrap_or(u64::MAX);
        self.file_crc32c = crc32c_combine(self.file_crc32c, crc, len);
        self.bytes_seen = self.bytes_seen.saturating_add(len);
        let is_last = !self.carried && self.bytes_seen >= self.total_size;
        if is_last {
            self.carried = true;
        }
        ChunkIntegrity {
            chunk_crc32c: Some(crc),
            file_crc32c: is_last.then_some(self.file_crc32c),
        }
    }

    /// The CRC-32C of every byte folded so far.
    #[must_use]
    pub fn file_crc32c(&self) -> u32 {
        self.file_crc32c
    }
}

/// A whole-body `(hash, hash_algo)` pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BodyHash {
    /// Hex digest of the body.
    pub hash: String,
    /// Algorithm name sent as `hash_algo`.
    pub algo: &'static str,
}

/// The whole-body hash for a single-call or stream upload, or `None` when the
/// legacy mode sends none.
#[must_use]
pub fn whole_body_hash(data: &[u8]) -> Option<BodyHash> {
    whole_body_hash_with_mode(UPLOAD_INTEGRITY, data)
}

/// [`whole_body_hash`] under an explicit mode.
#[must_use]
pub fn whole_body_hash_with_mode(mode: IntegrityMode, data: &[u8]) -> Option<BodyHash> {
    match mode {
        IntegrityMode::Legacy => None,
        IntegrityMode::Crc32c => Some(BodyHash {
            hash: crc32c_hex(data),
            algo: CRC32C_ALGO,
        }),
    }
}

/// The hash for one batch manifest entry: CRC-32C, or SHA-256 in legacy mode
/// (batch entries always carried a hash).
#[must_use]
pub fn batch_entry_hash(data: &[u8]) -> BodyHash {
    batch_entry_hash_with_mode(UPLOAD_INTEGRITY, data)
}

/// [`batch_entry_hash`] under an explicit mode.
#[must_use]
pub fn batch_entry_hash_with_mode(mode: IntegrityMode, data: &[u8]) -> BodyHash {
    whole_body_hash_with_mode(mode, data).unwrap_or_else(|| BodyHash {
        hash: crate::api::upload::sha256_hex(data),
        algo: SHA256_ALGO,
    })
}

/// Reject a half-supplied caller hash pair for a stream upload.
///
/// `hash` and `hash_algo` describe one checksum, so a caller supplies both or
/// neither (in which case the client computes the CRC-32C itself).
///
/// # Errors
///
/// Returns [`CliError::Parse`] when exactly one of the two is supplied.
pub fn check_stream_hash_pair(hash: Option<&str>, hash_algo: Option<&str>) -> Result<(), CliError> {
    if hash.is_some() != hash_algo.is_some() {
        return Err(CliError::Parse(
            "hash and hash_algo must be supplied together; omit both to send the CRC-32C \
             the client computes"
                .to_owned(),
        ));
    }
    Ok(())
}

/// Resolve the hash pair for a stream upload body.
///
/// A caller-supplied `hash` + `hash_algo` pair always wins and is passed
/// through unchanged. Only when neither is supplied does the client compute
/// the whole-body checksum itself (and only in CRC-32C mode).
///
/// # Errors
///
/// Returns [`CliError::Parse`] when exactly one of the pair is supplied (see
/// [`check_stream_hash_pair`]).
pub fn stream_body_hash(
    data: &[u8],
    hash: Option<&str>,
    hash_algo: Option<&str>,
) -> Result<(Option<String>, Option<String>), CliError> {
    stream_body_hash_with_mode(UPLOAD_INTEGRITY, data, hash, hash_algo)
}

/// [`stream_body_hash`] under an explicit mode.
///
/// # Errors
///
/// Returns [`CliError::Parse`] when exactly one of the pair is supplied.
pub fn stream_body_hash_with_mode(
    mode: IntegrityMode,
    data: &[u8],
    hash: Option<&str>,
    hash_algo: Option<&str>,
) -> Result<(Option<String>, Option<String>), CliError> {
    check_stream_hash_pair(hash, hash_algo)?;
    if hash.is_some() {
        return Ok((hash.map(str::to_owned), hash_algo.map(str::to_owned)));
    }
    Ok(match whole_body_hash_with_mode(mode, data) {
        Some(body) => (Some(body.hash), Some(body.algo.to_owned())),
        None => (None, None),
    })
}

/// `true` when `err` is the terminal whole-file CRC-32C mismatch
/// ([`ERR_UPLOAD_CRC32C_MISMATCH`]): nothing was stored and the session cannot
/// be retried.
#[must_use]
pub fn is_crc32c_mismatch(err: &CliError) -> bool {
    matches!(err, CliError::Api(api) if api.code == ERR_UPLOAD_CRC32C_MISMATCH)
}

/// Describe a whole-file CRC-32C failure recorded on an upload session.
///
/// Accepts either the `GET /upload/{id}/details/` payload (with a `session`
/// object) or the session object itself. Returns `None` unless the session
/// carries an `integrity_failure` object.
#[must_use]
pub fn session_integrity_failure(details: &Value) -> Option<String> {
    let failure = details
        .get("session")
        .and_then(|s| s.get("integrity_failure"))
        .or_else(|| details.get("integrity_failure"))
        .filter(|v| v.is_object())?;
    let expected = failure
        .get("expected_crc32c")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let computed = failure
        .get("computed_crc32c")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    Some(format!(
        "the uploaded bytes did not match the file's CRC-32C checksum \
         (expected {expected}, server computed {computed}), so nothing was stored; \
         upload the file again"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Independent bit-at-a-time reflected CRC-32C (no tables, no hardware).
    fn reference_crc32c(data: &[u8]) -> u32 {
        let mut crc: u32 = 0xFFFF_FFFF;
        for &byte in data {
            crc ^= u32::from(byte);
            for _ in 0..8 {
                crc = if crc & 1 == 1 {
                    (crc >> 1) ^ 0x82F6_3B78
                } else {
                    crc >> 1
                };
            }
        }
        crc ^ 0xFFFF_FFFF
    }

    fn sample(len: usize) -> Vec<u8> {
        (0..len)
            .map(|i| u8::try_from((i * 31 + 7) % 251).unwrap_or(0))
            .collect()
    }

    #[test]
    fn known_check_values() {
        assert_eq!(crc32c_hex(b"123456789"), "e3069283");
        assert_eq!(crc32c_hex(b""), "00000000");
        assert_eq!(format_crc32c(0x0077_2a99), "00772a99");
    }

    #[test]
    fn matches_bitwise_reference() {
        for len in [0, 1, 2, 7, 8, 9, 63, 64, 65, 1000, 4097] {
            let data = sample(len);
            assert_eq!(crc32c(&data), reference_crc32c(&data), "len {len}");
        }
    }

    #[test]
    fn combine_zero_length_returns_crc1() {
        assert_eq!(crc32c_combine(0xdead_beef, 0x1234_5678, 0), 0xdead_beef);
    }

    #[test]
    fn combine_split_into_small_pieces_matches_direct() {
        let a = sample(37);
        let b = sample(1000);
        let direct = crc32c_combine(crc32c(&a), crc32c(&b), 1000);
        assert_eq!(direct, crc32c(&[a.as_slice(), b.as_slice()].concat()));
        for max_piece in [1, 3, 7, 999, 1000, 1001] {
            assert_eq!(
                combine_in_pieces(crc32c(&a), crc32c(&b), 1000, max_piece),
                direct,
                "max_piece {max_piece}"
            );
        }
        // Huge synthetic length: piecewise equals the single call on 64-bit.
        let big = (1u64 << 33) + 12_345;
        assert_eq!(
            combine_in_pieces(0xe306_9283, 0x0000_0000, big, 1u64 << 31),
            crc32c_combine(0xe306_9283, 0x0000_0000, big)
        );
    }

    #[test]
    fn sequential_fold_equals_whole_buffer_crc() {
        // (total length, chunk size): exact multiples, a 1-byte last chunk,
        // a single short chunk, and one chunk exactly the file size.
        for (total, chunk) in [
            (12, 4),
            (13, 4),
            (3, 4),
            (4, 4),
            (1, 1),
            (4096 * 3, 4096),
            (4096 * 3 + 1, 4096),
            (10_000, 333),
        ] {
            let data = sample(total);
            let mut seq = SequentialChunkCrc32c::with_mode(IntegrityMode::Crc32c, total as u64);
            let pieces: Vec<&[u8]> = data.chunks(chunk).collect();
            let mut carriers = Vec::new();
            for (i, piece) in pieces.iter().enumerate() {
                let params = seq.next_chunk(piece);
                assert_eq!(params.chunk_crc32c(), Some(crc32c(piece)));
                if params.file_crc32c().is_some() {
                    carriers.push((i, params.file_crc32c()));
                }
            }
            assert_eq!(
                carriers,
                vec![(pieces.len() - 1, Some(crc32c(&data)))],
                "total {total} chunk {chunk}: only the last chunk carries the whole-file CRC"
            );
            assert_eq!(seq.file_crc32c(), crc32c(&data));
        }
    }

    #[test]
    fn query_pairs_shape() {
        let mut seq = SequentialChunkCrc32c::with_mode(IntegrityMode::Crc32c, 18);
        let first = seq.next_chunk(b"123456789");
        assert_eq!(
            first.query_pairs(),
            vec![
                ("hash_algo", "crc32c".to_owned()),
                ("hash", "e3069283".to_owned())
            ]
        );
        let last = seq.next_chunk(b"123456789");
        let pairs = last.query_pairs();
        assert_eq!(pairs.len(), 3);
        assert_eq!(pairs[2].0, "file_crc32c");
        assert_eq!(pairs[2].1, crc32c_hex(b"123456789123456789"));
        assert_eq!(
            ChunkIntegrity::for_only_chunk(b"123456789").query_pairs(),
            vec![
                ("hash_algo", "crc32c".to_owned()),
                ("hash", "e3069283".to_owned()),
                ("file_crc32c", "e3069283".to_owned()),
            ]
        );
    }

    #[test]
    fn legacy_mode_restores_previous_wire_shape() {
        let mut seq = SequentialChunkCrc32c::with_mode(IntegrityMode::Legacy, 9);
        assert!(seq.next_chunk(b"123456789").query_pairs().is_empty());
        assert!(
            ChunkIntegrity::for_chunk_with_mode(IntegrityMode::Legacy, b"x")
                .query_pairs()
                .is_empty()
        );
        assert_eq!(
            whole_body_hash_with_mode(IntegrityMode::Legacy, b"abc"),
            None
        );
        let batch = batch_entry_hash_with_mode(IntegrityMode::Legacy, b"abc");
        assert_eq!(batch.algo, "sha256");
        assert_eq!(batch.hash, crate::api::upload::sha256_hex(b"abc"));
        assert_eq!(
            stream_body_hash_with_mode(IntegrityMode::Legacy, b"abc", None, None).ok(),
            Some((None, None))
        );
    }

    #[test]
    fn crc32c_mode_body_hashes() {
        let body = whole_body_hash_with_mode(IntegrityMode::Crc32c, b"123456789");
        assert_eq!(
            body,
            Some(BodyHash {
                hash: "e3069283".to_owned(),
                algo: "crc32c"
            })
        );
        let batch = batch_entry_hash_with_mode(IntegrityMode::Crc32c, b"123456789");
        assert_eq!((batch.hash.as_str(), batch.algo), ("e3069283", "crc32c"));
    }

    #[test]
    fn stream_user_values_win() {
        assert_eq!(
            stream_body_hash_with_mode(
                IntegrityMode::Crc32c,
                b"123456789",
                Some("abc"),
                Some("sha256")
            )
            .ok(),
            Some((Some("abc".to_owned()), Some("sha256".to_owned())))
        );
        assert_eq!(
            stream_body_hash_with_mode(IntegrityMode::Crc32c, b"123456789", None, None).ok(),
            Some((Some("e3069283".to_owned()), Some("crc32c".to_owned())))
        );
    }

    #[test]
    fn stream_half_hash_pair_is_rejected() {
        // A partial caller pair is an error, never passed through or completed.
        for mode in [IntegrityMode::Crc32c, IntegrityMode::Legacy] {
            for (hash, algo) in [(Some("abc"), None), (None, Some("sha256"))] {
                let err = stream_body_hash_with_mode(mode, b"x", hash, algo)
                    .expect_err("a half pair must be rejected");
                assert!(
                    err.to_string()
                        .contains("hash and hash_algo must be supplied together"),
                    "{err}"
                );
            }
        }
        assert!(check_stream_hash_pair(None, None).is_ok());
        assert!(check_stream_hash_pair(Some("abc"), Some("md5")).is_ok());
    }

    #[test]
    fn integrity_failure_rendering() {
        let details = json!({
            "session": {
                "status": "assembly_failed",
                "integrity_failure": {
                    "reason": "file_crc32c_mismatch",
                    "expected_crc32c": "7a3c19e4",
                    "computed_crc32c": "1c2f9a07",
                    "error_code": 10778
                }
            }
        });
        let msg = session_integrity_failure(&details).unwrap_or_default();
        assert!(
            msg.contains("7a3c19e4") && msg.contains("1c2f9a07"),
            "{msg}"
        );
        assert!(msg.contains("nothing was stored"), "{msg}");
        // Accepts the bare session object too.
        assert!(session_integrity_failure(&details["session"]).is_some());
        assert_eq!(
            session_integrity_failure(&json!({"session": {"status": "assembly_failed"}})),
            None
        );
    }

    #[test]
    fn mismatch_classifier_keys_on_code() {
        let mismatch = CliError::Api(crate::error::ApiError::new(
            ERR_UPLOAD_CRC32C_MISMATCH,
            None,
            "x".to_owned(),
            406,
        ));
        assert!(is_crc32c_mismatch(&mismatch));
        let other = CliError::Api(crate::error::ApiError::new(1605, None, "x".to_owned(), 406));
        assert!(!is_crc32c_mismatch(&other));
    }
}
