//! Concrete v0.1 ToolSpec entries for Claude Code, Cursor, and Codex.
//!
//! Each entry defines:
//! - Binary names to try for version detection
//! - OS-aware config file and transcript directory candidates
//! - A versioned HookSet table (sorted ascending by min version)
//!
//! The `detect_version_*` functions run `<binary> --version` and extract the
//! first `\d+\.\d+\.\d+` substring from the output using a tiny char-by-char
//! scanner (no regex crate dependency). If the binary is missing or the output
//! contains no semver-shaped string, a structured `ToolSpecError` is returned.

use std::path::Path;
use std::process::Command;

use semver::Version;

use crate::toolspec::{HookSet, PathSpec, ToolSpec, ToolSpecError, VersionRange};

// ---------------------------------------------------------------------------
// Shared version-detection helper
// ---------------------------------------------------------------------------

/// Scan `output` bytes for the first substring matching `\d+\.\d+\.\d+` and
/// parse it as a semver `Version`. Returns `ToolSpecError::InvalidVersion` if
/// no such substring is found or if parsing fails.
///
/// This intentionally avoids the `regex` crate: the scanner is O(n) with
/// constant overhead and covers all realistic `--version` output shapes.
pub fn parse_semver_from_output(output: &[u8]) -> Result<Version, ToolSpecError> {
    let text = String::from_utf8_lossy(output);
    // Walk through the string looking for a digit-dot-digit-dot-digit run.
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    let mut i = 0;

    while i < len {
        // Must start with a digit.
        if !chars[i].is_ascii_digit() {
            i += 1;
            continue;
        }
        // Collect the first numeric component.
        let start = i;
        while i < len && chars[i].is_ascii_digit() {
            i += 1;
        }
        if i >= len || chars[i] != '.' {
            // Not a version start.
            continue;
        }
        i += 1; // skip '.'
                // Collect the second numeric component.
        if i >= len || !chars[i].is_ascii_digit() {
            continue;
        }
        while i < len && chars[i].is_ascii_digit() {
            i += 1;
        }
        if i >= len || chars[i] != '.' {
            continue;
        }
        i += 1; // skip '.'
                // Collect the third numeric component.
        if i >= len || !chars[i].is_ascii_digit() {
            continue;
        }
        let patch_start = i;
        while i < len && chars[i].is_ascii_digit() {
            i += 1;
        }
        // Make sure we got at least one patch digit.
        if i == patch_start {
            continue;
        }
        // Extract the candidate substring from the original text.
        let candidate: String = chars[start..i].iter().collect();
        return Version::parse(&candidate).map_err(|source| ToolSpecError::InvalidVersion {
            version: candidate,
            source,
        });
    }

    Err(ToolSpecError::InvalidVersion {
        version: String::from_utf8_lossy(output)
            .chars()
            .take(80)
            .collect::<String>(),
        source: Version::parse("not-a-version").unwrap_err(),
    })
}

/// Run `binary --version`, capture stdout+stderr, and extract the first semver
/// substring. Returns `ToolSpecError::BinaryNotFound` when the binary cannot
/// be executed (OS error kind `NotFound`).
fn run_version_cmd(binary: &Path) -> Result<Version, ToolSpecError> {
    let output = Command::new(binary)
        .arg("--version")
        .output()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                ToolSpecError::BinaryNotFound(binary.display().to_string())
            } else {
                ToolSpecError::Io(e)
            }
        })?;

    // Try stdout first, fall back to stderr (some tools write version there).
    if let Ok(v) = parse_semver_from_output(&output.stdout) {
        return Ok(v);
    }
    parse_semver_from_output(&output.stderr)
}

// ---------------------------------------------------------------------------
// Claude Code
// ---------------------------------------------------------------------------

fn detect_version_claude(binary: &Path) -> Result<Version, ToolSpecError> {
    run_version_cmd(binary)
}

static CLAUDE_CONFIG_PATH: PathSpec = PathSpec {
    linux: &["~/.claude/settings.json"],
    macos: &["~/.claude/settings.json"],
    windows: &[],
};

static CLAUDE_TRANSCRIPT_PATH: PathSpec = PathSpec {
    linux: &["~/.claude/projects/"],
    macos: &["~/.claude/projects/"],
    windows: &[],
};

static CLAUDE_HOOKSET_V1: HookSet = HookSet {
    session_start: "SessionStart",
    session_end: "Stop",
    pre_compact: Some("PreCompact"),
    user_prompt_submit: Some("UserPromptSubmit"),
    stop: Some("Stop"),
};

static CLAUDE_HOOKS: [(VersionRange, HookSet); 1] = [(
    VersionRange {
        min: Version::new(1, 0, 0),
        max: None,
    },
    CLAUDE_HOOKSET_V1,
)];

/// ToolSpec for Claude Code (`claude` binary, v1.0.0+).
pub static CLAUDE: ToolSpec = ToolSpec {
    name: "claude",
    detect_binary: &["claude", "claude-code"],
    detect_version: detect_version_claude,
    config_paths: &[CLAUDE_CONFIG_PATH],
    transcript_paths: &[CLAUDE_TRANSCRIPT_PATH],
    hooks_by_version: &CLAUDE_HOOKS,
};

// ---------------------------------------------------------------------------
// Cursor
// ---------------------------------------------------------------------------

fn detect_version_cursor(binary: &Path) -> Result<Version, ToolSpecError> {
    run_version_cmd(binary)
}

static CURSOR_CONFIG_PATH: PathSpec = PathSpec {
    linux: &["~/.cursor/hooks.json"],
    macos: &["~/.cursor/hooks.json"],
    windows: &[],
};

