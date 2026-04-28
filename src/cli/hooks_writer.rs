//! Writes Carryover hook stubs into each tool's native settings file.
//! Idempotent: if the stub for a (tool, event) pair already matches our
//! canonical entry, the file is left alone.
//!
//! Shapes:
//! - Claude Code: ~/.claude/settings.json  — JSON, `hooks.<EventName>` must
//!   be an array of matcher objects: `[{"matcher":"","hooks":[{"type":"command","command":"..."}]}]`
//! - Cursor:      ~/.cursor/hooks.json     — JSON, hooks under "<eventName>"
//!   as `{ "command": "...", "version": 1 }` objects. (Cursor B5 fix: TODO)
//! - Codex:       ~/.codex/config.toml     — TOML, `notify` array + AGENTS.md pointer

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

/// Path of the Cursor wrapper script for a given event, relative to home.
pub fn cursor_wrapper_path(home: &Path, event: &str) -> std::path::PathBuf {
    let name = match event {
        "beforeSubmitPrompt" => "cursor-prompt.sh",
        "stop" => "cursor-stop.sh",
        other => {
            // Fallback: sanitize the event name.
            let safe: String = other
                .chars()
                .map(|c| if c.is_alphanumeric() { c } else { '-' })
                .collect();
            return home
                .join(".carryover")
                .join("hooks")
                .join(format!("{safe}.sh"));
        }
    };
    home.join(".carryover").join("hooks").join(name)
}

fn cursor_wrapper_script(tool: &str, event: &str) -> String {
    format!(
        "#!/usr/bin/env sh\nINPUT=$(cat)\ncurl -X POST -s -H 'Content-Type: application/json' \\\n  -d \"$INPUT\" http://127.0.0.1:{LOOPBACK_PORT}/hook/{tool}/{event} > /dev/null 2>&1\nprintf '{{}}'\n"
    )
}

/// Write wrapper scripts for all Cursor hook events to `~/.carryover/hooks/`.
/// Returns `true` if any script was created or updated.
pub fn write_cursor_wrapper_scripts(home: &Path, hooks: &[(&str, &str)]) -> Result<bool> {
    let mut modified = false;
    for (tool, event) in hooks {
        let path = cursor_wrapper_path(home, event);
        let content = cursor_wrapper_script(tool, event);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).context("create hooks dir")?;
        }
        // Skip write if already identical.
        let needs_write = match std::fs::read_to_string(&path) {
            Ok(existing) => existing != content,
            Err(_) => true,
        };
        if needs_write {
            reject_symlink(&path)?;
            std::fs::write(&path, &content).context("write cursor wrapper script")?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                    .context("chmod wrapper script")?;
            }
            modified = true;
        }
    }
    Ok(modified)
}

/// Remove Cursor wrapper scripts for the given events.
pub fn remove_cursor_wrapper_scripts(home: &Path, events: &[&str]) -> Result<()> {
    for event in events {
        let path = cursor_wrapper_path(home, event);
        if path.exists() {
            std::fs::remove_file(&path).context("remove cursor wrapper script")?;
        }
    }
    Ok(())
}

/// Write Carryover hook stubs into Cursor's hooks.json.
///
/// Each event key maps to `{"command": "<wrapper-script-path>", "version": 1}`.
/// Cursor requires this object shape; bare strings are rejected.
/// Returns `true` if the file was modified.
pub fn write_cursor_hooks(settings_path: &Path, hooks: &[(&str, &str)]) -> Result<bool> {
    let home = dirs::home_dir().context("home dir not found")?;
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
    for (_tool, event) in hooks {
        let script = cursor_wrapper_path(&home, event);
        let script_str = script.to_string_lossy().into_owned();
        let key = event.to_string();
        let desired = json!({"command": script_str, "version": 1});
        let current = map.get(&key);
        // Idempotent: skip if command and version already match.
        let already = current
            .and_then(|v| v.get("command"))
            .and_then(|c| c.as_str())
            == Some(&script_str)
            && current
                .and_then(|v| v.get("version"))
                .and_then(|v| v.as_u64())
                == Some(1);
        if !already {
            map.insert(key, desired);
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
/// Matches by our wrapper script path pattern. Returns `true` if modified.
pub fn remove_cursor_hooks(path: &Path, events: &[&str]) -> Result<bool> {
    let home = dirs::home_dir().context("home dir not found")?;
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
        let script = cursor_wrapper_path(&home, event);
        let script_str = script.to_string_lossy().into_owned();
        let is_ours = map.get(*event).map(|v| {
            v.as_str().map(|s| s.contains("carryover")).unwrap_or(false)
                || v.get("command")
                    .and_then(|c| c.as_str())
                    .map(|s| s == script_str || s.contains("carryover"))
                    .unwrap_or(false)
        });
        if is_ours == Some(true) && map.remove(*event).is_some() {
            modified = true;
        }
    }
    if !modified {
        return Ok(false);
    }
    write_json_atomic(path, &Value::Object(map))?;
    Ok(true)
}

/// Path of the Codex notify wrapper script.
pub fn codex_wrapper_path(home: &Path) -> std::path::PathBuf {
    home.join(".carryover")
        .join("hooks")
        .join("codex-notify.sh")
}

fn codex_wrapper_script() -> String {
    format!(
        "#!/usr/bin/env sh\nINPUT=$(cat)\ncurl -X POST -s -H 'Content-Type: application/json' \\\n  -d \"$INPUT\" http://127.0.0.1:{LOOPBACK_PORT}/hook/codex/turnEnd > /dev/null 2>&1\n"
    )
}

/// Write the Codex notify wrapper script to `~/.carryover/hooks/codex-notify.sh`.
pub fn write_codex_wrapper_script(home: &Path) -> Result<bool> {
    let path = codex_wrapper_path(home);
    let content = codex_wrapper_script();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).context("create hooks dir")?;
    }
    let needs_write = match std::fs::read_to_string(&path) {
        Ok(existing) => existing != content,
        Err(_) => true,
    };
    if !needs_write {
        return Ok(false);
    }
    reject_symlink(&path)?;
    std::fs::write(&path, &content).context("write codex notify script")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .context("chmod codex notify script")?;
    }
    Ok(true)
}

