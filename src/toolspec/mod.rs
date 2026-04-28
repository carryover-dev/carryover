//! Per-tool version table — the source of truth for hook event names and
//! transcript file paths across supported tool versions.
//!
//! Hook event names and config paths drift across versions. Without a
//! versioned spec table, every release of every supported tool would
//! silently break Carryover. With this table, the daemon detects which
//! HookSet to use based on the installed tool version, and emits a
//! structured warning event when a tool version is newer than anything
//! in the table (best-effort fallback to the highest-known HookSet).
//!
//! Decision D2 (locked): newer-than-known versions fall back to the
//! highest-known HookSet, emit a warning event, and STILL succeed.
//! Older-than-known versions are refused (broken in unpredictable ways).

use semver::Version;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use thiserror::Error;

pub mod specs;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors produced by ToolSpec operations.
#[derive(Debug, Error)]
pub enum ToolSpecError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("could not parse version `{version}`: {source}")]
    InvalidVersion {
        version: String,
        #[source]
        source: semver::Error,
    },
    #[error("tool `{0}` not found in PATH")]
    BinaryNotFound(String),
    #[error("tool `{name}` version `{version}` is older than the minimum supported `{min}`")]
    OlderThanKnown {
        name: &'static str,
        version: String,
        min: String,
    },
}

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Inclusive lower bound, exclusive upper bound.
/// `min: 1.0.0, max: Some(2.0.0)` matches `>=1.0.0,<2.0.0`.
/// `max: None` is open-ended (highest range in a table).
#[derive(Clone, Debug)]
pub struct VersionRange {
    pub min: Version,
    pub max: Option<Version>,
}

impl VersionRange {
    pub fn contains(&self, v: &Version) -> bool {
        if v < &self.min {
            return false;
        }
        match &self.max {
            Some(max) => v < max,
            None => true,
        }
    }
}

/// Per-version hook event name table. Optional fields handle tools that
/// don't expose every event in every version.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HookSet {
    pub session_start: &'static str,
    pub session_end: &'static str,
    pub pre_compact: Option<&'static str>,
    pub user_prompt_submit: Option<&'static str>,
    pub stop: Option<&'static str>,
}

/// Per-OS list of candidate paths for a config file or transcript directory.
/// Each platform list is checked in order; the first existing path wins.
#[derive(Clone, Copy, Debug)]
pub struct PathSpec {
    pub linux: &'static [&'static str],
    pub macos: &'static [&'static str],
    /// Windows is a v0.4+ target; v0.1 leaves this empty.
    pub windows: &'static [&'static str],
}

impl PathSpec {
    /// Return the candidate list for the current OS.
    pub fn for_current_os(&self) -> &'static [&'static str] {
        if cfg!(target_os = "linux") {
            self.linux
        } else if cfg!(target_os = "macos") {
            self.macos
        } else if cfg!(target_os = "windows") {
            self.windows
        } else {
            &[]
        }
    }

    /// Resolve the first existing candidate against `home_dir`. Each
    /// candidate may contain a leading `~/` which expands to home_dir.
    /// Returns the first absolute path that exists, or None.
    pub fn resolve_first_existing(&self, home_dir: &Path) -> Option<PathBuf> {
        for cand in self.for_current_os() {
            let expanded = expand_tilde(cand, home_dir);
            if expanded.exists() {
                return Some(expanded);
            }
        }
        None
    }
}

/// Result of looking up a HookSet by version.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FallbackKind {
    /// Version matched a range exactly.
    Exact,
    /// Version was higher than the highest range; we used the
    /// highest-known HookSet as a best-effort fallback.
    NewerThanKnown,
    /// Version was lower than the lowest range; older versions
    /// behave unpredictably and Carryover refuses to install.
    OlderThanKnown,
}

/// Static structured warning to be emitted when fallback is non-Exact.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct FallbackEvent {
    pub tool: String,
    pub detected_version: String,
    pub used_hookset_for_version: String,
    pub fallback_kind: FallbackKindSerde,
}

