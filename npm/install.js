#!/usr/bin/env node
"use strict";

const crypto = require("crypto");
const fs = require("fs");
const https = require("https");
const path = require("path");
const os = require("os");
const { URL } = require("url");

const REPO = "MediaFire/fastio_cli";
const VERSION = require("./package.json").version;

const MAX_REDIRECTS = 10;
// Abort a request that goes this long without receiving data.
const REQUEST_TIMEOUT_MS = 30 * 1000;
// Upper bound on the whole install, across every download. Generous so slow
// links still finish; stalled connections are caught by REQUEST_TIMEOUT_MS.
const OVERALL_TIMEOUT_MS = 20 * 60 * 1000;

const PLATFORM_MAP = {
  "darwin-arm64": "fastio-darwin-arm64",
  "darwin-x64": "fastio-darwin-x64",
  "linux-arm64": "fastio-linux-arm64",
  "linux-x64": "fastio-linux-x64",
  "win32-x64": "fastio-windows-x64.exe",
};

function getPlatformKey() {
  const platform = os.platform();
  const arch = os.arch();
  return `${platform}-${arch}`;
}

function getBinaryName() {
  const key = getPlatformKey();
  const name = PLATFORM_MAP[key];
  if (!name) {
    console.error(`Unsupported platform: ${key}`);
    console.error(`Supported: ${Object.keys(PLATFORM_MAP).join(", ")}`);
    process.exit(1);
  }
  return name;
}

function getInstallDir() {
  return path.dirname(require.resolve("./package.json"));
}

function getBinaryPath() {
  const dir = getInstallDir();
  const isWindows = os.platform() === "win32";
  return path.join(dir, isWindows ? "fastio.exe" : "fastio");
}

/**
 * Parse `sha256sum` output (`<64 hex>  <name>`, or `<64 hex> *<name>` in
 * binary mode) into a Map of file name to lowercase hex digest. Blank lines
 * are ignored; any other line that does not match the format, or a name
 * listed twice with different digests, is an error.
 */
function parseChecksums(text) {
  const sums = new Map();
  const lines = String(text).split("\n");
  for (let i = 0; i < lines.length; i++) {
    const line = lines[i].replace(/\r$/, "");
    if (line.trim() === "") {
      continue;
    }
    const match = /^([0-9a-fA-F]{64}) [ *](.+)$/.exec(line);
    if (!match) {
      throw new Error(`Malformed SHA256SUMS line ${i + 1}`);
    }
    const hash = match[1].toLowerCase();
    const name = match[2];
    if (sums.has(name) && sums.get(name) !== hash) {
      throw new Error(`Conflicting SHA256SUMS entries for ${name}`);
    }
    sums.set(name, hash);
  }
  return sums;
}

/** Return the expected digest for `name`, or throw if it is not listed. */
function expectedHashFor(sums, name) {
  const hash = sums.get(name);
  if (!hash) {
    throw new Error(`No checksum for ${name} in SHA256SUMS`);
  }
  return hash;
}

function sha256Hex(data) {
  return crypto.createHash("sha256").update(data).digest("hex");
}

/**
 * Throw unless `data` hashes to the digest SHA256SUMS lists for `name`.
 */
function verifyChecksum(data, checksumsText, name) {
  const expected = expectedHashFor(parseChecksums(checksumsText), name);
  const actual = sha256Hex(data);
  if (actual !== expected) {
    throw new Error(
      `Checksum mismatch for ${name}: expected ${expected}, got ${actual}`
    );
  }
}

/**
 * Resolve a redirect `Location` against the URL that returned it. Only
 * https targets are followed.
 */
function resolveRedirect(fromUrl, location) {
  const next = new URL(location, fromUrl);
  if (next.protocol !== "https:") {
    throw new Error(`Refusing to follow redirect to non-https URL: ${next.href}`);
  }
  return next.href;
}

function download(url, redirectsLeft = MAX_REDIRECTS) {
  return new Promise((resolve, reject) => {
    if (new URL(url).protocol !== "https:") {
      reject(new Error(`Refusing to download from non-https URL: ${url}`));
      return;
    }
    const req = https.get(
      url,
      { headers: { "User-Agent": "fastio-cli-npm" } },
      (res) => {
        if (res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) {
          res.resume();
          if (redirectsLeft <= 0) {
            reject(new Error("Download failed: too many redirects"));
            return;
          }
          let next;
          try {
            next = resolveRedirect(url, res.headers.location);
          } catch (err) {
            reject(err);
            return;
          }
          download(next, redirectsLeft - 1).then(resolve, reject);
          return;
        }
        if (res.statusCode !== 200) {
          res.resume();
          reject(new Error(`Download failed: HTTP ${res.statusCode}`));
          return;
        }
        const chunks = [];
        res.on("data", (chunk) => chunks.push(chunk));
        res.on("end", () => resolve(Buffer.concat(chunks)));
        res.on("error", reject);
      }
    );
    req.setTimeout(REQUEST_TIMEOUT_MS, () => {
      req.destroy(new Error("Download failed: request timed out"));
    });
    req.on("error", reject);
  });
}

function withTimeout(promise, ms, message) {
  let timer;
  const timeout = new Promise((_, reject) => {
    timer = setTimeout(() => reject(new Error(message)), ms);
  });
  return Promise.race([promise, timeout]).then(
    (value) => {
      clearTimeout(timer);
      return value;
    },
    (err) => {
      clearTimeout(timer);
      throw err;
    }
  );
}

async function install(binaryName, binaryPath) {
  const base = `https://github.com/${REPO}/releases/download/v${VERSION}`;
  const checksums = (await download(`${base}/SHA256SUMS`)).toString("utf8");
  // Fail on a missing or malformed entry before fetching the binary.
  expectedHashFor(parseChecksums(checksums), binaryName);
  const data = await download(`${base}/${binaryName}`);
  verifyChecksum(data, checksums, binaryName);
  fs.writeFileSync(binaryPath, data);
  fs.chmodSync(binaryPath, 0o755);
}

async function main() {
  const binaryName = getBinaryName();
  const binaryPath = getBinaryPath();

  if (fs.existsSync(binaryPath)) {
    return;
  }

  const url = `https://github.com/${REPO}/releases/download/v${VERSION}/${binaryName}`;
  console.log(`Downloading fastio v${VERSION} for ${getPlatformKey()}...`);

  try {
    await withTimeout(
      install(binaryName, binaryPath),
      OVERALL_TIMEOUT_MS,
      "Download failed: timed out"
    );
    console.log(`Installed fastio to ${binaryPath}`);
  } catch (err) {
    try {
      fs.unlinkSync(binaryPath);
    } catch (_) {
      // Nothing was written.
    }
    console.error(`Failed to install fastio: ${err.message}`);
    console.error(`URL: ${url}`);
    console.error("");
    console.error("You can download manually from:");
    console.error(`  https://github.com/${REPO}/releases`);
    process.exit(1);
  }
}

if (require.main === module) {
  main();
}

module.exports = {
  MAX_REDIRECTS,
  parseChecksums,
  expectedHashFor,
  sha256Hex,
  verifyChecksum,
  resolveRedirect,
};
