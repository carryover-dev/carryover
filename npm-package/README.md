# carryover (npm)

Zero-LLM-token context-handoff daemon. Resume any AI session across Claude Code, Cursor, and Codex.

```sh
npm install -g carryover
carryoverd install
```

Source repo and full documentation: https://github.com/carryover-dev/carryover

## How `npm install` works for this package

The npm package is a thin shim. The `postinstall` script downloads the matching `carryoverd` binary for your platform from the GitHub Release page that corresponds to the package version, verifies the SHA-256 against the published `SHA256SUMS` manifest, and drops it into the npm bin dir.

Supported platforms in v0.1:

| Platform | Arch | Notes |
|---|---|---|
| Linux | x86_64 | Native install |
| Linux | aarch64 | Native install |
| macOS | x86_64 / aarch64 | Works, but Gatekeeper requires a one-time `xattr` workaround. See the macOS note below. |

## macOS

v0.1 binaries are signed with [cosign](https://github.com/sigstore/cosign) (free, open-source, transparent) but NOT with an Apple Developer ID. macOS Gatekeeper refuses to launch unsigned (by Apple) binaries on first run. Run this once after install:

```sh
xattr -d com.apple.quarantine $(which carryoverd)
```

Apple Developer ID notarization is on the v1.0 roadmap. To skip the `xattr` step, install via Homebrew once the tap is published:

```sh
brew install carryover-dev/tap/carryover
```

## Verification

The postinstall script aborts with a SHA-256-mismatch error if the downloaded tarball's hash doesn't match the published manifest. To verify the cosign signature manually after install, see the verification block in the GitHub release notes for your version.

## License

Apache-2.0. See [LICENSE](https://github.com/carryover-dev/carryover/blob/main/LICENSE).