/// Wire-form of FallbackKind for the structured event log.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FallbackKindSerde {
    Exact,
    NewerThanKnown,
    OlderThanKnown,
}

impl From<FallbackKind> for FallbackKindSerde {
    fn from(k: FallbackKind) -> Self {
        match k {
            FallbackKind::Exact => Self::Exact,
            FallbackKind::NewerThanKnown => Self::NewerThanKnown,
            FallbackKind::OlderThanKnown => Self::OlderThanKnown,
        }
    }
}

/// One supported tool's full version-aware spec.
pub struct ToolSpec {
    pub name: &'static str,
    /// Binary names to try (e.g. ["claude", "claude-code"])
    pub detect_binary: &'static [&'static str],
    /// Function that runs the binary with --version and parses the output.
    /// Implementations live in specs.rs.
    pub detect_version: fn(&Path) -> Result<Version, ToolSpecError>,
    pub config_paths: &'static [PathSpec],
    pub transcript_paths: &'static [PathSpec],
    /// Sorted ascending by min version. Highest range typically has
    /// max = None (open-ended).
    pub hooks_by_version: &'static [(VersionRange, HookSet)],
}

impl ToolSpec {
    /// Look up a HookSet for the given version, applying the D2 fallback
    /// semantics. Returns the chosen HookSet plus a FallbackKind so callers
    /// can emit the warning event.
    pub fn resolve_hookset(&self, v: &Version) -> Result<(HookSet, FallbackKind), ToolSpecError> {
        if self.hooks_by_version.is_empty() {
            return Err(ToolSpecError::OlderThanKnown {
                name: self.name,
                version: v.to_string(),
                min: "0.0.0".to_string(),
            });
        }
        // Refuse older-than-lowest.
        let lowest_min = &self.hooks_by_version[0].0.min;
        if v < lowest_min {
            return Err(ToolSpecError::OlderThanKnown {
                name: self.name,
                version: v.to_string(),
                min: lowest_min.to_string(),
            });
        }
        // Iterate; the highest range with max=None acts as the fallback.
        for (range, hookset) in self.hooks_by_version {
            if range.contains(v) {
                return Ok((*hookset, FallbackKind::Exact));
            }
        }
        // Not in any range AND >= lowest min → newer-than-known. Use the
        // last (= highest) hookset as fallback.
        let (_, last_hookset) = self.hooks_by_version.last().unwrap();
        Ok((*last_hookset, FallbackKind::NewerThanKnown))
    }

    /// Build the structured warning event for non-Exact fallbacks.
    pub fn build_fallback_event(
        &self,
        detected: &Version,
        kind: FallbackKind,
    ) -> Option<FallbackEvent> {
        if matches!(kind, FallbackKind::Exact) {
            return None;
        }
        let used_for = self
            .hooks_by_version
            .last()
            .map(|(r, _)| r.min.to_string())
            .unwrap_or_default();
        Some(FallbackEvent {
            tool: self.name.to_string(),
            detected_version: detected.to_string(),
            used_hookset_for_version: used_for,
            fallback_kind: kind.into(),
        })
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn expand_tilde(path: &str, home: &Path) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        home.join(rest)
    } else {
        PathBuf::from(path)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn make_version(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    fn make_range(min: &str, max: Option<&str>) -> VersionRange {
        VersionRange {
            min: make_version(min),
            max: max.map(make_version),
        }
    }

    fn dummy_hookset(label: &'static str) -> HookSet {
        HookSet {
            session_start: label,
            session_end: label,
            pre_compact: None,
            user_prompt_submit: None,
            stop: None,
        }
    }

    #[test]
    fn version_range_contains_inclusive_lower() {
        let r = make_range("1.0.0", Some("2.0.0"));
        assert!(
            r.contains(&make_version("1.0.0")),
            "lower bound is inclusive"
        );
        assert!(!r.contains(&make_version("0.9.9")), "below lower bound");
    }

    #[test]
    fn version_range_contains_exclusive_upper() {
        let r = make_range("1.0.0", Some("2.0.0"));
        assert!(
            !r.contains(&make_version("2.0.0")),
            "upper bound is exclusive"
        );
        assert!(r.contains(&make_version("1.9.9")), "just below upper bound");
    }

    #[test]
    fn version_range_open_upper() {
        let r = make_range("1.0.0", None);
        assert!(r.contains(&make_version("1.0.0")));
        assert!(r.contains(&make_version("999.0.0")));
        assert!(!r.contains(&make_version("0.9.9")));
    }

    #[test]
    fn path_spec_resolves_existing_candidate() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        // Create the second candidate only (tilde-relative to home).
        fs::create_dir_all(home.join("second_dir")).unwrap();

        // Use tilde paths: first doesn't exist, second does.
        let spec = PathSpec {
            linux: &["~/first_missing", "~/second_dir"],
            macos: &["~/first_missing", "~/second_dir"],
            windows: &[],
        };

        let resolved = spec.resolve_first_existing(home).unwrap();
        assert_eq!(resolved, home.join("second_dir"));
    }

    #[test]
    fn path_spec_returns_none_when_all_missing() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();

        let spec = PathSpec {
            linux: &["~/does_not_exist_a", "~/does_not_exist_b"],
            macos: &["~/does_not_exist_a", "~/does_not_exist_b"],
            windows: &[],
        };

        assert!(spec.resolve_first_existing(home).is_none());
    }

