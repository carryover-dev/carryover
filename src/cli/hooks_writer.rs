//! Writes Carryover hook stubs into each tool's native settings file.
//! Idempotent: if the stub for a (tool, event) pair already matches our
//! canonical curl line, the file is left alone.
//!
//! Shapes:
//! - Claude Code: ~/.claude/settings.json  — JSON, hooks under "hooks.<EventName>"
//! - Cursor:      ~/.cursor/hooks.json     — JSON, hooks under "<eventName>"
//! - Codex:       ~/.codex/config.toml     — TOML; v0.1 SKIPS Codex stub writing
//!   (no toml dep). Codex still detected; manual hook setup documented in
//!   MAC_HANDOVER.md.

use anyhow::{Context, Result};
use serde_json::Value;
use std::io::Write;
use std::path::Path;

use crate::daemon::hook_endpoint::LOOPBACK_PORT;

/// Return the canonical curl stub line for a (tool, event) pair. The host
/// and port come from the daemon's `LOOPBACK_PORT` constant — single
/// source of truth across the codebase.
pub fn curl_stub(tool: &str, event: &str) -> String {
    format!(
        "curl -X POST -s -d '{{}}' http://127.0.0.1:{LOOPBACK_PORT}/hook/{tool}/{event} > /dev/null 2>&1 &"
    )
}

/// Reject if `path` exists and is a symlink. We use `symlink_metadata` so
/// we look at the link itself, not its target. Non-existent paths are
/// fine (they will be created by the atomic write).
fn reject_symlink(path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => Err(anyhow::anyhow!(
            "refusing to follow symlink at {}",
            path.display()
        )),
        Ok(_) | Err(_) => Ok(()),
    }
}

/// Write Carryover hook stubs into Claude Code's settings.json.
/// Hooks live under the top-level `"hooks"` key.
/// Returns `true` if the file was modified.
pub fn write_claude_hooks(settings_path: &Path, hooks: &[(&str, &str)]) -> Result<bool> {
    write_json_hooks(settings_path, "hooks", hooks)
}

/// Write Carryover hook stubs into Cursor's hooks.json.
/// Hooks live at the top level of the file (not nested).
/// Returns `true` if the file was modified.
pub fn write_cursor_hooks(settings_path: &Path, hooks: &[(&str, &str)]) -> Result<bool> {
    let existing = match std::fs::read_to_string(settings_path) {
        Ok(s) => serde_json::from_str::<Value>(&s)
            .unwrap_or_else(|_| Value::Object(serde_json::Map::new())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Value::Object(serde_json::Map::new()),
        Err(e) => return Err(e).context("read cursor hooks.json"),
    };
    let mut map = match existing {
        Value::Object(m) => m,
        _ => serde_json::Map::new(),
    };
    let mut modified = false;
    for (tool, event) in hooks {
        let stub = curl_stub(tool, event);
        let key = event.to_string();
        if map.get(&key).and_then(|v| v.as_str()) != Some(&stub) {
            map.insert(key, Value::String(stub));
            modified = true;
        }
    }
    if !modified {
        return Ok(false);
    }
    write_json_atomic(settings_path, &Value::Object(map))?;
    Ok(true)
}

/// Write hooks under a nested `top_key` in the JSON file.
fn write_json_hooks(path: &Path, top_key: &str, hooks: &[(&str, &str)]) -> Result<bool> {
    let existing = match std::fs::read_to_string(path) {
        Ok(s) => serde_json::from_str::<Value>(&s)
            .unwrap_or_else(|_| Value::Object(serde_json::Map::new())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Value::Object(serde_json::Map::new()),
        Err(e) => return Err(e).context("read tool settings"),
    };
    let mut root = match existing {
        Value::Object(m) => m,
        _ => serde_json::Map::new(),
    };
    let hooks_obj = root
        .entry(top_key.to_string())
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    let hooks_map = match hooks_obj {
        Value::Object(m) => m,
        other => {
            *other = Value::Object(serde_json::Map::new());
            match other {
                Value::Object(m) => m,
                _ => unreachable!(),
            }
        }
    };
    let mut modified = false;
    for (tool, event) in hooks {
        let stub = curl_stub(tool, event);
        let key = event.to_string();
        if hooks_map.get(&key).and_then(|v| v.as_str()) != Some(&stub) {
            hooks_map.insert(key, Value::String(stub));
            modified = true;
        }
    }
    if !modified {
        return Ok(false);
    }
    write_json_atomic(path, &Value::Object(root))?;
    Ok(true)
}

fn write_json_atomic(path: &Path, value: &Value) -> Result<()> {
    // Refuse to write through a symlink at the destination. Without this,
    // an attacker who can drop a symlink in `~/.claude/` or `~/.cursor/`
    // would turn the install path into an arbitrary-write primitive.
    reject_symlink(path)?;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).context("create settings dir")?;
    }
    let json = serde_json::to_string_pretty(value).context("serialize json")?;
    let dir = path.parent().context("settings path has no parent")?;
    let mut tmp = tempfile::NamedTempFile::new_in(dir).context("create temp file")?;
    tmp.write_all(json.as_bytes()).context("write temp file")?;
    tmp.as_file_mut().sync_all().context("sync temp file")?;
    tmp.persist(path)
        .map_err(|e| anyhow::anyhow!("{}", e))
        .context("persist atomic write")?;
    Ok(())
}

/// Remove our hook stubs from Claude Code's settings.json.
/// Returns `true` if the file was modified.
pub fn remove_claude_hooks(path: &Path, events: &[&str]) -> Result<bool> {
    remove_json_hooks(path, "hooks", events)
}