static CURSOR_TRANSCRIPT_PATH: PathSpec = PathSpec {
    linux: &["~/.config/Cursor/User/globalStorage/state.vscdb"],
    macos: &["~/Library/Application Support/Cursor/User/globalStorage/state.vscdb"],
    windows: &[],
};

/// Workspace-storage directory — where Cursor writes per-session prompt DBs.
/// Watching this directory triggers re-ingestion whenever a prompt is saved.
static CURSOR_WORKSPACE_STORAGE_PATH: PathSpec = PathSpec {
    linux: &["~/.config/Cursor/User/workspaceStorage"],
    macos: &["~/Library/Application Support/Cursor/User/workspaceStorage"],
    windows: &[],
};

static CURSOR_HOOKSET_V040: HookSet = HookSet {
    session_start: "beforeSubmitPrompt",
    session_end: "stop",
    pre_compact: None,
    user_prompt_submit: None,
    stop: Some("stop"),
};

static CURSOR_HOOKS: [(VersionRange, HookSet); 1] = [(
    VersionRange {
        min: Version::new(0, 40, 0),
        max: None,
    },
    CURSOR_HOOKSET_V040,
)];

/// ToolSpec for Cursor (`cursor` binary, v0.40.0+).
pub static CURSOR: ToolSpec = ToolSpec {
    name: "cursor",
    detect_binary: &["cursor"],
    detect_version: detect_version_cursor,
    config_paths: &[CURSOR_CONFIG_PATH],
    transcript_paths: &[CURSOR_TRANSCRIPT_PATH, CURSOR_WORKSPACE_STORAGE_PATH],
    hooks_by_version: &CURSOR_HOOKS,
};

// ---------------------------------------------------------------------------
// Codex
// ---------------------------------------------------------------------------

fn detect_version_codex(binary: &Path) -> Result<Version, ToolSpecError> {
    run_version_cmd(binary)
}

static CODEX_CONFIG_PATH: PathSpec = PathSpec {
    linux: &["~/.codex/config.toml"],
    macos: &["~/.codex/config.toml"],
    windows: &[],
};

static CODEX_TRANSCRIPT_PATH: PathSpec = PathSpec {
    linux: &["~/.codex/sessions/"],
    macos: &["~/.codex/sessions/"],
    windows: &[],
};

static CODEX_HOOKSET_V020: HookSet = HookSet {
    session_start: "SessionStart",
    session_end: "Stop",
    pre_compact: None,
    user_prompt_submit: None,
    stop: Some("Stop"),
};

static CODEX_HOOKS: [(VersionRange, HookSet); 1] = [(
    VersionRange {
        min: Version::new(0, 20, 0),
        max: None,
    },
    CODEX_HOOKSET_V020,
)];

/// ToolSpec for Codex CLI (`codex` binary, v0.20.0+).
pub static CODEX: ToolSpec = ToolSpec {
    name: "codex",
    detect_binary: &["codex"],
    detect_version: detect_version_codex,
    config_paths: &[CODEX_CONFIG_PATH],
    transcript_paths: &[CODEX_TRANSCRIPT_PATH],
    hooks_by_version: &CODEX_HOOKS,
};

// ---------------------------------------------------------------------------
// Master table
// ---------------------------------------------------------------------------

/// All v0.1 tool specs in one slice. Used by the install CLI to iterate.
pub static ALL_TOOLS: &[&ToolSpec] = &[&CLAUDE, &CURSOR, &CODEX];

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_spec_resolves_known_version() {
        let v = Version::parse("2.0.0").unwrap();
        let (hookset, kind) = CLAUDE.resolve_hookset(&v).unwrap();
        assert_eq!(kind, crate::toolspec::FallbackKind::Exact);
        assert_eq!(hookset.session_start, "SessionStart");
        assert_eq!(hookset.pre_compact, Some("PreCompact"));
    }

    #[test]
    fn cursor_spec_resolves_known_version() {
        let v = Version::parse("0.50.0").unwrap();
        let (hookset, kind) = CURSOR.resolve_hookset(&v).unwrap();
        assert_eq!(kind, crate::toolspec::FallbackKind::Exact);
        assert_eq!(hookset.session_start, "beforeSubmitPrompt");
        assert!(hookset.pre_compact.is_none());
    }

    #[test]
    fn codex_spec_resolves_known_version() {
        let v = Version::parse("0.30.0").unwrap();
        let (hookset, kind) = CODEX.resolve_hookset(&v).unwrap();
        assert_eq!(kind, crate::toolspec::FallbackKind::Exact);
        assert_eq!(hookset.session_start, "SessionStart");
        assert_eq!(hookset.stop, Some("Stop"));
    }

    #[test]
    fn parse_semver_from_output_finds_version() {
        let input = b"claude version 1.2.3 (build abc)";
        let v = parse_semver_from_output(input).unwrap();
        assert_eq!(v, Version::new(1, 2, 3));
    }

    #[test]
    fn parse_semver_from_output_handles_malformed() {
        let input = b"no version here";
        let result = parse_semver_from_output(input);
        assert!(matches!(result, Err(ToolSpecError::InvalidVersion { .. })));
    }

    #[test]
    fn all_tools_table_lists_three_tools() {
        assert_eq!(ALL_TOOLS.len(), 3);
    }

    #[test]
    fn each_tool_has_at_least_one_hookset() {
        for spec in ALL_TOOLS {
            assert!(
                !spec.hooks_by_version.is_empty(),
                "tool `{}` has no hookset entries",
                spec.name
            );
        }
    }
}
