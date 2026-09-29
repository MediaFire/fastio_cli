"use strict";

const test = require("node:test");
const assert = require("node:assert");
const crypto = require("crypto");

const {
  parseChecksums,
  expectedHashFor,
  sha256Hex,
  verifyChecksum,
  resolveRedirect,
} = require("../install.js");

const BINARY = Buffer.from("example binary contents");
const BINARY_HASH = crypto.createHash("sha256").update(BINARY).digest("hex");
const OTHER_HASH = "0".repeat(64);

function sums(lines) {
  return lines.join("\n") + "\n";
}

test("parseChecksums reads sha256sum text and binary-mode lines", () => {
  const parsed = parseChecksums(
    sums([
      `${BINARY_HASH}  fastio-linux-x64`,
      `${OTHER_HASH.toUpperCase()} *fastio-windows-x64.exe`,
    ])
  );
  assert.strictEqual(parsed.get("fastio-linux-x64"), BINARY_HASH);
  assert.strictEqual(parsed.get("fastio-windows-x64.exe"), OTHER_HASH);
});

test("parseChecksums tolerates CRLF and blank lines", () => {
  const parsed = parseChecksums(`\r\n${BINARY_HASH}  fastio-darwin-arm64\r\n\r\n`);
  assert.strictEqual(parsed.get("fastio-darwin-arm64"), BINARY_HASH);
  assert.strictEqual(parsed.size, 1);
});

test("parseChecksums rejects malformed lines", () => {
  assert.throws(() => parseChecksums("not a checksum line\n"), /Malformed SHA256SUMS line 1/);
  assert.throws(
    () => parseChecksums(sums([`${BINARY_HASH}  fastio-linux-x64`, "abc123  fastio-linux-arm64"])),
    /Malformed SHA256SUMS line 2/
  );
  assert.throws(() => parseChecksums(`${BINARY_HASH}fastio-linux-x64\n`), /Malformed/);
});

test("parseChecksums rejects conflicting duplicate entries", () => {
  assert.throws(
    () => parseChecksums(sums([`${BINARY_HASH}  fastio-linux-x64`, `${OTHER_HASH}  fastio-linux-x64`])),
    /Conflicting/
  );
});

test("expectedHashFor throws when the binary is not listed", () => {
  const parsed = parseChecksums(sums([`${BINARY_HASH}  fastio-linux-x64`]));
  assert.strictEqual(expectedHashFor(parsed, "fastio-linux-x64"), BINARY_HASH);
  assert.throws(() => expectedHashFor(parsed, "fastio-linux-arm64"), /No checksum for fastio-linux-arm64/);
});

test("sha256Hex matches the crypto digest", () => {
  assert.strictEqual(sha256Hex(BINARY), BINARY_HASH);
});

test("verifyChecksum accepts a matching binary", () => {
  assert.doesNotThrow(() =>
    verifyChecksum(BINARY, sums([`${BINARY_HASH}  fastio-linux-x64`]), "fastio-linux-x64")
  );
});

test("verifyChecksum rejects a mismatched binary", () => {
  assert.throws(
    () => verifyChecksum(Buffer.from("tampered"), sums([`${BINARY_HASH}  fastio-linux-x64`]), "fastio-linux-x64"),
    /Checksum mismatch for fastio-linux-x64/
  );
  assert.throws(
    () => verifyChecksum(BINARY, sums([`${OTHER_HASH}  fastio-linux-x64`]), "fastio-linux-x64"),
    /Checksum mismatch/
  );
});

test("verifyChecksum rejects a binary missing from SHA256SUMS", () => {
  assert.throws(
    () => verifyChecksum(BINARY, sums([`${BINARY_HASH}  fastio-darwin-x64`]), "fastio-linux-x64"),
    /No checksum for fastio-linux-x64/
  );
  assert.throws(() => verifyChecksum(BINARY, "", "fastio-linux-x64"), /No checksum/);
});

test("resolveRedirect follows absolute and relative https locations", () => {
  const from = "https://github.com/owner/repo/releases/download/v1.0.0/fastio-linux-x64";
  assert.strictEqual(
    resolveRedirect(from, "https://objects.example.com/asset?sig=1"),
    "https://objects.example.com/asset?sig=1"
  );
  assert.strictEqual(
    resolveRedirect(from, "/owner/repo/other"),
    "https://github.com/owner/repo/other"
  );
});

test("resolveRedirect refuses non-https locations", () => {
  const from = "https://github.com/owner/repo/releases/download/v1.0.0/fastio-linux-x64";
  assert.throws(() => resolveRedirect(from, "http://objects.example.com/asset"), /non-https/);
  assert.throws(() => resolveRedirect(from, "ftp://objects.example.com/asset"), /non-https/);
  assert.throws(() => resolveRedirect(from, "file:///etc/passwd"), /non-https/);
});
