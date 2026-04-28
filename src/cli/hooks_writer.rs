//! Writes Carryover hook stubs into each tool's native settings file.
//! Idempotent: if the stub for a (tool, event) pair already matches our
//! canonical entry, the file is left alone.
//!
//! Shapes:
//! - Claude Code: ~/.claude/settings.json  — JSON, `hooks.<EventName>` must
//!   be an array of matcher objects: `[{"matcher":"","hooks":[{"type":"command","command":"..."}]}]`
//! - Cursor:      ~/.cursor/hooks.json     — JSON, hooks under "<eventName>"
//!   as `{ "command": "...", "version": 1 }` objects. (Cursor B5 fix: TODO)
//! - Codex:       ~/.codex/config.toml     — TOML; v0.1 SKIPS Codex stub writing.

use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::io::Write;
use std::path::Path;

use crate::daemon::hook_endpoint::LOOPBACK_PORT;

/// Return the canonical curl stub command for a (tool, event) pair.
pub fn curl_stub(tool: &str, event: &str) -> String {
    format!(
        "curl -X POST -s -d '{{}}' http://127.0.0.1:{LOOPBACK_PORT}/hook/{tool}/{event} > /dev/null 2>&1 &"
    )
}

/// Build the canonical Claude Code matcher entry for a given command.
/// Shape: `{"matcher":"","hooks":[{"type":"command","command":"<cmd>"}]}`
fn claude_matcher_entry(command: &str) -> Value {
    json!({
        "matcher": "",
        "hooks": [{ "type": "command", "command": command }]
    })
}

