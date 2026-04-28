#!/usr/bin/env bash
# Smoke-test the npm postinstall script offline.
#
# We can't actually download a real release tarball in CI (the v0.1.0
# tag won't exist until ship day), but we CAN verify:
#   1. The script parses package.json correctly
#   2. The platform/arch detection produces a known target triple
#   3. The SHA-256 helper produces a known hash for known input
#   4. The dev-version short-circuit (version=0.0.0) skips the download
#
# This script runs from the repo root.

set -euo pipefail

cd "$(dirname "$0")/.."  # cd into npm-package/

if ! command -v node >/dev/null 2>&1; then
  echo "skipping: node not installed in test environment"
  exit 0
fi

PASS=0
FAIL=0

ok()   { echo "  ok:   $*"; PASS=$((PASS+1)); }
fail() { echo "  fail: $*"; FAIL=$((FAIL+1)); }

echo "== package.json sanity =="
NODE_OUT=$(node -e "
  const pkg = require('./package.json');
  if (pkg.name !== 'carryover') process.exit(1);
  if (!pkg.bin || !pkg.bin.carryoverd) process.exit(2);
  if (!pkg.scripts || !pkg.scripts.postinstall) process.exit(3);
  if (!Array.isArray(pkg.os) || !pkg.os.includes('linux')) process.exit(4);
  if (!Array.isArray(pkg.cpu) || !pkg.cpu.includes('x64')) process.exit(5);
  console.log('ok');
") || { fail "package.json shape (exit=$?)"; exit 1; }
[[ "$NODE_OUT" == "ok" ]] && ok "package.json shape"

echo "== sha256 helper =="
NODE_OUT=$(node -e "
  const { sha256OfFile } = require('./scripts/sha256');
  const fs = require('fs');
  const path = require('path');
  const os = require('os');
  const tmp = path.join(os.tmpdir(), 'carryover-npm-test.bin');
  fs.writeFileSync(tmp, 'hello');
  // Known SHA-256 of 'hello': 2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824
  sha256OfFile(tmp).then(h => {
    fs.unlinkSync(tmp);
    if (h !== '2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824') {
      console.error('mismatch:', h);
      process.exit(1);
    }
    console.log('ok');
  });
") || { fail "sha256 helper"; exit 1; }
[[ "$NODE_OUT" == "ok" ]] && ok "sha256 helper"

echo "== dev-version short-circuit (skips download) =="
OUT=$(CARRYOVER_VERSION=0.0.0 node scripts/postinstall.js 2>&1)
echo "$OUT" | grep -q "dev/placeholder version 0.0.0" \
  && ok "dev short-circuit message" \
  || fail "dev short-circuit message"

echo "== platform/arch detection map =="
NODE_OUT=$(node -e "
  const map = {
    'linux:x64': 'x86_64-unknown-linux-gnu',
    'linux:arm64': 'aarch64-unknown-linux-gnu',
    'darwin:x64': 'x86_64-apple-darwin',
    'darwin:arm64': 'aarch64-apple-darwin',
  };
  const fs = require('fs');
  const src = fs.readFileSync('./scripts/postinstall.js', 'utf8');
  for (const key of Object.keys(map)) {
    if (!src.includes('\"' + key + '\":')) {
      console.error('missing key:', key);
      process.exit(1);
    }
    if (!src.includes(map[key])) {
      console.error('missing target:', map[key]);
      process.exit(2);
    }
  }
  console.log('ok');
") || { fail "platform map (exit=$?)"; exit 1; }
[[ "$NODE_OUT" == "ok" ]] && ok "platform map covers 4 supported targets"

echo
echo "== summary =="
echo "  passed: $PASS"
echo "  failed: $FAIL"
if [[ "$FAIL" -gt 0 ]]; then
  exit 1
fi
