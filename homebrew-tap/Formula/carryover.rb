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
  version "0.1.3"

  on_macos do
    on_arm do
      url "https://github.com/carryover-dev/carryover/releases/download/v#{version}/carryoverd-v#{version}-aarch64-apple-darwin.tar.gz"
      sha256 "77cf46c53646bb177788c51705aec40301bad34a725f04f5dd7e94318657d883"
    end
    on_intel do
      url "https://github.com/carryover-dev/carryover/releases/download/v#{version}/carryoverd-v#{version}-x86_64-apple-darwin.tar.gz"
      sha256 "94a590cd9481726e99da74ba573859074d9971ca47bd9b7b4b4fdf6be1d453f4"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/carryover-dev/carryover/releases/download/v#{version}/carryoverd-v#{version}-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "e51ecb93590cd5ca6e706ee0a9d2bd2b3d51307cecd997bf82810682171f7219"
    end
    on_intel do
      url "https://github.com/carryover-dev/carryover/releases/download/v#{version}/carryoverd-v#{version}-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "807e458b0d89a986672d80550aba76bcfebfc7be1f84f6e1b6cb708e438217fd"
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