    #[test]
    fn expand_tilde_replaces_prefix() {
        let home = Path::new("/tmp/x");
        let result = expand_tilde("~/foo", home);
        assert_eq!(result, PathBuf::from("/tmp/x/foo"));
    }

    #[test]
    fn expand_tilde_passthrough_for_absolute() {
        let home = Path::new("/tmp/x");
        let result = expand_tilde("/abs/path", home);
        assert_eq!(result, PathBuf::from("/abs/path"));
    }

    // ---------------------------------------------------------------------------
    // resolve_hookset tests — uses a local ToolSpec built from scratch.
    // We can't construct ToolSpec directly because detect_version is a fn ptr,
    // so we use the specs::CLAUDE spec and override via a local helper.
    // ---------------------------------------------------------------------------

    fn make_spec_with_table(ranges: &'static [(VersionRange, HookSet)]) -> ToolSpec {
        fn dummy_detect(_p: &Path) -> Result<Version, ToolSpecError> {
            Ok(Version::new(1, 0, 0))
        }
        ToolSpec {
            name: "test-tool",
            detect_binary: &["test-tool"],
            detect_version: dummy_detect,
            config_paths: &[],
            transcript_paths: &[],
            hooks_by_version: ranges,
        }
    }

    // Static tables for tests — must be 'static to satisfy ToolSpec field.
    static RANGE_1_TO_2: std::sync::LazyLock<[(VersionRange, HookSet); 1]> =
        std::sync::LazyLock::new(|| {
            [(
                VersionRange {
                    min: Version::new(1, 0, 0),
                    max: Some(Version::new(2, 0, 0)),
                },
                HookSet {
                    session_start: "start-v1",
                    session_end: "end-v1",
                    pre_compact: None,
                    user_prompt_submit: None,
                    stop: None,
                },
            )]
        });

    // A table with ALL bounded ranges — no open-ended top — so a version above
    // the highest max triggers NewerThanKnown (the fallback path).
    static RANGE_1_TO_2_THEN_2_TO_3: std::sync::LazyLock<[(VersionRange, HookSet); 2]> =
        std::sync::LazyLock::new(|| {
            [
                (
                    VersionRange {
                        min: Version::new(1, 0, 0),
                        max: Some(Version::new(2, 0, 0)),
                    },
                    HookSet {
                        session_start: "start-v1",
                        session_end: "end-v1",
                        pre_compact: None,
                        user_prompt_submit: None,
                        stop: None,
                    },
                ),
                (
                    VersionRange {
                        min: Version::new(2, 0, 0),
                        max: Some(Version::new(3, 0, 0)),
                    },
                    HookSet {
                        session_start: "start-v2",
                        session_end: "end-v2",
                        pre_compact: None,
                        user_prompt_submit: None,
                        stop: None,
                    },
                ),
            ]
        });

