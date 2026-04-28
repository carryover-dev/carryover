#!/usr/bin/env node
// Carryover npm postinstall — downloads the carryoverd binary for the
// current platform/arch from the matching GitHub Release, verifies the
// SHA-256 against the published SHA256SUMS manifest, and extracts it
// into the npm bin directory.
//
// Linux x64 + Linux aarch64 are supported in v0.1.
// macOS install via npm is deferred — `brew install carryover-dev/tap/carryover`
// is the recommended path. If a Mac user does install via npm, this script
// downloads the binary anyway and prints the `xattr` workaround.

"use strict";

const fs = require("node:fs");
const path = require("node:path");
const https = require("node:https");
const zlib = require("node:zlib");
const { spawnSync } = require("node:child_process");
const { sha256OfFile } = require("./sha256");

const REPO = "carryover-dev/carryover";
const PKG = require("../package.json");
const VERSION = process.env.CARRYOVER_VERSION || PKG.version;
const TAG = `v${VERSION}`;

const PLATFORM_TARGETS = {
  "linux:x64": "x86_64-unknown-linux-gnu",
  "linux:arm64": "aarch64-unknown-linux-gnu",
  "darwin:x64": "x86_64-apple-darwin",
  "darwin:arm64": "aarch64-apple-darwin",
};

function log(msg) {
  process.stderr.write(`[carryover postinstall] ${msg}\n`);
}

function fail(msg) {
  log(`ERROR: ${msg}`);
  process.exit(1);
}

function macOsAdvisory() {
  log("");
  log("macOS install via npm is supported but Gatekeeper will refuse the");
  log("binary on first run because v0.1 ships unsigned (cosign-only).");
  log("Strip the quarantine flag once after install:");
  log("");
  log("  xattr -d com.apple.quarantine $(which carryoverd)");
  log("");
  log("Apple Developer ID notarization is on the v1.0 roadmap. To skip the");
  log("xattr step, install via Homebrew once the tap lands:");
  log("");
  log("  brew install carryover-dev/tap/carryover");
  log("");
}

function detectTarget() {
  const key = `${process.platform}:${process.arch}`;
  const target = PLATFORM_TARGETS[key];
  if (!target) {
    fail(
      `unsupported platform/arch: ${key}. Supported: ${Object.keys(PLATFORM_TARGETS).join(", ")}.\n` +
        "Carryover v0.1 ships Linux x64, Linux aarch64, and macOS Intel/Apple Silicon.\n" +
        "If you need Windows, please open an issue."
    );
  }
  return target;
}

function downloadTo(url, dest, redirects) {
  redirects = redirects ?? 0;
  if (redirects > 5) {
    return Promise.reject(new Error(`too many redirects: ${url}`));
  }
  return new Promise((resolve, reject) => {
    https
      .get(url, { headers: { "User-Agent": "carryover-npm-postinstall" } }, (res) => {
        if (
          res.statusCode &&
          res.statusCode >= 300 &&
          res.statusCode < 400 &&
          res.headers.location
        ) {
          res.resume();
          return downloadTo(res.headers.location, dest, redirects + 1).then(resolve, reject);
        }
        if (res.statusCode !== 200) {
          res.resume();
          return reject(
            new Error(`HTTP ${res.statusCode} for ${url}`)
          );
        }
        const file = fs.createWriteStream(dest);
        res.pipe(file);
        file.on("finish", () => file.close(resolve));
        file.on("error", (err) => {
          try {
            fs.unlinkSync(dest);
          } catch {}
          reject(err);
        });
      })
      .on("error", reject);
  });
}

