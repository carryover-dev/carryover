//! Cursor (the AI editor) transcript adapter.
//!
//! Reads conversation data from two Cursor storage locations:
//! - Global DB: `~/.config/Cursor/User/globalStorage/state.vscdb` (Linux)
//!   `~/Library/Application Support/Cursor/User/globalStorage/state.vscdb` (macOS)
//! - Per-workspace DBs: `workspaceStorage/<hash>/state.vscdb` (sibling of globalStorage)
//!
//! Cursor migrated its storage schema. The adapter auto-detects which is in use:
//! - New schema: `composer.composerHeaders` in global DB + per-workspace prompt arrays
//! - Old schema: `aiService.generations`, `aiService.prompts`, `composer.composerData`
//!   all in the global DB
//!
//! When the live DB is locked (Cursor is running), opening it directly may return
//! `SQLITE_BUSY`. The adapter falls back to copying the DB + its WAL/SHM sidecars
//! into a tempdir and reading from the copy.

use crate::adapters::{Adapter, AdapterError, RawRecord};
use crate::storage::LedgerRow;
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Per-poll read cap for a single ItemTable value.
const MAX_BYTES_PER_VALUE: usize = 64 * 1024 * 1024;

/// Maximum workspace DBs to scan per poll to bound I/O on large installs.
const MAX_WORKSPACES_PER_POLL: usize = 20;

// ---------------------------------------------------------------------------
// Cursor (bookmark)
// ---------------------------------------------------------------------------

/// Read-position bookmark for the Cursor adapter.
///
/// Supports both the new schema (seen_prompts per workspace) and the old
/// schema (last_msg_id string watermark). Old stored cursors that have
/// `db_path`/`last_rowid` fields deserialize cleanly because serde ignores
/// unknown fields by default.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CursorCursor {
    /// Unix ms of the most recently seen composer's `lastUpdatedAt`.
    #[serde(default)]
    pub last_updated_at_ms: i64,

    /// Per-workspace-id: number of prompts already emitted.
    /// Key = workspace hash, Value = count of consumed prompts.
    #[serde(default)]
    pub seen_prompts: HashMap<String, usize>,

    /// Per-workspace-id: text of the most recently emitted prompt. Used to
    /// recover the resume point even when Cursor truncates the prompts array
    /// (the array is bounded — when the user types a new prompt, the oldest
    /// gets dropped, so `seen_count == array_length` no longer means
    /// "caught up"). On read we look for this text in the current array; if
    /// found, we resume from the next index. If absent (truncated), we fall
    /// back to emitting the full array — duplicates are filtered downstream
    /// by progress-log dedupe and Task accumulation.
    #[serde(default)]
    pub last_prompt_text: HashMap<String, String>,

    /// Most recently active project path. Written here so that
    /// `infer_project_dir_from_cursor` in the pipeline can route fs-watcher
    /// events to the right `.carryover/` directory without re-reading headers.
    #[serde(default)]
    pub project_dir: Option<String>,

    /// Legacy watermark for old-schema reads. Preserved so that old stored
    /// cursors can resume without re-emitting everything on first upgrade.
    #[serde(default)]
    pub last_msg_id: String,
}

// ---------------------------------------------------------------------------
// Adapter
// ---------------------------------------------------------------------------

/// Cursor transcript adapter.
pub struct CursorAdapter {
    /// Optional override for the globalStorage directory.
    /// `None` means use the OS-default path.
    pub db_root: Option<PathBuf>,
}

impl CursorAdapter {
    /// Default constructor — uses the OS-default globalStorage path.
    pub fn new() -> Self {
        Self { db_root: None }
    }

    /// Test override: set the *globalStorage directory* (parent of `state.vscdb`).
    pub fn with_db_root(root: PathBuf) -> Self {
        Self {
            db_root: Some(root),
        }
    }

