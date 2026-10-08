//! Shared CRC-32C vectors for the upload integrity contract.
//!
//! `fixtures/crc32c-vectors.json` is the language-neutral vector file every
//! client implementation is checked against. Each section is exercised through
//! the public helpers, and every case whose bytes are available is also checked
//! against an independent oracle: a bit-at-a-time reflected CRC-32C computed in
//! this file over the concatenated bytes, sharing no code with the helpers.

use fastio_cli::api::upload_integrity::{
    crc32c, crc32c_combine, crc32c_combine_ordered, crc32c_hex, format_crc32c,
};
use serde_json::Value;

const VECTORS: &str = include_str!("fixtures/crc32c-vectors.json");

fn vectors() -> Value {
    serde_json::from_str(VECTORS).expect("vector file parses")
}

/// Bit-at-a-time reflected CRC-32C (init and xorout `0xFFFFFFFF`, poly
/// `0x82F63B78`). Deliberately table-free and hardware-free.
fn oracle(data: &[u8]) -> u32 {
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

fn hex_bytes(hex: &str) -> Vec<u8> {
    assert!(hex.len().is_multiple_of(2), "odd-length hex: {hex}");
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("valid hex byte"))
        .collect()
}

fn crc_field(case: &Value, key: &str) -> u32 {
    let text = case[key].as_str().expect("crc field is a string");
    assert_eq!(text.len(), 8, "{key} must be 8 hex digits: {text}");
    assert_eq!(text, text.to_ascii_lowercase(), "{key} must be lowercase");
    u32::from_str_radix(text, 16).expect("crc parses as hex")
}

fn len_field(case: &Value, key: &str) -> u64 {
    case[key]
        .as_u64()
        .unwrap_or_else(|| panic!("{key} is a non-negative 64-bit length: {}", case[key]))
}

fn section<'a>(all: &'a Value, name: &str) -> &'a Vec<Value> {
    let cases = all[name]
        .as_array()
        .unwrap_or_else(|| panic!("section {name} present"));
    assert!(!cases.is_empty(), "section {name} is empty");
    cases
}

fn name(case: &Value) -> String {
    case["name"].as_str().unwrap_or("(unnamed)").to_owned()
}

#[test]
fn compute_vectors_match_helper_and_oracle() {
    let all = vectors();
    for case in section(&all, "compute") {
        let data = hex_bytes(case["data_hex"].as_str().expect("data_hex"));
        assert_eq!(
            data.len() as u64,
            len_field(case, "length"),
            "{}",
            name(case)
        );
        let expected = case["crc32c"].as_str().expect("crc32c");
        assert_eq!(crc32c_hex(&data), expected, "helper: {}", name(case));
        assert_eq!(
            format_crc32c(oracle(&data)),
            expected,
            "oracle: {}",
            name(case)
        );
    }
}

#[test]
fn combine_vectors_match_helper_and_concatenation_oracle() {
    let all = vectors();
    for case in section(&all, "combine") {
        let a = hex_bytes(case["a_hex"].as_str().expect("a_hex"));
        let b = hex_bytes(case["b_hex"].as_str().expect("b_hex"));
        let crc1 = crc_field(case, "crc1");
        let crc2 = crc_field(case, "crc2");
        let len2 = len_field(case, "len2");
        let expected = crc_field(case, "expected");
        // The inputs are self-consistent with their bytes.
        assert_eq!(crc32c(&a), crc1, "crc1: {}", name(case));
        assert_eq!(crc32c(&b), crc2, "crc2: {}", name(case));
        assert_eq!(b.len() as u64, len2, "len2: {}", name(case));
        assert_eq!(crc32c_combine(crc1, crc2, len2), expected, "{}", name(case));
        let joined = [a.as_slice(), b.as_slice()].concat();
        assert_eq!(oracle(&joined), expected, "oracle: {}", name(case));
    }
}

#[test]
fn combine_ordered_vectors_match_helper_and_concatenation_oracle() {
    let all = vectors();
    for case in section(&all, "combine_ordered") {
        let pieces = case["pieces"].as_array().expect("pieces");
        let mut joined = Vec::new();
        let mut folded = Vec::new();
        for piece in pieces {
            let data = hex_bytes(piece["data_hex"].as_str().expect("data_hex"));
            let crc = crc_field(piece, "crc32c");
            let size = len_field(piece, "size");
            assert_eq!(crc32c(&data), crc, "piece crc: {}", name(case));
            assert_eq!(data.len() as u64, size, "piece size: {}", name(case));
            joined.extend_from_slice(&data);
            folded.push((crc, size));
        }
        let expected = crc_field(case, "expected");
        assert_eq!(crc32c_combine_ordered(folded), expected, "{}", name(case));
        assert_eq!(oracle(&joined), expected, "oracle: {}", name(case));
    }
}

#[test]
fn synthetic_lengths_beyond_32_bits() {
    let all = vectors();
    for case in section(&all, "combine_synthetic") {
        let got = crc32c_combine(
            crc_field(case, "crc1"),
            crc_field(case, "crc2"),
            len_field(case, "len2"),
        );
        assert_eq!(got, crc_field(case, "expected"), "{}", name(case));
    }
}

#[test]
fn synthetic_associativity() {
    let all = vectors();
    for (i, case) in section(&all, "associativity_synthetic").iter().enumerate() {
        let (c1, c2, c3) = (
            crc_field(case, "crc1"),
            crc_field(case, "crc2"),
            crc_field(case, "crc3"),
        );
        let (l2, l3) = (len_field(case, "len2"), len_field(case, "len3"));
        let expected = crc_field(case, "expected");
        let left = crc32c_combine(crc32c_combine(c1, c2, l2), c3, l3);
        let right = crc32c_combine(c1, crc32c_combine(c2, c3, l3), l2 + l3);
        assert_eq!(left, expected, "left fold, case {i}");
        assert_eq!(right, expected, "right fold, case {i}");
    }
}

/// The contract says a non-positive `len2` returns `crc1`. The helper takes an
/// unsigned length, so the only representable case is zero.
#[test]
fn zero_len2_returns_crc1() {
    for crc1 in [0, 1, 0xe306_9283, u32::MAX] {
        assert_eq!(crc32c_combine(crc1, 0x1234_5678, 0), crc1);
    }
}

/// Chunked folds equal the CRC of the whole buffer across chunk-size/length
/// combinations, including exact multiples and a 1-byte last chunk.
#[test]
fn in_order_chunk_fold_equals_whole_buffer() {
    let data: Vec<u8> = (0u32..20_000)
        .map(|i| u8::try_from(i.wrapping_mul(2_654_435_761) >> 24).unwrap_or(0))
        .collect();
    for (len, chunk) in [
        (20_000, 4_000),
        (20_000, 1_000),
        (19_999, 1_000),
        (16_001, 4_000),
        (4_096, 4_096),
        (1, 4_096),
        (0, 4_096),
        (7_777, 333),
    ] {
        let slice = &data[..len];
        let folded = crc32c_combine_ordered(
            slice
                .chunks(chunk)
                .map(|c| (crc32c(c), c.len() as u64))
                .collect::<Vec<_>>(),
        );
        assert_eq!(folded, crc32c(slice), "len {len} chunk {chunk}");
        assert_eq!(folded, oracle(slice), "oracle len {len} chunk {chunk}");
    }
}