/// Return `true` if `entry` is the matcher object we wrote for `command`.
fn is_our_entry(entry: &Value, command: &str) -> bool {
    entry
        .get("hooks")
        .and_then(|h| h.as_array())
        .and_then(|arr| arr.first())
        .and_then(|h| h.get("command"))
        .and_then(|c| c.as_str())
        == Some(command)
}

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
///
/// Each event gets one matcher-object entry appended to the `hooks.<Event>`
/// array. If our entry (identified by the exact command string) is already
/// present, the file is left unchanged. Entries from other tools or plugins
/// in the same array are preserved.
///
/// Returns `true` if the file was modified.
pub fn write_claude_hooks(settings_path: &Path, hooks: &[(&str, &str)]) -> Result<bool> {
    let existing = match std::fs::read_to_string(settings_path) {
        Ok(s) => serde_json::from_str::<Value>(&s)
            .unwrap_or_else(|_| Value::Object(serde_json::Map::new())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Value::Object(serde_json::Map::new()),
        Err(e) => return Err(e).context("read claude settings.json"),
    };

    let mut root = match existing {
        Value::Object(m) => m,
        _ => serde_json::Map::new(),
    };

    // Ensure "hooks" is an object; if it's anything else (including the
    // old bare-string shape written by a previous Carryover version),
    // replace it with an empty object so we rebuild correctly.
    match root.get("hooks") {
        Some(Value::Object(_)) | None => {}
        _ => {
            root.insert("hooks".to_string(), Value::Object(serde_json::Map::new()));
        }
    }

    let hooks_obj = root
        .entry("hooks".to_string())
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    let hooks_map = match hooks_obj {
        Value::Object(m) => m,
        _ => unreachable!("ensured above"),
    };

    let mut modified = false;
    for (tool, event) in hooks {
        let stub = curl_stub(tool, event);
        let event_key = event.to_string();
        let entry = claude_matcher_entry(&stub);

        // Ensure the value for this event is an array.
        let arr = hooks_map
            .entry(event_key)
            .or_insert_with(|| Value::Array(vec![]));
        if !matches!(arr, Value::Array(_)) {
            // Old bare-string — replace with empty array, then append.
            *arr = Value::Array(vec![]);
        }
        let arr = match arr {
            Value::Array(a) => a,
            _ => unreachable!(),
        };

        // Idempotent: only append if our entry isn't already present.
        if !arr.iter().any(|e| is_our_entry(e, &stub)) {
            arr.push(entry);
            modified = true;
        }
    }

    if !modified {
        return Ok(false);
    }
    write_json_atomic(settings_path, &Value::Object(root))?;
    Ok(true)
}

/// Remove our hook stubs from Claude Code's settings.json.
///
/// Removes only the specific matcher entry we wrote (identified by its
/// command string). Other entries in the same array are left untouched.
/// Returns `true` if the file was modified.
pub fn remove_claude_hooks(path: &Path, events: &[&str]) -> Result<bool> {
    let existing = match std::fs::read_to_string(path) {
        Ok(s) => serde_json::from_str::<Value>(&s).unwrap_or(Value::Null),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e).context("read claude settings.json"),
    };
    let mut root = match existing {
        Value::Object(m) => m,
        _ => return Ok(false),
    };

    let mut modified = false;
    if let Some(Value::Object(hooks_map)) = root.get_mut("hooks") {
        for event in events {
            let port_path = format!("/hook/claude/{event}");
            if let Some(Value::Array(arr)) = hooks_map.get_mut(*event) {
                let before = arr.len();
                arr.retain(|e| {
                    let cmd = e
                        .get("hooks")
                        .and_then(|h| h.as_array())
                        .and_then(|a| a.first())
                        .and_then(|h| h.get("command"))
                        .and_then(|c| c.as_str())
                        .unwrap_or("");
                    !cmd.contains(&port_path)
                });
                if arr.len() != before {
                    modified = true;
                }
            }
        }
        // Prune event keys whose arrays are now empty.
        hooks_map.retain(|_, v| !matches!(v, Value::Array(a) if a.is_empty()));
    }

    if !modified {
        return Ok(false);
    }
    write_json_atomic(path, &Value::Object(root))?;
    Ok(true)
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

fn write_json_atomic(path: &Path, value: &Value) -> Result<()> {
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ---- helpers -----------------------------------------------------------

    /// Extract the first command string from a Claude hook array entry.
    fn first_command(val: &Value, event: &str) -> Option<String> {
        val.get("hooks")?
            .as_object()?
            .get(event)?
            .as_array()?
            .iter()
            .find_map(|entry| {
                entry
                    .get("hooks")?
                    .as_array()?
                    .first()?
                    .get("command")?
                    .as_str()
                    .map(str::to_string)
            })
    }

    /// Assert a Claude hook array entry has the canonical matcher-object shape.
    fn assert_matcher_shape(val: &Value, event: &str) {
        let hooks_obj = val
            .get("hooks")
            .expect("hooks key")
            .as_object()
            .expect("hooks is object");
        let arr = hooks_obj
            .get(event)
            .unwrap_or_else(|| panic!("event {event} not found"))
            .as_array()
            .unwrap_or_else(|| panic!("event {event} is not an array"));
        assert!(!arr.is_empty(), "array for {event} must not be empty");
        let entry = &arr[0];
        assert!(
            entry.get("matcher").is_some(),
            "entry must have 'matcher' key"
        );
        let inner_hooks = entry
            .get("hooks")
            .expect("entry must have 'hooks' key")
            .as_array()
            .expect("hooks must be an array");
        assert!(!inner_hooks.is_empty());
        let hook = &inner_hooks[0];
        assert_eq!(hook.get("type").and_then(|v| v.as_str()), Some("command"));
        assert!(
            hook.get("command").and_then(|v| v.as_str()).is_some(),
            "hook must have 'command' string"
        );
    }

    // ---- write_claude_hooks -----------------------------------------------

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

        // Shape assertions — not just presence.
        for event in [
            "SessionStart",
            "SessionEnd",
            "PreCompact",
            "UserPromptSubmit",
        ] {
            assert_matcher_shape(&val, event);
        }
    }

    #[test]
    fn write_claude_hooks_command_matches_curl_stub() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.json");
        write_claude_hooks(&p, &[("claude", "SessionStart")]).unwrap();
        let raw = std::fs::read_to_string(&p).unwrap();
        let val: Value = serde_json::from_str(&raw).unwrap();
        let cmd = first_command(&val, "SessionStart").unwrap();
        assert_eq!(cmd, curl_stub("claude", "SessionStart"));
    }

    #[test]
    fn write_claude_hooks_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.json");
        let hooks = [("claude", "SessionStart"), ("claude", "SessionEnd")];
        let first = write_claude_hooks(&p, &hooks).unwrap();
        assert!(first);
        let second = write_claude_hooks(&p, &hooks).unwrap();
        assert!(
            !second,
            "second call with same hooks returns false (no change)"
        );

        // Array must still contain exactly one entry per event (no duplicates).
        let val: Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        let arr = val["hooks"]["SessionStart"].as_array().unwrap();
        assert_eq!(
            arr.len(),
            1,
            "idempotent install must not duplicate entries"
        );
    }

    #[test]
    fn write_claude_hooks_coexists_with_other_entries() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.json");

        // Pre-populate with a foreign hook entry in SessionStart.
        let initial = json!({
            "hooks": {
                "SessionStart": [
                    { "matcher": "Bash", "hooks": [{ "type": "command", "command": "echo hi" }] }
                ]
            }
        });
        std::fs::write(&p, serde_json::to_string_pretty(&initial).unwrap()).unwrap();

        write_claude_hooks(&p, &[("claude", "SessionStart")]).unwrap();

        let val: Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        let arr = val["hooks"]["SessionStart"].as_array().unwrap();
        // Both entries must be present.
        assert_eq!(arr.len(), 2, "our entry appended alongside foreign entry");
        assert!(
            arr.iter().any(|e| e["hooks"][0]["command"] == "echo hi"),
            "foreign entry preserved"
        );
        assert!(
            arr.iter()
                .any(|e| is_our_entry(e, &curl_stub("claude", "SessionStart"))),
            "our entry added"
        );
    }

    #[test]
    fn write_claude_hooks_preserves_unrelated_keys() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.json");
        let initial = json!({"theme": "dark"});
        std::fs::write(&p, serde_json::to_string_pretty(&initial).unwrap()).unwrap();

        write_claude_hooks(&p, &[("claude", "SessionStart")]).unwrap();

        let val: Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(
            val.get("theme").and_then(|v| v.as_str()),
            Some("dark"),
            "unrelated key must survive"
        );
    }

    #[test]
    fn write_claude_hooks_upgrades_old_bare_string_format() {
        // If the file has the old wrong format (bare string), we should
        // replace it with the correct array-of-matchers shape.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.json");
        let old_format = json!({
            "hooks": {
                "SessionStart": "curl -X POST -s http://127.0.0.1:47823/hook/claude/SessionStart &"
            }
        });
        std::fs::write(&p, serde_json::to_string_pretty(&old_format).unwrap()).unwrap();

        write_claude_hooks(&p, &[("claude", "SessionStart")]).unwrap();

        let val: Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_matcher_shape(&val, "SessionStart");
    }

    // ---- remove_claude_hooks ----------------------------------------------

    #[test]
    fn remove_claude_hooks_idempotent_on_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.json");
        let result = remove_claude_hooks(&p, &["SessionStart"]).unwrap();
        assert!(!result, "removing from non-existent file returns false");
    }

    #[test]
    fn remove_claude_hooks_removes_our_entry_only() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.json");

        // Write a foreign entry + our entry.
        let initial = json!({
            "hooks": {
                "SessionStart": [
                    { "matcher": "Bash", "hooks": [{ "type": "command", "command": "echo hi" }] }
                ]
            }
        });
        std::fs::write(&p, serde_json::to_string_pretty(&initial).unwrap()).unwrap();
        write_claude_hooks(&p, &[("claude", "SessionStart")]).unwrap();

        // Remove should strip our entry but keep the foreign one.
        let removed = remove_claude_hooks(&p, &["SessionStart"]).unwrap();
        assert!(removed);

        let val: Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        let arr = val["hooks"]["SessionStart"].as_array().unwrap();
        assert_eq!(arr.len(), 1, "foreign entry must survive");
        assert_eq!(arr[0]["hooks"][0]["command"], "echo hi");
    }

    #[test]
    fn remove_claude_hooks_removes_written_stubs() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.json");
        let hooks = [("claude", "SessionStart"), ("claude", "SessionEnd")];
        write_claude_hooks(&p, &hooks).unwrap();

        let removed = remove_claude_hooks(&p, &["SessionStart"]).unwrap();
        assert!(removed);

        let val: Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        let hooks_map = val["hooks"].as_object().unwrap();
        // Empty arrays are pruned — key should be absent.
        assert!(
            !hooks_map.contains_key("SessionStart"),
            "empty SessionStart array should be pruned"
        );
        // SessionEnd array still has our entry.
        assert_matcher_shape(&val, "SessionEnd");
    }

    // ---- write_cursor_hooks -----------------------------------------------

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