    /// Return the configured db_root, or the OS-default globalStorage path.
    fn resolve_db_root(&self) -> Result<PathBuf, AdapterError> {
        if let Some(p) = &self.db_root {
            return Ok(p.clone());
        }

        let home =
            dirs::home_dir().ok_or_else(|| AdapterError::PathNotFound(PathBuf::from("$HOME")))?;

        #[cfg(target_os = "linux")]
        {
            Ok(home
                .join(".config")
                .join("Cursor")
                .join("User")
                .join("globalStorage"))
        }

        #[cfg(target_os = "macos")]
        {
            Ok(home
                .join("Library")
                .join("Application Support")
                .join("Cursor")
                .join("User")
                .join("globalStorage"))
        }

        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            Err(AdapterError::PathNotFound(PathBuf::from(
                "Cursor globalStorage (unsupported platform for v0.1)",
            )))
        }
    }

    /// Path to the global `state.vscdb`.
    fn db_path(&self) -> Result<PathBuf, AdapterError> {
        Ok(self.resolve_db_root()?.join("state.vscdb"))
    }

    /// workspaceStorage root — a sibling directory of globalStorage.
    fn workspace_storage_root(&self) -> Result<PathBuf, AdapterError> {
        let global_root = self.resolve_db_root()?;
        let parent = global_root
            .parent()
            .ok_or_else(|| AdapterError::PathNotFound(global_root.clone()))?;
        Ok(parent.join("workspaceStorage"))
    }
}

impl Default for CursorAdapter {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Adapter trait impl
// ---------------------------------------------------------------------------

impl Adapter for CursorAdapter {
    type Cursor = CursorCursor;

    fn name(&self) -> &'static str {
        "cursor"
    }

    fn detect(&self) -> Result<Option<PathBuf>, AdapterError> {
        match self.db_path() {
            Ok(p) if p.exists() => Ok(Some(p)),
            _ => Ok(None),
        }
    }

    fn read_new_records(
        &self,
        since: &CursorCursor,
    ) -> Result<(Vec<RawRecord>, CursorCursor), AdapterError> {
        let global_db = self.db_path()?;
        if !global_db.exists() {
            return Ok((vec![], since.clone()));
        }

        let ws_root = self.workspace_storage_root()?;
        let (conn, _guard) = open_with_fallback(&global_db)?;

        match read_item_value(&conn, "composer.composerHeaders")? {
            Some(headers_text) => read_new_schema(since, &headers_text, &ws_root),
            None => read_old_schema(&conn, since),
        }
    }

    fn parse(&self, records: Vec<RawRecord>) -> Result<Vec<LedgerRow>, AdapterError> {
        let mut rows = Vec::with_capacity(records.len());

        for rec in records {
            let v: serde_json::Value =
                serde_json::from_slice(&rec.payload).map_err(|e| AdapterError::Parse {
                    offset: rec.offset,
                    context: "cursor payload is not valid JSON",
                    source: e,
                })?;

            let session_id = v
                .get("sessionId")
                .and_then(|s| s.as_str())
                .ok_or_else(|| AdapterError::Parse {
                    offset: rec.offset,
                    context: "missing sessionId field",
                    source: make_missing_field_error(),
                })?
                .to_string();

            let ts = extract_ts(&v, rec.offset)?;

            let role = v
                .get("role")
                .and_then(|r| r.as_str())
                .ok_or_else(|| AdapterError::Parse {
                    offset: rec.offset,
                    context: "missing role field",
                    source: make_missing_field_error(),
                })?
                .to_string();

            let content = match v.get("content") {
                None => String::new(),
                Some(serde_json::Value::String(s)) => s.clone(),
                Some(other) => serde_json::to_string(other).map_err(|e| AdapterError::Parse {
                    offset: rec.offset,
                    context: "failed to serialize content field",
                    source: e,
                })?,
            };

            rows.push(LedgerRow {
                session_id,
                tool: "cursor".to_string(),
                ts,
                role,
                content,
                tool_calls_json: None,
                files_touched_json: None,
                parent_id: None,
            });
        }

        Ok(rows)
    }
}

// ---------------------------------------------------------------------------
// New-schema reader
// ---------------------------------------------------------------------------

struct ComposerHeader {
    composer_id: String,
    workspace_id: String,
    fs_path: String,
    last_updated_at_ms: i64,
}

fn parse_composer_headers(text: &str) -> Result<Vec<ComposerHeader>, AdapterError> {
    let obj: serde_json::Value = serde_json::from_str(text).map_err(|e| AdapterError::Parse {
        offset: 0,
        context: "composer.composerHeaders is not valid JSON",
        source: e,
    })?;

    let arr = match obj.get("allComposers").and_then(|v| v.as_array()) {
        Some(a) => a,
        None => return Ok(vec![]),
    };

    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        let composer_id = match item.get("composerId").and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => continue,
        };

        let ws_ident = match item.get("workspaceIdentifier") {
            Some(v) => v,
            None => continue,
        };

        let workspace_id = match ws_ident.get("id").and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => continue,
        };

        let fs_path = ws_ident
            .get("uri")
            .and_then(|u| u.get("fsPath"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let last_updated_at_ms = item
            .get("lastUpdatedAt")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);

        out.push(ComposerHeader {
            composer_id,
            workspace_id,
            fs_path,
            last_updated_at_ms,
        });
    }

    Ok(out)
}