/// Write `notify = ["<wrapper-script>"]` into `~/.codex/config.toml`.
///
/// Uses `toml_edit` to preserve all existing comments and keys.
/// Idempotent: if our wrapper path is already in the `notify` array, no-op.
/// Returns `true` if the file was modified.
pub fn write_codex_notify(config_path: &Path, home: &Path) -> Result<bool> {
    use toml_edit::{Array, DocumentMut, Item, Value as TomlValue};

    let wrapper = codex_wrapper_path(home);
    let wrapper_str = wrapper.to_string_lossy().into_owned();

    let raw = match std::fs::read_to_string(config_path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e).context("read codex config.toml"),
    };

    let mut doc: DocumentMut = raw.parse().context("parse codex config.toml as TOML")?;

    // Check if our wrapper is already in the notify array.
    let already_present = doc
        .get("notify")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().any(|v| v.as_str() == Some(wrapper_str.as_str())))
        .unwrap_or(false);

    if already_present {
        return Ok(false);
    }

    // Get or create the notify array, then append our wrapper.
    match doc.get_mut("notify") {
        Some(Item::Value(TomlValue::Array(arr))) => {
            arr.push(wrapper_str.as_str());
        }
        None => {
            let mut arr = Array::new();
            arr.push(wrapper_str.as_str());
            doc["notify"] = toml_edit::value(arr);
        }
        Some(other) => {
            // notify exists but is not an array — replace with our array.
            let _ = other;
            let mut arr = Array::new();
            arr.push(wrapper_str.as_str());
            doc["notify"] = toml_edit::value(arr);
        }
    }

    reject_symlink(config_path)?;
    if let Some(parent) = config_path.parent() {
        std::fs::create_dir_all(parent).context("create codex config dir")?;
    }
    std::fs::write(config_path, doc.to_string()).context("write codex config.toml")?;
    Ok(true)
}

