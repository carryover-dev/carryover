//! Test helpers for fixture-driven adapter tests. Each adapter PR will
//! define its own #[test] fns that load fixtures via these helpers and
//! exercise the adapter's parse() + read_new_records() invariants.

use std::path::{Path, PathBuf};

pub fn fixtures_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

pub fn claude_fixture(name: &str) -> PathBuf {
    fixtures_root().join("claude").join(name)
}

pub fn cursor_fixture(name: &str) -> PathBuf {
    fixtures_root().join("cursor").join(name)
}

pub fn codex_fixture(name: &str) -> PathBuf {
    fixtures_root().join("codex").join(name)
}

fn assert_fixture_exists_and_nonempty(path: &Path) {
    // Strip the absolute manifest prefix so failure messages don't leak the
    // CI runner's filesystem layout into public GitHub Actions logs.
    let rel = path
        .strip_prefix(env!("CARGO_MANIFEST_DIR"))
        .unwrap_or(path);
    assert!(path.exists(), "fixture missing: {}", rel.display());
    let meta =
        std::fs::metadata(path).unwrap_or_else(|e| panic!("cannot stat {}: {}", rel.display(), e));
    assert!(meta.len() > 0, "fixture is empty: {}", rel.display());
}

// ---------------------------------------------------------------------------
// Smoke tests — verify fixtures exist and are non-empty.
// Parsing correctness is the responsibility of each adapter PR.
// ---------------------------------------------------------------------------

#[test]
fn claude_fixtures_present() {
    let names = [
        "1-simple-conversation.jsonl",
        "2-parentuuid-chain-deep.jsonl",
        "3-tool-use-and-result.jsonl",
        "4-non-conversation-types.jsonl",
        "5-partial-line-tail.jsonl",
    ];
    for name in &names {
        assert_fixture_exists_and_nonempty(&claude_fixture(name));
    }
}

#[test]
fn cursor_fixtures_present() {
    assert_fixture_exists_and_nonempty(&cursor_fixture("1-state.vscdb"));
}

#[test]
fn codex_fixtures_present() {
    let names = [
        "1-simple-session.jsonl",
        "2-missing-session-meta.jsonl",
        "3-multiple-sessions.jsonl",
        "4-tool-use-event.jsonl",
        "5-partial-line-tail.jsonl",
    ];
    for name in &names {
        assert_fixture_exists_and_nonempty(&codex_fixture(name));
    }
}