/// Workspace IDs are hex-ish hashes; reject anything with path-traversal chars.
fn is_safe_workspace_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Read new-style workspace prompts (plain `{text, commandType}` objects).
///
/// Returns prompts after the resume point. The resume point is determined by
/// (in priority order):
///   1. `last_text`: find the last occurrence of this text in the array, resume
///      from the next index. Survives Cursor's bounded-array truncation.
///   2. `skip_fallback`: legacy index-based skip (used on first upgrade when
///      `last_text` is None).
///
/// On truncation (last_text not found in array), emits the full array — the
/// progress-log and Task accumulators dedupe by content downstream.
fn read_workspace_prompts(
    conn: &Connection,
    last_text: Option<&str>,
    skip_fallback: usize,
) -> Result<(Vec<String>, Option<String>), AdapterError> {
    let text = match read_item_value(conn, "aiService.prompts")? {
        None => return Ok((vec![], None)),
        Some(t) => t,
    };

    let arr: Vec<serde_json::Value> =
        serde_json::from_str(&text).map_err(|e| AdapterError::Parse {
            offset: 0,
            context: "aiService.prompts is not a JSON array",
            source: e,
        })?;

    let resume_index = match last_text {
        Some(t) => arr
            .iter()
            .rposition(|item| item.get("text").and_then(|v| v.as_str()) == Some(t))
            .map(|i| i + 1)
            .unwrap_or(0), // not found = truncated, re-emit all (dedup handles)
        None => skip_fallback.min(arr.len()),
    };

    // Capture the last prompt's text (for the next read's watermark) BEFORE
    // we move arr into the iterator.
    let new_last_text = arr
        .last()
        .and_then(|item| item.get("text").and_then(|v| v.as_str()))
        .map(String::from);

    let out: Vec<String> = arr
        .into_iter()
        .skip(resume_index)
        .filter_map(|item| {
            item.get("text")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
        })
        .collect();

    Ok((out, new_last_text))
}

/// Read generation timestamps; index-aligned with prompts.
fn read_workspace_generation_timestamps(conn: &Connection) -> Result<Vec<i64>, AdapterError> {
    let text = match read_item_value(conn, "aiService.generations")? {
        None => return Ok(vec![]),
        Some(t) => t,
    };

    let arr: Vec<serde_json::Value> =
        serde_json::from_str(&text).map_err(|e| AdapterError::Parse {
            offset: 0,
            context: "aiService.generations is not a JSON array",
            source: e,
        })?;

    Ok(arr
        .into_iter()
        .filter_map(|item| item.get("unixMs").and_then(|v| v.as_i64()))
        .collect())
}

