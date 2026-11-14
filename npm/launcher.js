#!/usr/bin/env node
// Launcher: runs the native binary shipped by the matching platform package.
// build.mjs copies this script to bin/korg.js of the launcher package.
"use strict";

const { spawn } = require("node:child_process");
const os = require("node:os");
const path = require("node:path");

const NAME = "korg";

// Candidate packages per platform, in order of preference. The musl build is fully static,
// so on Linux (x64 and arm64) it is also a valid fallback when the glibc package was not installed.
const PLATFORMS = {
  "linux-x64": ["@archhaze24/korg-linux-x64", "@archhaze24/korg-linux-x64-musl"],
  "linux-arm64": ["@archhaze24/korg-linux-arm64", "@archhaze24/korg-linux-arm64-musl"],
  "darwin-x64": ["@archhaze24/korg-darwin-x64"],
  "darwin-arm64": ["@archhaze24/korg-darwin-arm64"],
  "win32-x64": ["@archhaze24/korg-win32-x64"],
};

// True on a musl-based Linux (Alpine etc.): glibc reports its version, musl does not.
function isMusl() {
  if (process.platform !== "linux") return false;
  try {
    const report = process.report.getReport();
    const header = typeof report === "string" ? JSON.parse(report).header : report.header;
    if (header && "glibcVersionRuntime" in header) return !header.glibcVersionRuntime;
  } catch {
    // fall through to the file check
  }
  try {
    return require("node:fs").readdirSync("/lib").some((name) => name.startsWith("ld-musl-"));
  } catch {
    return false;
  }
}

function binaryPath() {
  const key = `${process.platform}-${process.arch}`;
  const candidates = PLATFORMS[key];
  if (!candidates) {
    throw new Error(
      `${NAME} has no prebuilt binary for ${key}. Supported: ${Object.keys(PLATFORMS).join(", ")}. ` +
        "Build from source instead: cargo install korg-cli",
    );
  }
  const ordered = key.startsWith("linux-") && isMusl() ? [...candidates].reverse() : candidates;
  const exe = process.platform === "win32" ? `${NAME}.exe` : NAME;
  for (const pkg of ordered) {
    try {
      const manifest = require.resolve(`${pkg}/package.json`);
      return path.join(path.dirname(manifest), "bin", exe);
    } catch {
      // not installed, try the next candidate
    }
  }
  throw new Error(
    `The platform package ${ordered[0]} is not installed (npm skipped the optional dependency). ` +
      "Reinstall without --omit=optional, or run: cargo install korg-cli",
  );
}

let binary;
try {
  binary = binaryPath();
} catch (error) {
  console.error(`${NAME}: ${error.message}`);
  process.exit(1);
}

const child = spawn(binary, process.argv.slice(2), { stdio: "inherit" });

// Forward termination signals so the native process does not outlive the launcher: korg
// restores renamed album folders when it is interrupted, so it must get the chance to run
// its cleanup. Windows only supports SIGINT/SIGBREAK; the others are skipped.
const FORWARDED = process.platform === "win32" ? ["SIGINT", "SIGBREAK"] : ["SIGINT", "SIGTERM", "SIGHUP"];
// On a POSIX terminal Ctrl-C already reaches the child through the tty (same foreground process
// group), so SIGINT is only swallowed here (the handler keeps the launcher alive until the child
// exits) and not forwarded a second time.
const TTY_SIGINT = process.platform !== "win32" && process.stdin.isTTY === true;
for (const signal of FORWARDED) {
  process.on(signal, () => {
    if (signal === "SIGINT" && TTY_SIGINT) return;
    if (child.exitCode === null && child.signalCode === null) child.kill(signal);
  });
}

child.on("error", (error) => {
  console.error(`${NAME}: could not run ${binary}: ${error.message}`);
  process.exit(1);
});

child.on("exit", (code, signal) => {
  if (signal) {
    process.exit(128 + (os.constants.signals[signal] ?? 0));
  }
  process.exit(code ?? 1);
});