    static RANGE_OPEN_FROM_1: std::sync::LazyLock<[(VersionRange, HookSet); 1]> =
        std::sync::LazyLock::new(|| {
            [(
                VersionRange {
                    min: Version::new(1, 0, 0),
                    max: None,
                },
                HookSet {
                    session_start: "start-v1",
                    session_end: "end-v1",
                    pre_compact: None,
                    user_prompt_submit: None,
                    stop: None,
                },
            )]
        });

    #[test]
    fn resolve_hookset_exact_match() {
        // version 1.5.0 in range 1.0.0..2.0.0 → Exact
        let spec = make_spec_with_table(&*RANGE_1_TO_2);
        let v = make_version("1.5.0");
        let (hookset, kind) = spec.resolve_hookset(&v).unwrap();
        assert_eq!(kind, FallbackKind::Exact);
        assert_eq!(hookset.session_start, "start-v1");
    }

    #[test]
    fn resolve_hookset_newer_than_known() {
        // version 999.0.0 with table bounded at 2.0.0..3.0.0 → NewerThanKnown
        // (no open-ended range, so 999.0.0 falls above all ranges)
        let spec = make_spec_with_table(&*RANGE_1_TO_2_THEN_2_TO_3);
        let v = make_version("999.0.0");
        let (hookset, kind) = spec.resolve_hookset(&v).unwrap();
        assert_eq!(kind, FallbackKind::NewerThanKnown);
        // Falls back to last hookset (2.0.0..3.0.0)
        assert_eq!(hookset.session_start, "start-v2");
    }

    #[test]
    fn resolve_hookset_older_than_known() {
        // version 0.5.0 with table starting at 1.0.0 → Err OlderThanKnown
        let spec = make_spec_with_table(&*RANGE_OPEN_FROM_1);
        let v = make_version("0.5.0");
        let result = spec.resolve_hookset(&v);
        assert!(matches!(result, Err(ToolSpecError::OlderThanKnown { .. })));
    }

    #[test]
    fn build_fallback_event_for_exact_returns_none() {
        let spec = make_spec_with_table(&*RANGE_OPEN_FROM_1);
        let v = make_version("1.5.0");
        let event = spec.build_fallback_event(&v, FallbackKind::Exact);
        assert!(event.is_none());
    }

    #[test]
    fn build_fallback_event_for_newer_returns_some() {
        let spec = make_spec_with_table(&*RANGE_OPEN_FROM_1);
        let v = make_version("999.0.0");
        let event = spec.build_fallback_event(&v, FallbackKind::NewerThanKnown);
        let event = event.unwrap();
        assert_eq!(event.tool, "test-tool");
        assert_eq!(event.detected_version, "999.0.0");
        assert_eq!(event.fallback_kind, FallbackKindSerde::NewerThanKnown);
        // Serializable to JSON.
        let json = serde_json::to_string(&event).unwrap();
        assert!(!json.is_empty());
    }

    #[test]
    fn fallback_event_serializes_to_snake_case() {
        let event = FallbackEvent {
            tool: "some-tool".to_string(),
            detected_version: "5.0.0".to_string(),
            used_hookset_for_version: "3.0.0".to_string(),
            fallback_kind: FallbackKindSerde::NewerThanKnown,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(
            json.contains("newer_than_known"),
            "expected snake_case, got: {json}"
        );
        assert!(
            !json.contains("NewerThanKnown"),
            "must not be PascalCase: {json}"
        );
    }

    #[test]
    fn dummy_hookset_coverage() {
        // Exercise the helper used above for clippy/dead-code purposes.
        let h = dummy_hookset("test");
        assert_eq!(h.session_start, "test");
    }
}
