# Homebrew formula for Carryover.
#
# This file is the source of truth for the carryover-dev/homebrew-tap
# repository. Ship-day procedure (see MAC_HANDOVER.md):
#
#   1. Tag v0.1.0 in this repo so the Linux release workflow uploads
#      the Linux binaries to the GitHub Release page.
#   2. On a real Mac, build the macOS binaries (aarch64-apple-darwin
#      and x86_64-apple-darwin), tar.gz them, and attach them to the
#      same Release page.
#   3. Compute the SHA-256 of each macOS tarball.
#   4. Update the `version`, `sha256`, and `url` placeholders below.
#   5. Copy this file to carryover-dev/homebrew-tap:Formula/carryover.rb
#      and open a PR there.
#
# Until step 5 lands, `brew install carryover-dev/tap/carryover` does
# not work. macOS users can install via npm in the meantime — they
# just need the one-time `xattr -d com.apple.quarantine` workaround.

class Carryover < Formula
  desc "Zero-LLM-token context-handoff daemon — resume any AI session across Claude Code, Cursor, and Codex"
  homepage "https://github.com/carryover-dev/carryover"
  license "Apache-2.0"
  version "0.1.2"

  on_macos do
    on_arm do
      url "https://github.com/carryover-dev/carryover/releases/download/v#{version}/carryoverd-v#{version}-aarch64-apple-darwin.tar.gz"
      sha256 "56011ea56d600390864bc53e850efe0670f50fc913d169950c279a1607778baf"
    end
    on_intel do
      url "https://github.com/carryover-dev/carryover/releases/download/v#{version}/carryoverd-v#{version}-x86_64-apple-darwin.tar.gz"
      sha256 "c13b403de1ae1cce944927b8b7a3f486e2e077d163cd47acea07a1efc7420d11"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/carryover-dev/carryover/releases/download/v#{version}/carryoverd-v#{version}-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "9c37926a7d97de5899f1bc5e29abd311ca105705ffc161add18406804d83d149"
    end
    on_intel do
      url "https://github.com/carryover-dev/carryover/releases/download/v#{version}/carryoverd-v#{version}-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "089b28ca195deadc729d518388398c901ecc06211930e3a3d2a2a6f46f352328"
    end
  end

  def install
    bin.install "carryoverd"
  end

  test do
    # Smoke test: --help should print the subcommand list and exit 0.
    output = shell_output("#{bin}/carryoverd --help")
    assert_match "install", output
    assert_match "refresh", output
    assert_match "status", output
    assert_match "uninstall", output
  end

  def caveats
    <<~EOS
      Carryover is signed with cosign (open-source) but NOT with an Apple
      Developer ID. macOS Gatekeeper may still warn on first run; if it
      does, run:

        xattr -d com.apple.quarantine #{bin}/carryoverd

      Apple Developer ID notarization is on the v1.0 roadmap.

      To get started:

        carryoverd install
    EOS
  end
end