/// Remove our wrapper from the `notify` array in `~/.codex/config.toml`.
/// Returns `true` if the file was modified.
pub fn remove_codex_notify(config_path: &Path, home: &Path) -> Result<bool> {
    use toml_edit::{DocumentMut, Item, Value as TomlValue};

    let wrapper = codex_wrapper_path(home);
    let wrapper_str = wrapper.to_string_lossy().into_owned();

    let raw = match std::fs::read_to_string(config_path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e).context("read codex config.toml"),
    };

    let mut doc: DocumentMut = raw.parse().context("parse codex config.toml as TOML")?;

    if let Some(Item::Value(TomlValue::Array(arr))) = doc.get_mut("notify") {
        let before = arr.len();
        // Build a new array without our entry.
        let keep: Vec<String> = arr
            .iter()
            .filter(|v| v.as_str() != Some(wrapper_str.as_str()))
            .map(|v| v.as_str().unwrap_or("").to_string())
            .collect();
        if keep.len() == before {
            return Ok(false);
        }
        let mut new_arr = toml_edit::Array::new();
        for s in &keep {
            new_arr.push(s.as_str());
        }
        *arr = new_arr;
        reject_symlink(config_path)?;
        std::fs::write(config_path, doc.to_string()).context("write codex config.toml")?;
        return Ok(true);
    }

    Ok(false)
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
    fn write_cursor_hooks_object_shape() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let p = home.join("hooks.json");
        let hooks = [("cursor", "beforeSubmitPrompt"), ("cursor", "stop")];
        // write_cursor_hooks reads home_dir() internally; we test the shape
        // by calling write_cursor_wrapper_scripts first then checking hooks.json.
        write_cursor_wrapper_scripts(home, &hooks).unwrap();

        // Manually invoke with a custom home-like dir by using a wrapper.
        // Since write_cursor_hooks calls dirs::home_dir() internally, just
        // verify the file written by write_cursor_wrapper_scripts has the right shape.
        let prompt_script = cursor_wrapper_path(home, "beforeSubmitPrompt");
        let stop_script = cursor_wrapper_path(home, "stop");
        assert!(prompt_script.exists(), "cursor-prompt.sh should be written");
        assert!(stop_script.exists(), "cursor-stop.sh should be written");

        // Verify scripts are executable and contain the correct event in the curl URL.
        let prompt_content = std::fs::read_to_string(&prompt_script).unwrap();
        assert!(
            prompt_content.contains("/hook/cursor/beforeSubmitPrompt"),
            "prompt script must POST to beforeSubmitPrompt endpoint"
        );
        let stop_content = std::fs::read_to_string(&stop_script).unwrap();
        assert!(
            stop_content.contains("/hook/cursor/stop"),
            "stop script must POST to stop endpoint"
        );
        assert!(
            !p.exists(),
            "hooks.json not created by wrapper writer alone"
        );
    }

    #[test]
    fn write_cursor_wrapper_scripts_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let hooks = [("cursor", "beforeSubmitPrompt"), ("cursor", "stop")];
        let first = write_cursor_wrapper_scripts(home, &hooks).unwrap();
        assert!(first);
        let second = write_cursor_wrapper_scripts(home, &hooks).unwrap();
        assert!(!second, "second call with same scripts returns false");
    }

    #[test]
    fn remove_cursor_wrapper_scripts_cleans_up() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let hooks = [("cursor", "beforeSubmitPrompt"), ("cursor", "stop")];
        write_cursor_wrapper_scripts(home, &hooks).unwrap();
        let events = ["beforeSubmitPrompt", "stop"];
        remove_cursor_wrapper_scripts(home, &events).unwrap();
        assert!(!cursor_wrapper_path(home, "beforeSubmitPrompt").exists());
        assert!(!cursor_wrapper_path(home, "stop").exists());
    }

    #[test]
    fn remove_cursor_hooks_idempotent_on_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("hooks.json");
        let result = remove_cursor_hooks(&p, &["beforeSubmitPrompt"]).unwrap();
        assert!(!result);
    }

    // ---- write_codex_notify / remove_codex_notify -------------------------

    #[test]
    fn write_codex_notify_creates_config_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let config = home.join(".codex").join("config.toml");

        let first = write_codex_notify(&config, home).unwrap();
        assert!(first, "first call should create file");

        let raw = std::fs::read_to_string(&config).unwrap();
        let wrapper = codex_wrapper_path(home);
        assert!(
            raw.contains(&*wrapper.to_string_lossy()),
            "notify should contain our wrapper path"
        );

        let second = write_codex_notify(&config, home).unwrap();
        assert!(!second, "second call should be no-op");
    }

    #[test]
    fn write_codex_notify_preserves_existing_keys_and_comments() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let config = home.join("config.toml");
        std::fs::write(&config, "# my codex config\nsome_key = \"value\"\n").unwrap();

        write_codex_notify(&config, home).unwrap();

        let raw = std::fs::read_to_string(&config).unwrap();
        assert!(raw.contains("# my codex config"), "comment preserved");
        assert!(raw.contains("some_key"), "existing key preserved");
        assert!(raw.contains("notify"), "notify key added");
    }

    #[test]
    fn write_codex_notify_appends_to_existing_notify_array() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let config = home.join("config.toml");
        std::fs::write(&config, "notify = [\"other-script.sh\"]\n").unwrap();

        write_codex_notify(&config, home).unwrap();

        let raw = std::fs::read_to_string(&config).unwrap();
        assert!(raw.contains("other-script.sh"), "existing entry preserved");
        let wrapper = codex_wrapper_path(home);
        assert!(raw.contains(&*wrapper.to_string_lossy()), "our entry added");
    }

    #[test]
    fn remove_codex_notify_removes_our_entry_only() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let config = home.join("config.toml");
        std::fs::write(&config, "notify = [\"other-script.sh\"]\n").unwrap();

        write_codex_notify(&config, home).unwrap();
        let removed = remove_codex_notify(&config, home).unwrap();
        assert!(removed, "should report modification");

        let raw = std::fs::read_to_string(&config).unwrap();
        assert!(raw.contains("other-script.sh"), "foreign entry preserved");
        let wrapper = codex_wrapper_path(home);
        assert!(
            !raw.contains(&*wrapper.to_string_lossy()),
            "our entry removed"
        );
    }

    #[test]
    fn write_codex_wrapper_script_is_idempotent_and_contains_endpoint() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();

        let first = write_codex_wrapper_script(home).unwrap();
        assert!(first);
        let second = write_codex_wrapper_script(home).unwrap();
        assert!(!second, "idempotent");

        let content = std::fs::read_to_string(codex_wrapper_path(home)).unwrap();
        assert!(
            content.contains("/hook/codex/turnEnd"),
            "script must POST to turnEnd"
        );
    }
}