fn read_new_schema(
    since: &CursorCursor,
    headers_text: &str,
    ws_root: &Path,
) -> Result<(Vec<RawRecord>, CursorCursor), AdapterError> {
    let mut composers = parse_composer_headers(headers_text)?;
    if composers.is_empty() {
        return Ok((vec![], since.clone()));
    }

    // Most-recently-updated first; cap to bound I/O on large installs.
    composers.sort_unstable_by_key(|c| std::cmp::Reverse(c.last_updated_at_ms));
    composers.truncate(MAX_WORKSPACES_PER_POLL);

    // Most recently updated composer's path is the active project.
    let new_project_dir = composers
        .first()
        .map(|c| c.fs_path.clone())
        .filter(|s| !s.is_empty());

    let mut new_seen_prompts = since.seen_prompts.clone();
    let mut new_last_prompt_text = since.last_prompt_text.clone();
    let mut new_last_updated_at_ms = since.last_updated_at_ms;
    let mut all_msgs: Vec<ParsedMsg> = Vec::new();

    for composer in &composers {
        if !is_safe_workspace_id(&composer.workspace_id) {
            continue;
        }

        let ws_db = ws_root.join(&composer.workspace_id).join("state.vscdb");
        if !ws_db.exists() {
            continue;
        }

        // Use new_* (already updated by earlier composers in this batch) so
        // that two composers sharing the same workspace don't both re-read it.
        let skip_fallback = new_seen_prompts
            .get(&composer.workspace_id)
            .copied()
            .unwrap_or(0);
        let last_text = new_last_prompt_text
            .get(&composer.workspace_id)
            .map(|s| s.as_str());

        let (ws_conn, _ws_guard) = match open_with_fallback(&ws_db) {
            Ok(c) => c,
            Err(_) => continue,
        };

        let (new_prompts, latest_array_text) =
            read_workspace_prompts(&ws_conn, last_text, skip_fallback)?;

        // Update last_prompt_text watermark to the LAST item in the array
        // (regardless of whether we emitted any new prompts) so future reads
        // can find the resume point even if Cursor truncates.
        if let Some(t) = latest_array_text {
            new_last_prompt_text.insert(composer.workspace_id.clone(), t);
        }

        if new_prompts.is_empty() {
            continue;
        }

        let gen_timestamps = read_workspace_generation_timestamps(&ws_conn)?;
        let fallback_ts = composer.last_updated_at_ms;

        for (i, text) in new_prompts.iter().enumerate() {
            let abs_idx = skip_fallback + i;
            let ts = gen_timestamps.get(abs_idx).copied().unwrap_or(fallback_ts);
            all_msgs.push(ParsedMsg {
                msg_id: format!("{}:prompt:{}", composer.composer_id, abs_idx),
                value: serde_json::json!({
                    "sessionId": composer.composer_id,
                    "role": "user",
                    "content": text,
                    "ts": ts,
                }),
            });
        }

        new_seen_prompts.insert(
            composer.workspace_id.clone(),
            skip_fallback + new_prompts.len(),
        );
        if composer.last_updated_at_ms > new_last_updated_at_ms {
            new_last_updated_at_ms = composer.last_updated_at_ms;
        }
    }

    let records: Vec<RawRecord> = all_msgs
        .iter()
        .enumerate()
        .map(|(i, m)| RawRecord {
            tool: "cursor".to_string(),
            payload: serde_json::to_vec(&m.value).unwrap_or_default(),
            offset: i as u64 + 1,
        })
        .collect();

    let advanced = CursorCursor {
        last_updated_at_ms: new_last_updated_at_ms,
        seen_prompts: new_seen_prompts,
        last_prompt_text: new_last_prompt_text,
        project_dir: new_project_dir,
        last_msg_id: String::new(),
    };

    Ok((records, advanced))
}

// ---------------------------------------------------------------------------
// Old-schema reader (pre-migration fallback)
// ---------------------------------------------------------------------------

fn read_old_schema(
    conn: &Connection,
    since: &CursorCursor,
) -> Result<(Vec<RawRecord>, CursorCursor), AdapterError> {
    let mut raw_msgs: Vec<ParsedMsg> = Vec::new();
    raw_msgs.extend(read_generations(conn)?);
    raw_msgs.extend(read_prompts(conn)?);
    raw_msgs.extend(read_composer_data(conn)?);

    let filtered: Vec<ParsedMsg> = raw_msgs
        .into_iter()
        .filter(|m| m.msg_id > since.last_msg_id)
        .collect();

    let new_last_msg_id = filtered
        .iter()
        .map(|m| m.msg_id.as_str())
        .max()
        .map(|s| s.to_string())
        .unwrap_or_else(|| since.last_msg_id.clone());

    let records: Vec<RawRecord> = filtered
        .iter()
        .enumerate()
        .map(|(i, m)| RawRecord {
            tool: "cursor".to_string(),
            payload: serde_json::to_vec(&m.value).unwrap_or_default(),
            offset: i as u64 + 1,
        })
        .collect();

    let advanced = CursorCursor {
        last_updated_at_ms: since.last_updated_at_ms,
        seen_prompts: since.seen_prompts.clone(),
        last_prompt_text: since.last_prompt_text.clone(),
        project_dir: since.project_dir.clone(),
        last_msg_id: new_last_msg_id,
    };

    Ok((records, advanced))
}

// ---------------------------------------------------------------------------
// Old-schema key parsers
// ---------------------------------------------------------------------------

