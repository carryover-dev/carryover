# Carryover Homebrew tap (source-of-truth copy)

This directory holds the canonical Homebrew formula for Carryover. The actual tap repo is at:

> **https://github.com/carryover-dev/homebrew-tap**

We keep the formula here too so:

1. The formula and the binary it points at version together — when you tag `v0.1.0`, this file's URLs and SHAs are what go to the tap repo.
2. PRs that change the install UX can edit one place and propagate.
3. Anyone reading this repo can see exactly what `brew install carryover-dev/tap/carryover` will run.

## Ship-day procedure

When tagging a release, follow [`MAC_HANDOVER.md`](../MAC_HANDOVER.md) (lives in `~/workspace/carryover-context/v01-pilot/` — internal). The short version:

1. Tag `v0.1.0` in this repo. The Linux release workflow uploads the Linux binaries to the GitHub Release page.
2. On a real Mac, build the macOS binaries (`aarch64-apple-darwin` and `x86_64-apple-darwin`), tar them, attach to the same Release page.
3. Compute SHA-256 for each tarball:
   ```sh
   shasum -a 256 carryoverd-v0.1.0-*.tar.gz
   ```
4. Update `Formula/carryover.rb` in this directory:
   - bump `version`
   - replace each `sha256 "0000…"` placeholder with the real hash
5. Copy `Formula/carryover.rb` to `carryover-dev/homebrew-tap:Formula/carryover.rb` and open a PR there.
6. Once that PR merges, `brew install carryover-dev/tap/carryover` works.

## Why are the SHAs all zeros right now?

This file ships before any release exists. The placeholders are deliberate — Homebrew refuses to install with a hash mismatch, so a stale `0000…` is the safe default; it forces the ship-day procedure to update the hashes before publishing.

## License

Apache-2.0. Same as the main project.