async function fetchText(url, redirects) {
  redirects = redirects ?? 0;
  if (redirects > 5) {
    throw new Error(`too many redirects: ${url}`);
  }
  return new Promise((resolve, reject) => {
    https
      .get(url, { headers: { "User-Agent": "carryover-npm-postinstall" } }, (res) => {
        if (
          res.statusCode &&
          res.statusCode >= 300 &&
          res.statusCode < 400 &&
          res.headers.location
        ) {
          res.resume();
          return fetchText(res.headers.location, redirects + 1).then(resolve, reject);
        }
        if (res.statusCode !== 200) {
          res.resume();
          return reject(new Error(`HTTP ${res.statusCode} for ${url}`));
        }
        let body = "";
        res.setEncoding("utf8");
        res.on("data", (chunk) => (body += chunk));
        res.on("end", () => resolve(body));
        res.on("error", reject);
      })
      .on("error", reject);
  });
}

function expectedSha(sumsText, asset) {
  for (const line of sumsText.split(/\r?\n/)) {
    const trimmed = line.trim();
    if (!trimmed) continue;
    const m = trimmed.match(/^([0-9a-fA-F]{64})\s+(.+)$/);
    if (!m) continue;
    const [, sha, name] = m;
    if (path.basename(name) === asset) return sha.toLowerCase();
  }
  return null;
}

async function extractTarGz(archive, destDir) {
  // We avoid pulling in a tar dependency; shell out to `tar` which is
  // present on every Linux + macOS box. If it isn't, the user has bigger
  // problems than this script.
  const result = spawnSync("tar", ["-xzf", archive, "-C", destDir], {
    stdio: "inherit",
  });
  if (result.status !== 0) {
    throw new Error(`tar -xzf failed with status ${result.status}`);
  }
}

async function main() {
  // Skip in dev installs (`npm pack` / `npm link` from the repo) — the
  // postinstall is for end-user installs of a published version.
  if (VERSION === "0.0.0") {
    log("dev/placeholder version 0.0.0 — skipping binary download");
    return;
  }

  const target = detectTarget();
  const asset = `carryoverd-${TAG}-${target}.tar.gz`;
  const releaseUrl = `https://github.com/${REPO}/releases/download/${TAG}/${asset}`;
  const sumsUrl = `https://github.com/${REPO}/releases/download/${TAG}/SHA256SUMS`;

  const binDir = path.join(__dirname, "..", "bin");
  fs.mkdirSync(binDir, { recursive: true });
  const tarballPath = path.join(binDir, asset);

  log(`downloading ${asset}`);
  await downloadTo(releaseUrl, tarballPath);

  log("fetching SHA256SUMS");
  const sumsText = await fetchText(sumsUrl);
  const expected = expectedSha(sumsText, asset);
  if (!expected) {
    fail(
      `no SHA-256 entry for ${asset} in SHA256SUMS — refusing to install an unverified binary.`
    );
  }

  const actual = await sha256OfFile(tarballPath);
  if (actual !== expected) {
    try {
      fs.unlinkSync(tarballPath);
    } catch {}
    fail(
      `SHA-256 mismatch for ${asset}\n  expected: ${expected}\n  got:      ${actual}\nRefusing to install a corrupted or tampered binary.`
    );
  }
  log(`SHA-256 verified (${actual.slice(0, 16)}...)`);

  await extractTarGz(tarballPath, binDir);

  // Tarball produced by release.yml contains `carryoverd` at the top level.
  const binPath = path.join(binDir, "carryoverd");
  if (!fs.existsSync(binPath)) {
    fail(`expected ${binPath} after extraction; tarball layout changed?`);
  }
  fs.chmodSync(binPath, 0o755);

  // Clean up the tarball; the binary is what npm needs.
  try {
    fs.unlinkSync(tarballPath);
  } catch {}

  log(`installed carryoverd ${VERSION} for ${target}`);

  if (process.platform === "darwin") {
    macOsAdvisory();
  }
}

main().catch((err) => {
  log(`postinstall failed: ${err.message || err}`);
  log("");
  log("If this is a transient network issue, re-run `npm install -g carryover`.");
  log("Otherwise, please report the error at:");
  log(`  https://github.com/${REPO}/issues`);
  process.exit(1);
});