fn read_generations(conn: &Connection) -> Result<Vec<ParsedMsg>, AdapterError> {
    let text = match read_item_value(conn, "aiService.generations")? {
        None => return Ok(vec![]),
        Some(t) => t,
    };

    let arr: Vec<serde_json::Value> =
        serde_json::from_str(&text).map_err(|e| AdapterError::Parse {
            offset: 0,
            context: "aiService.generations is not a JSON array",
            source: e,
        })?;

    let mut out = Vec::with_capacity(arr.len() * 2);

    for gen in arr {
        let gen_id = gen
            .get("generationId")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let session_id = gen
            .get("sessionId")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let request_ts = gen.get("requestTs").and_then(|v| v.as_i64()).unwrap_or(0);
        let response_ts = gen.get("responseTs").and_then(|v| v.as_i64()).unwrap_or(0);

        let user_msg = gen
            .get("userMessage")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let asst_msg = gen
            .get("assistantMessage")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        out.push(ParsedMsg {
            msg_id: format!("{gen_id}:user"),
            value: serde_json::json!({
                "sessionId": session_id,
                "role": "user",
                "content": user_msg,
                "ts": request_ts,
            }),
        });
        out.push(ParsedMsg {
            msg_id: format!("{gen_id}:assistant"),
            value: serde_json::json!({
                "sessionId": session_id,
                "role": "assistant",
                "content": asst_msg,
                "ts": response_ts,
            }),
        });
    }

    Ok(out)
}

fn read_prompts(conn: &Connection) -> Result<Vec<ParsedMsg>, AdapterError> {
    let text = match read_item_value(conn, "aiService.prompts")? {
        None => return Ok(vec![]),
        Some(t) => t,
    };

    let arr: Vec<serde_json::Value> =
        serde_json::from_str(&text).map_err(|e| AdapterError::Parse {
            offset: 0,
            context: "aiService.prompts is not a JSON array",
            source: e,
        })?;

    let mut out = Vec::with_capacity(arr.len());

    for prompt in arr {
        // Old-schema prompts have a promptId field; new-schema prompts do not.
        let prompt_id = match prompt.get("promptId").and_then(|v| v.as_str()) {
            Some(id) if !id.is_empty() => id.to_string(),
            _ => continue, // new-schema prompt — skip in old fallback
        };

        let session_id = prompt
            .get("sessionId")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let ts = prompt.get("ts").and_then(|v| v.as_i64()).unwrap_or(0);

        let text_content = prompt
            .get("text")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        out.push(ParsedMsg {
            msg_id: format!("{prompt_id}:user"),
            value: serde_json::json!({
                "sessionId": session_id,
                "role": "user",
                "content": text_content,
                "ts": ts,
            }),
        });
    }

    Ok(out)
}

fn read_composer_data(conn: &Connection) -> Result<Vec<ParsedMsg>, AdapterError> {
    let text = match read_item_value(conn, "composer.composerData")? {
        None => return Ok(vec![]),
        Some(t) => t,
    };

    let obj: serde_json::Value = serde_json::from_str(&text).map_err(|e| AdapterError::Parse {
        offset: 0,
        context: "composer.composerData is not valid JSON",
        source: e,
    })?;

    let composers = match obj.get("composers").and_then(|v| v.as_array()) {
        None => return Ok(vec![]),
        Some(a) => a,
    };

    let mut out = Vec::new();

    for composer in composers {
        let composer_id = composer
            .get("composerId")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let session_id = composer
            .get("sessionId")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let messages = match composer.get("messages").and_then(|v| v.as_array()) {
            None => continue,
            Some(m) => m,
        };

        for (idx, msg) in messages.iter().enumerate() {
            let role = msg
                .get("role")
                .and_then(|v| v.as_str())
                .unwrap_or("user")
                .to_string();

            let content = match msg.get("content") {
                None => String::new(),
                Some(serde_json::Value::String(s)) => s.clone(),
                Some(other) => other.to_string(),
            };

            let ts = msg.get("ts").and_then(|v| v.as_i64()).unwrap_or(0);

            out.push(ParsedMsg {
                msg_id: format!("{composer_id}:msg:{idx}"),
                value: serde_json::json!({
                    "sessionId": session_id,
                    "role": role,
                    "content": content,
                    "ts": ts,
                }),
            });
        }
    }

    Ok(out)
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct ParsedMsg {
    msg_id: String,
    value: serde_json::Value,
}

fn attempt_open(db_path: &Path) -> Result<Connection, rusqlite::Error> {
    let conn = Connection::open_with_flags(
        db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_URI,
    )?;
    conn.busy_timeout(Duration::from_millis(200))?;
    Ok(conn)
}

fn copy_db_to_temp(db_path: &Path) -> Result<(tempfile::TempDir, PathBuf), AdapterError> {
    let tmp = build_owner_only_tempdir()?;
    let dst = tmp.path().join("state.vscdb");

    std::fs::copy(db_path, &dst).map_err(AdapterError::WalCopyFailed)?;
    chmod_owner_only(&dst)?;

    let wal = db_path.with_extension("vscdb-wal");
    if wal.exists() {
        let dst_wal = tmp.path().join("state.vscdb-wal");
        std::fs::copy(&wal, &dst_wal).map_err(AdapterError::WalCopyFailed)?;
        chmod_owner_only(&dst_wal)?;
    }
    let shm = db_path.with_extension("vscdb-shm");
    if shm.exists() {
        let dst_shm = tmp.path().join("state.vscdb-shm");
        std::fs::copy(&shm, &dst_shm).map_err(AdapterError::WalCopyFailed)?;
        chmod_owner_only(&dst_shm)?;
    }

    Ok((tmp, dst))
}

fn build_owner_only_tempdir() -> Result<tempfile::TempDir, AdapterError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()
            .map_err(AdapterError::WalCopyFailed)
    }
    #[cfg(not(unix))]
    {
        tempfile::tempdir().map_err(AdapterError::WalCopyFailed)
    }
}

