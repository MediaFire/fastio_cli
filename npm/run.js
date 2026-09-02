#!/usr/bin/env node
"use strict";

const { execFileSync } = require("child_process");
const path = require("path");
const os = require("os");

const dir = path.dirname(require.resolve("./package.json"));
const isWindows = os.platform() === "win32";
const binary = path.join(dir, isWindows ? "fastio.exe" : "fastio");

try {
  execFileSync(binary, process.argv.slice(2), { stdio: "inherit" });
} catch (err) {
  if (err.status !== undefined) {
    process.exit(err.status);
  }
  console.error(`Failed to run fastio: ${err.message}`);
  console.error("Try reinstalling: npm install -g @vividengine/fastio-cli");
  process.exit(1);
}
