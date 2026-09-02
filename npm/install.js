#!/usr/bin/env node
"use strict";

const { execSync } = require("child_process");
const fs = require("fs");
const https = require("https");
const http = require("http");
const path = require("path");
const os = require("os");

const REPO = "MediaFire/fastio_cli";
const VERSION = require("./package.json").version;

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

function download(url) {
  return new Promise((resolve, reject) => {
    const client = url.startsWith("https") ? https : http;
    client
      .get(url, { headers: { "User-Agent": "fastio-cli-npm" } }, (res) => {
        if (res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) {
          return download(res.headers.location).then(resolve, reject);
        }
        if (res.statusCode !== 200) {
          return reject(new Error(`Download failed: HTTP ${res.statusCode}`));
        }
        const chunks = [];
        res.on("data", (chunk) => chunks.push(chunk));
        res.on("end", () => resolve(Buffer.concat(chunks)));
        res.on("error", reject);
      })
      .on("error", reject);
  });
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
    const data = await download(url);
    fs.writeFileSync(binaryPath, data);
    fs.chmodSync(binaryPath, 0o755);
    console.log(`Installed fastio to ${binaryPath}`);
  } catch (err) {
    console.error(`Failed to download fastio: ${err.message}`);
    console.error(`URL: ${url}`);
    console.error("");
    console.error("You can download manually from:");
    console.error(`  https://github.com/${REPO}/releases`);
    process.exit(1);
  }
}

main();