fn chmod_owner_only(path: &Path) -> Result<(), AdapterError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(AdapterError::WalCopyFailed)?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

fn is_busy(err: &rusqlite::Error) -> bool {
    use rusqlite::ffi::ErrorCode;
    matches!(
        err,
        rusqlite::Error::SqliteFailure(e, _)
            if e.code == ErrorCode::DatabaseBusy || e.code == ErrorCode::SystemIoFailure
    )
}

fn open_with_fallback(
    db_path: &Path,
) -> Result<(Connection, Option<tempfile::TempDir>), AdapterError> {
    let delays = [50u64, 200, 500];

    for delay_ms in delays {
        match attempt_open(db_path) {
            Ok(conn) => return Ok((conn, None)),
            Err(e) if is_busy(&e) => {
                std::thread::sleep(Duration::from_millis(delay_ms));
            }
            Err(e) => return Err(AdapterError::Sqlite(e)),
        }
    }

    let (tmp, copied_path) = copy_db_to_temp(db_path)?;
    let conn = attempt_open(&copied_path).map_err(AdapterError::Sqlite)?;
    Ok((conn, Some(tmp)))
}

fn read_item_value(conn: &Connection, key: &str) -> Result<Option<String>, AdapterError> {
    let mut stmt = conn
        .prepare("SELECT value FROM ItemTable WHERE key = ?1")
        .map_err(AdapterError::Sqlite)?;

    let result: Option<String> = stmt
        .query_row([key], |row| {
            use rusqlite::types::ValueRef;
            let bytes: Vec<u8> = match row.get_ref(0)? {
                ValueRef::Text(t) => t.to_vec(),
                ValueRef::Blob(b) => b.to_vec(),
                ValueRef::Null => return Ok(None),
                _ => {
                    return Err(rusqlite::Error::InvalidColumnType(
                        0,
                        "value".to_string(),
                        rusqlite::types::Type::Blob,
                    ))
                }
            };
            Ok(Some(bytes))
        })
        .optional()
        .map_err(AdapterError::Sqlite)?
        .flatten()
        .map(|bytes| {
            if bytes.len() > MAX_BYTES_PER_VALUE {
                Err(bytes)
            } else {
                Ok(bytes)
            }
        })
        .transpose()
        .map_err(|_oversized| {
            let dummy_err = serde_json::from_str::<serde_json::Value>("").unwrap_err();
            AdapterError::Parse {
                offset: 0,
                context: "value exceeds size cap",
                source: dummy_err,
            }
        })?
        .map(|bytes| {
            String::from_utf8(bytes).map_err(|e| AdapterError::Parse {
                offset: 0,
                context: "ItemTable value is not valid UTF-8",
                source: serde_json::from_str::<serde_json::Value>(&e.to_string()).unwrap_err(),
            })
        })
        .transpose()?;

    Ok(result)
}

fn extract_ts(v: &serde_json::Value, offset: u64) -> Result<i64, AdapterError> {
    match v.get("ts") {
        Some(serde_json::Value::Number(n)) => n.as_i64().ok_or_else(|| AdapterError::Parse {
            offset,
            context: "ts field is not a valid i64",
            source: make_missing_field_error(),
        }),
        _ => Err(AdapterError::Parse {
            offset,
            context: "missing or non-numeric ts field",
            source: make_missing_field_error(),
        }),
    }
}