/// Remove our hook stubs from Cursor's hooks.json.
/// Returns `true` if the file was modified.
pub fn remove_cursor_hooks(path: &Path, events: &[&str]) -> Result<bool> {
    let existing = match std::fs::read_to_string(path) {
        Ok(s) => serde_json::from_str::<Value>(&s).unwrap_or(Value::Null),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e).context("read cursor hooks.json"),
    };
    let mut map = match existing {
        Value::Object(m) => m,
        _ => return Ok(false),
    };
    let mut modified = false;
    for event in events {
        if map.remove(*event).is_some() {
            modified = true;
        }
    }
    if !modified {
        return Ok(false);
    }
    write_json_atomic(path, &Value::Object(map))?;
    Ok(true)
}

fn remove_json_hooks(path: &Path, top_key: &str, events: &[&str]) -> Result<bool> {
    let existing = match std::fs::read_to_string(path) {
        Ok(s) => serde_json::from_str::<Value>(&s).unwrap_or(Value::Null),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e).context("read tool settings"),
    };
    let mut root = match existing {
        Value::Object(m) => m,
        _ => return Ok(false),
    };
    let mut modified = false;
    if let Some(Value::Object(hooks_map)) = root.get_mut(top_key) {
        for event in events {
            if hooks_map.remove(*event).is_some() {
                modified = true;
            }
        }
    }
    if !modified {
        return Ok(false);
    }
    write_json_atomic(path, &Value::Object(root))?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn curl_stub_format() {
        let s = curl_stub("claude", "SessionStart");
        assert_eq!(
            s,
            "curl -X POST -s -d '{}' http://127.0.0.1:47823/hook/claude/SessionStart > /dev/null 2>&1 &"
        );
    }

    #[test]
    fn write_claude_hooks_creates_settings_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.json");
        let hooks = [
            ("claude", "SessionStart"),
            ("claude", "SessionEnd"),
            ("claude", "PreCompact"),
            ("claude", "UserPromptSubmit"),
        ];
        let modified = write_claude_hooks(&p, &hooks).unwrap();
        assert!(modified, "file should have been written");

        let raw = std::fs::read_to_string(&p).unwrap();
        let val: Value = serde_json::from_str(&raw).unwrap();
        let hooks_obj = val.get("hooks").unwrap().as_object().unwrap();
        assert!(hooks_obj.contains_key("SessionStart"));
        assert!(hooks_obj.contains_key("SessionEnd"));
        assert!(hooks_obj.contains_key("PreCompact"));
        assert!(hooks_obj.contains_key("UserPromptSubmit"));
        assert_eq!(hooks_obj.len(), 4);
    }

    #[test]
    fn write_claude_hooks_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.json");
        let hooks = [("claude", "SessionStart"), ("claude", "SessionEnd")];
        let first = write_claude_hooks(&p, &hooks).unwrap();
        assert!(first);
        let second = write_claude_hooks(&p, &hooks).unwrap();
        assert!(!second, "second call should return false (no change)");
    }

    #[test]
    fn write_claude_hooks_preserves_unrelated_keys() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.json");
        // Pre-populate with an unrelated key.
        let initial = json!({"theme": "dark"});
        std::fs::write(&p, serde_json::to_string_pretty(&initial).unwrap()).unwrap();

        let hooks = [("claude", "SessionStart")];
        write_claude_hooks(&p, &hooks).unwrap();

        let raw = std::fs::read_to_string(&p).unwrap();
        let val: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(
            val.get("theme").and_then(|v| v.as_str()),
            Some("dark"),
            "unrelated key must survive"
        );
        assert!(val.get("hooks").is_some(), "hooks key must be added");
    }

    #[test]
    fn remove_claude_hooks_idempotent_on_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.json");
        let result = remove_claude_hooks(&p, &["SessionStart"]).unwrap();
        assert!(!result, "removing from non-existent file returns false");
    }

    #[test]
    fn remove_claude_hooks_removes_written_stubs() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.json");
        let hooks = [("claude", "SessionStart"), ("claude", "SessionEnd")];
        write_claude_hooks(&p, &hooks).unwrap();

        let removed = remove_claude_hooks(&p, &["SessionStart"]).unwrap();
        assert!(removed);

        let raw = std::fs::read_to_string(&p).unwrap();
        let val: Value = serde_json::from_str(&raw).unwrap();
        let hooks_obj = val.get("hooks").unwrap().as_object().unwrap();
        assert!(!hooks_obj.contains_key("SessionStart"), "should be removed");
        assert!(hooks_obj.contains_key("SessionEnd"), "other hooks survive");
    }

    #[test]
    fn write_cursor_hooks_top_level_keys() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("hooks.json");
        let hooks = [("cursor", "sessionStart"), ("cursor", "stop")];
        let modified = write_cursor_hooks(&p, &hooks).unwrap();
        assert!(modified);

        let raw = std::fs::read_to_string(&p).unwrap();
        let val: Value = serde_json::from_str(&raw).unwrap();
        let obj = val.as_object().unwrap();
        // Keys must be at the top level, not nested under "hooks".
        assert!(
            obj.contains_key("sessionStart"),
            "sessionStart at top level"
        );
        assert!(obj.contains_key("stop"), "stop at top level");
        assert!(!obj.contains_key("hooks"), "must NOT nest under hooks key");
    }

    #[test]
    fn remove_cursor_hooks_idempotent_on_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("hooks.json");
        let result = remove_cursor_hooks(&p, &["sessionStart"]).unwrap();
        assert!(!result);
    }
}