fn make_missing_field_error() -> serde_json::Error {
    serde_json::from_str::<serde_json::Value>("").unwrap_err()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::Adapter;

    const FIXTURE_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/cursor");

    // --- New schema helpers ---

    fn new_schema_adapter() -> CursorAdapter {
        CursorAdapter::with_db_root(PathBuf::from(FIXTURE_DIR).join("globalStorage"))
    }

    fn empty_cursor_new() -> CursorCursor {
        CursorCursor::default()
    }

    // --- Old schema helpers ---

    fn old_schema_adapter() -> CursorAdapter {
        CursorAdapter::with_db_root(PathBuf::from(FIXTURE_DIR).join("oldSchema"))
    }

    fn empty_cursor_old() -> CursorCursor {
        CursorCursor::default()
    }

    // -----------------------------------------------------------------------
    // New schema tests
    // -----------------------------------------------------------------------

    #[test]
    fn new_schema_parses_workspace_prompts() {
        let adapter = new_schema_adapter();
        let (records, _cursor) = adapter.read_new_records(&empty_cursor_new()).unwrap();
        let rows = adapter.parse(records).unwrap();

        // Fixture has 2 prompts in ws1 + 1 prompt in ws2 = 3 total.
        assert_eq!(rows.len(), 3, "expected 3 user rows from two workspaces");
        assert!(
            rows.iter().all(|r| r.role == "user"),
            "all rows must be user"
        );
        assert!(rows.iter().all(|r| !r.session_id.is_empty()));

        // Content spot-check
        let contents: Vec<&str> = rows.iter().map(|r| r.content.as_str()).collect();
        assert!(
            contents.contains(&"How do I implement binary search?"),
            "expected binary search prompt"
        );
        assert!(
            contents.contains(&"Explain Docker networking"),
            "expected docker prompt"
        );
    }

    #[test]
    fn new_schema_cursor_includes_project_dir() {
        let adapter = new_schema_adapter();
        let (_records, cursor) = adapter.read_new_records(&empty_cursor_new()).unwrap();

        // Most-recently-updated composer is composer-new-0001 (ts=1700300010000),
        // which lives in /synthetic/project-alpha.
        assert_eq!(
            cursor.project_dir.as_deref(),
            Some("/synthetic/project-alpha"),
            "project_dir must point to the most-recently-updated composer's fsPath"
        );
    }

    #[test]
    fn new_schema_cursor_advances_monotonically() {
        let adapter = new_schema_adapter();
        let since0 = empty_cursor_new();

        let (records1, cursor1) = adapter.read_new_records(&since0).unwrap();
        assert!(!records1.is_empty(), "first read must return records");

        // Second read: seen_prompts are updated, so no new records.
        let (records2, cursor2) = adapter.read_new_records(&cursor1).unwrap();
        assert!(
            records2.is_empty(),
            "second read with advanced cursor must return nothing"
        );
        assert_eq!(
            cursor2.seen_prompts, cursor1.seen_prompts,
            "seen_prompts must be stable when no new records"
        );
    }

    #[test]
    fn new_schema_uses_generation_timestamps() {
        let adapter = new_schema_adapter();
        let (records, _cursor) = adapter.read_new_records(&empty_cursor_new()).unwrap();
        let rows = adapter.parse(records).unwrap();

        // ws1 prompt[0] should use gen[0].unixMs = 1700300001000
        let binary_search_row = rows
            .iter()
            .find(|r| r.content == "How do I implement binary search?")
            .expect("binary search prompt must be present");
        assert_eq!(
            binary_search_row.ts, 1700300001000,
            "prompt ts should come from corresponding generation unixMs"
        );
    }

    #[test]
    fn new_schema_two_sessions() {
        let adapter = new_schema_adapter();
        let (records, _cursor) = adapter.read_new_records(&empty_cursor_new()).unwrap();
        let rows = adapter.parse(records).unwrap();

        let sessions: std::collections::HashSet<&str> =
            rows.iter().map(|r| r.session_id.as_str()).collect();
        assert_eq!(
            sessions.len(),
            2,
            "rows must span two composer sessions (ws1 and ws2)"
        );
        assert!(sessions.contains("composer-new-0001"));
        assert!(sessions.contains("composer-new-0002"));
    }

    #[test]
    fn new_schema_unsafe_workspace_id_rejected() {
        // is_safe_workspace_id must reject path-traversal strings.
        assert!(!is_safe_workspace_id("../etc/passwd"));
        assert!(!is_safe_workspace_id("ws/evil"));
        assert!(!is_safe_workspace_id(""));
        assert!(is_safe_workspace_id("wsaaa111bbb222cc"));
        assert!(is_safe_workspace_id("f206e451-ab12-cd34"));
    }

    // -----------------------------------------------------------------------
    // Old schema tests
    // -----------------------------------------------------------------------

    #[test]
    fn old_schema_parses_generations_and_prompts() {
        let adapter = old_schema_adapter();
        let since = empty_cursor_old();
        let (records, _cursor) = adapter.read_new_records(&since).unwrap();
        let rows = adapter.parse(records).unwrap();

        // Old fixture: 3 generations×2 + 3 prompts (old-style with promptId) + 2×2 composer msgs = 13
        assert!(rows.len() >= 6, "expected at least 6 rows from old schema");

        let sessions: std::collections::HashSet<&str> =
            rows.iter().map(|r| r.session_id.as_str()).collect();
        assert!(
            sessions.len() >= 2,
            "expected rows from at least 2 sessions"
        );
        assert!(sessions.contains("cursor-session-001"));
    }

    #[test]
    fn old_schema_cursor_advances_monotonically() {
        let adapter = old_schema_adapter();
        let since0 = empty_cursor_old();

        let (records1, cursor1) = adapter.read_new_records(&since0).unwrap();
        assert!(!records1.is_empty(), "first read must return records");

        let (records2, cursor2) = adapter.read_new_records(&cursor1).unwrap();
        assert!(records2.is_empty(), "second read must return nothing");
        assert_eq!(cursor2.last_msg_id, cursor1.last_msg_id);
    }

    // -----------------------------------------------------------------------
    // Shared / regression tests
    // -----------------------------------------------------------------------

    #[test]
    fn detect_returns_path_when_global_db_present() {
        let adapter = new_schema_adapter();
        let result = adapter.detect().unwrap();
        assert!(
            result.is_some(),
            "detect must return Some when state.vscdb exists"
        );
    }

    #[test]
    fn detect_returns_none_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = CursorAdapter::with_db_root(dir.path().to_path_buf());
        assert!(adapter.detect().unwrap().is_none());
    }

    #[test]
    fn read_cap_rejects_oversized_value() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("state.vscdb");

        {
            let conn = rusqlite::Connection::open(&db_path).unwrap();
            conn.execute_batch("CREATE TABLE ItemTable (key TEXT PRIMARY KEY, value BLOB);")
                .unwrap();
            let oversized: Vec<u8> = vec![b'x'; 65 * 1024 * 1024];
            conn.execute(
                "INSERT INTO ItemTable (key, value) VALUES (?1, ?2)",
                rusqlite::params!["aiService.generations", oversized],
            )
            .unwrap();
        }

        let adapter = CursorAdapter::with_db_root(dir.path().to_path_buf());
        let err = adapter.read_new_records(&empty_cursor_old()).unwrap_err();
        match err {
            AdapterError::Parse { context, .. } => {
                assert_eq!(context, "value exceeds size cap");
            }
            other => panic!("expected AdapterError::Parse, got {other:?}"),
        }
    }

    #[test]
    fn is_busy_detects_database_busy_code() {
        use rusqlite::ffi::{Error as SqliteErr, ErrorCode};
        let busy = rusqlite::Error::SqliteFailure(
            SqliteErr {
                code: ErrorCode::DatabaseBusy,
                extended_code: 5,
            },
            None,
        );
        assert!(is_busy(&busy));

        let io = rusqlite::Error::SqliteFailure(
            SqliteErr {
                code: ErrorCode::SystemIoFailure,
                extended_code: 10,
            },
            None,
        );
        assert!(is_busy(&io));

        let other = rusqlite::Error::SqliteFailure(
            SqliteErr {
                code: ErrorCode::NotADatabase,
                extended_code: 26,
            },
            None,
        );
        assert!(!is_busy(&other));
    }

    #[test]
    fn old_cursor_json_deserializes_to_new_struct() {
        // Old stored cursors have db_path/last_rowid/last_msg_id; new struct must
        // deserialize them without panicking (serde ignores unknown fields).
        let old_json = r#"{
            "db_path": "/home/user/.config/Cursor/User/globalStorage/state.vscdb",
            "last_rowid": 42,
            "last_msg_id": "gen-synthetic-0003:assistant"
        }"#;
        let cursor: CursorCursor = serde_json::from_str(old_json).expect("must deserialize");
        assert_eq!(cursor.last_msg_id, "gen-synthetic-0003:assistant");
        assert_eq!(cursor.last_updated_at_ms, 0);
        assert!(cursor.seen_prompts.is_empty());
    }
}
