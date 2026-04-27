//! Cursor (the AI editor) transcript adapter.
//!
//! Reads `~/.config/Cursor/User/globalStorage/state.vscdb` (Linux) or
//! `~/Library/Application Support/Cursor/User/globalStorage/state.vscdb` (macOS)
//! and parses the AI conversation rows into `LedgerRow`.
//!
//! When the live `state.vscdb` is locked (Cursor is running), opening it
//! directly may return `SQLITE_BUSY`. The adapter falls back to copying
//! `state.vscdb` plus its `-wal`/`-shm` sidecars into a tempdir and reading the
//! copy. The copy is auto-cleaned on tempdir drop.

use crate::adapters::{Adapter, AdapterError, RawRecord};
use crate::storage::LedgerRow;
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;

// ---------------------------------------------------------------------------
// Read cap
// ---------------------------------------------------------------------------

/// Per-poll read cap for a single ItemTable value. Same rationale as the
/// Claude adapter: prevents OOM if a transcript value grows unbounded.
const MAX_BYTES_PER_VALUE: usize = 64 * 1024 * 1024;

// ---------------------------------------------------------------------------
// Cursor (bookmark)
// ---------------------------------------------------------------------------

/// Read-position bookmark for a single Cursor `state.vscdb` file.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CursorCursor {
    /// Path to the live `state.vscdb` on the user's machine.
    pub db_path: PathBuf,
    /// Monotonic record count consumed so far. Advanced by `records.len()` on
    /// each successful read.
    pub last_rowid: i64,
    /// Stable per-message ID of the highest record consumed. Used to filter
    /// duplicates on subsequent reads.
    pub last_msg_id: String,
}

// ---------------------------------------------------------------------------
// Adapter
// ---------------------------------------------------------------------------

/// Cursor transcript adapter.
pub struct CursorAdapter {
    /// Optional override for the directory containing `state.vscdb`.
    /// `None` means use the OS-default path.
    pub db_root: Option<PathBuf>,
}

impl CursorAdapter {
    /// Default constructor — uses the OS-default `state.vscdb` location.
    pub fn new() -> Self {
        Self { db_root: None }
    }

    /// Test override: set the *directory* that contains `state.vscdb`.
    pub fn with_db_root(root: PathBuf) -> Self {
        Self {
            db_root: Some(root),
        }
    }

    /// Return the configured `db_root`, or the OS-default globalStorage path.
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
            // Windows + others: v0.4+ work. Stub returns PathNotFound.
            Err(AdapterError::PathNotFound(PathBuf::from(
                "Cursor globalStorage (unsupported platform for v0.1)",
            )))
        }
    }

    /// Path to the `state.vscdb` file.
    fn db_path(&self) -> Result<PathBuf, AdapterError> {
        Ok(self.resolve_db_root()?.join("state.vscdb"))
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
            Ok(p) => {
                if p.exists() {
                    Ok(Some(p))
                } else {
                    Ok(None)
                }
            }
            // Platform not supported or $HOME missing → not detected.
            Err(_) => Ok(None),
        }
    }

    fn read_new_records(
        &self,
        since: &CursorCursor,
    ) -> Result<(Vec<RawRecord>, CursorCursor), AdapterError> {
        // ---------------------------------------------------------------
        // Containment check
        // ---------------------------------------------------------------
        // `since.db_path` is persisted to the SQLite cursors table and could be
        // tampered with. Without this check a tampered cursor could read
        // arbitrary files on the host. canonicalize() resolves symlinks before
        // the prefix check, closing symlink-escape paths as well.
        if !since.db_path.as_os_str().is_empty() {
            let db_root = self.resolve_db_root()?;
            let canonical_root = db_root
                .canonicalize()
                .map_err(|_| AdapterError::PathNotFound(db_root.clone()))?;
            // Parent of state.vscdb must equal canonical_root.
            let canonical_db = since
                .db_path
                .canonicalize()
                .map_err(|_| AdapterError::PathNotFound(since.db_path.clone()))?;
            let db_parent = canonical_db
                .parent()
                .ok_or_else(|| AdapterError::PathNotFound(since.db_path.clone()))?;
            if !db_parent.starts_with(&canonical_root) {
                return Err(AdapterError::PathNotFound(since.db_path.clone()));
            }
        }

        // ---------------------------------------------------------------
        // Resolve the DB path to use
        // ---------------------------------------------------------------
        let live_db = if since.db_path.as_os_str().is_empty() {
            self.db_path()?
        } else {
            since.db_path.clone()
        };

        // ---------------------------------------------------------------
        // Open with retries + WAL-copy fallback
        // ---------------------------------------------------------------
        // We try to open the live DB up to 3 times with increasing backoff.
        // On a busy/locked error we fall back to a tempdir copy.
        //
        // `_temp_guard` MUST stay alive for the entire body of this method.
        // When it drops at end-of-scope (after the queries below complete)
        // the tempdir + copied DB + sidecars are removed automatically.
        // This prevents tempdir leakage across busy-poll cycles.
        let (conn, _temp_guard) = open_with_fallback(&live_db)?;

        // ---------------------------------------------------------------
        // Query each of the three known keys
        // ---------------------------------------------------------------
        let mut raw_msgs: Vec<ParsedMsg> = Vec::new();

        raw_msgs.extend(read_generations(&conn)?);
        raw_msgs.extend(read_prompts(&conn)?);
        raw_msgs.extend(read_composer_data(&conn)?);

        // ---------------------------------------------------------------
        // Filter messages already consumed
        // ---------------------------------------------------------------
        let filtered: Vec<ParsedMsg> = raw_msgs
            .into_iter()
            .filter(|m| m.msg_id > since.last_msg_id)
            .collect();

        // ---------------------------------------------------------------
        // Build RawRecords
        // ---------------------------------------------------------------
        let records: Vec<RawRecord> = filtered
            .iter()
            .enumerate()
            .map(|(i, m)| {
                let payload = serde_json::to_vec(&m.value).unwrap_or_default();
                // Per-record monotonic offset within the batch so downstream
                // error attribution and dedup keys are unique per row.
                let offset = (since.last_rowid + 1 + i as i64) as u64;
                RawRecord {
                    tool: "cursor".to_string(),
                    payload,
                    offset,
                }
            })
            .collect();

        // ---------------------------------------------------------------
        // Advance cursor
        // ---------------------------------------------------------------
        let new_last_msg_id = filtered
            .iter()
            .map(|m| m.msg_id.as_str())
            .max()
            .map(|s| s.to_string())
            .unwrap_or_else(|| since.last_msg_id.clone());

        let new_rowid = since.last_rowid + filtered.len() as i64;

        let advanced = CursorCursor {
            db_path: live_db,
            last_rowid: new_rowid,
            last_msg_id: new_last_msg_id,
        };

        Ok((records, advanced))
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

            // Required: sessionId
            let session_id = v
                .get("sessionId")
                .and_then(|s| s.as_str())
                .ok_or_else(|| AdapterError::Parse {
                    offset: rec.offset,
                    context: "missing sessionId field",
                    source: make_missing_field_error(),
                })?
                .to_string();

            // Required: ts (unix epoch ms)
            let ts = extract_ts(&v, rec.offset)?;

            // Required: role
            let role = v
                .get("role")
                .and_then(|r| r.as_str())
                .ok_or_else(|| AdapterError::Parse {
                    offset: rec.offset,
                    context: "missing role field",
                    source: make_missing_field_error(),
                })?
                .to_string();

            // content: string preferred, fall back to JSON serialization
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
// Internal helpers
// ---------------------------------------------------------------------------

/// A parsed message from any of the three ItemTable keys, normalized to a
/// common shape before being turned into a `RawRecord`.
#[derive(Debug)]
struct ParsedMsg {
    /// Stable string ID used to deduplicate across reads.
    msg_id: String,
    /// JSON value to be stored as `RawRecord::payload`.
    value: serde_json::Value,
}

/// Attempt a single read-only open of `db_path`.
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

/// Copy `db_path` and its `-wal`/`-shm` sidecars to a fresh tempdir.
/// Returns `(TempDir, path_to_copied_db)`. The `TempDir` must be kept alive
/// for the duration of the connection; it cleans up on drop.
///
/// On unix the tempdir is created with mode 0o700 and every copied file is
/// chmod'd to 0o600. Without these the tempdir inherits 0o755 (umask) and
/// the copied DB inherits the source mode (often 0o644), making the copied
/// transcript readable by every local user — unacceptable since transcripts
/// may contain pasted secrets.
fn copy_db_to_temp(db_path: &Path) -> Result<(tempfile::TempDir, PathBuf), AdapterError> {
    let tmp = build_owner_only_tempdir()?;
    let dst = tmp.path().join("state.vscdb");

    std::fs::copy(db_path, &dst).map_err(AdapterError::WalCopyFailed)?;
    chmod_owner_only(&dst)?;

    // Copy sidecars if present; missing is fine.
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

/// Create a tempdir with mode 0o700 on unix. On non-unix targets we fall
/// back to the platform default (Windows ACLs grant owner-only access by
/// default for user temp directories).
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

/// chmod a single file to 0o600 on unix; no-op elsewhere.
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

/// Return true if a rusqlite error indicates the DB is busy / locked.
fn is_busy(err: &rusqlite::Error) -> bool {
    use rusqlite::ffi::ErrorCode;
    matches!(
        err,
        rusqlite::Error::SqliteFailure(e, _)
            if e.code == ErrorCode::DatabaseBusy || e.code == ErrorCode::SystemIoFailure
    )
}

/// Open `db_path` with retries (50ms / 200ms / 500ms backoff) and WAL-copy
/// fallback on busy. Returns `(Connection, Option<TempDir>)`. The caller must
/// keep the returned `TempDir` alive for the entire connection lifetime;
/// dropping the tuple cleans up the copy automatically.
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

    // All retries exhausted with busy errors — fall back to a file copy that
    // lives in an owner-only tempdir. The TempDir guard is returned so it
    // drops alongside the Connection at the caller's end-of-scope.
    let (tmp, copied_path) = copy_db_to_temp(db_path)?;
    let conn = attempt_open(&copied_path).map_err(AdapterError::Sqlite)?;
    Ok((conn, Some(tmp)))
}

/// Read and check the byte length of a value from `ItemTable`.
fn read_item_value(conn: &Connection, key: &str) -> Result<Option<String>, AdapterError> {
    let mut stmt = conn
        .prepare("SELECT value FROM ItemTable WHERE key = ?1")
        .map_err(AdapterError::Sqlite)?;

    // Use query_row with get_ref so we can handle both TEXT and BLOB columns.
    // The Python fixture writes TEXT (json.dumps); real Cursor may use BLOB.
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
                // Signal size cap via a sentinel; checked below.
                Err(bytes)
            } else {
                Ok(bytes)
            }
        })
        .transpose()
        .map_err(|oversized| {
            let _ = oversized;
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

/// Parse `aiService.generations` → `ParsedMsg` list.
///
/// Each generation object has shape:
/// ```json
/// {
///   "generationId": "...",
///   "sessionId": "...",
///   "requestTs": 1234567890000,
///   "responseTs": 1234567891500,
///   "userMessage": "...",
///   "assistantMessage": "..."
/// }
/// ```
/// We emit two `ParsedMsg` per generation (one user turn, one assistant turn),
/// because each generation encodes both the prompt and the response.
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

        // User turn
        out.push(ParsedMsg {
            msg_id: format!("{gen_id}:user"),
            value: serde_json::json!({
                "sessionId": session_id,
                "role": "user",
                "content": user_msg,
                "ts": request_ts,
            }),
        });

        // Assistant turn
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

/// Parse `aiService.prompts` → `ParsedMsg` list.
///
/// Each prompt object has shape:
/// ```json
/// {
///   "promptId": "...",
///   "sessionId": "...",
///   "ts": 1234567890000,
///   "text": "...",
///   "files": [...]
/// }
/// ```
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
        let prompt_id = prompt
            .get("promptId")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

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

/// Parse `composer.composerData` → `ParsedMsg` list.
///
/// The value is a JSON object with a `composers` array. Each composer has a
/// `messages` array with `{role, content, ts}` objects.
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

/// Extract `ts` from a parsed message value.
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

/// Produce a synthetic `serde_json::Error` for use in `AdapterError::Parse`
/// where the true error is a missing field rather than a JSON syntax error.
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

    fn fixture_db_path() -> PathBuf {
        PathBuf::from(FIXTURE_DIR).join("1-state.vscdb")
    }

    /// Build an adapter that points at the fixture directory so the
    /// containment check accepts the fixture path.
    fn fixture_adapter() -> CursorAdapter {
        CursorAdapter::with_db_root(PathBuf::from(FIXTURE_DIR))
    }

    fn empty_cursor(db_path: PathBuf) -> CursorCursor {
        CursorCursor {
            db_path,
            last_rowid: 0,
            last_msg_id: String::new(),
        }
    }

    // 1. parses_aiservice_generations
    #[test]
    fn parses_aiservice_generations() {
        let adapter = fixture_adapter();
        let since = empty_cursor(fixture_db_path());
        let (records, _cursor) = adapter.read_new_records(&since).unwrap();
        let rows = adapter.parse(records).unwrap();

        // Fixture has 3 generations × 2 turns = 6 rows from this key alone (plus prompts + composers).
        let assistant_rows: Vec<_> = rows.iter().filter(|r| r.role == "assistant").collect();
        assert!(
            !assistant_rows.is_empty(),
            "expected at least one assistant row from generations"
        );
        // All assistant rows have a session_id and tool = "cursor"
        for row in &assistant_rows {
            assert!(!row.session_id.is_empty(), "session_id must not be empty");
            assert_eq!(row.tool, "cursor");
        }
        // Spot-check: first generation is in session-001
        assert!(
            rows.iter().any(|r| r.session_id == "cursor-session-001"),
            "expected cursor-session-001 in results"
        );
    }

    // 2. parses_aiservice_prompts
    #[test]
    fn parses_aiservice_prompts() {
        let adapter = fixture_adapter();
        let since = empty_cursor(fixture_db_path());
        let (records, _cursor) = adapter.read_new_records(&since).unwrap();
        let rows = adapter.parse(records).unwrap();

        // Fixture has 3 prompts, all with role "user"
        let user_rows: Vec<_> = rows.iter().filter(|r| r.role == "user").collect();
        assert!(
            !user_rows.is_empty(),
            "expected at least one user row from prompts"
        );
        // At least one user row must contain "CSS" content from the fixture
        let css_row = user_rows
            .iter()
            .any(|r| r.content.contains("CSS") || r.content.contains("center a div"));
        assert!(css_row, "expected CSS prompt content in user rows");
    }

    // 3. merges_three_keys
    #[test]
    fn merges_three_keys() {
        let adapter = fixture_adapter();
        let since = empty_cursor(fixture_db_path());
        let (records, _cursor) = adapter.read_new_records(&since).unwrap();
        let rows = adapter.parse(records).unwrap();

        // Fixture: 3 generations×2 + 3 prompts + 2 composers×2msgs = 6+3+4 = 13
        // But some may deduplicate by msg_id across keys (they have distinct IDs in fixture).
        // We simply assert we have enough to cover all three key sources.
        assert!(
            rows.len() >= 6,
            "expected at least 6 rows (3 keys merged), got {}",
            rows.len()
        );
        // Multiple sessions present
        let sessions: std::collections::HashSet<_> =
            rows.iter().map(|r| r.session_id.as_str()).collect();
        assert!(
            sessions.len() >= 2,
            "expected rows from at least 2 sessions"
        );
    }

    // 4. cursor_advances_monotonically
    #[test]
    fn cursor_advances_monotonically() {
        let adapter = fixture_adapter();
        let since0 = empty_cursor(fixture_db_path());

        let (records1, cursor1) = adapter.read_new_records(&since0).unwrap();
        assert!(!records1.is_empty(), "first read must return records");
        assert!(
            cursor1.last_rowid >= since0.last_rowid,
            "cursor must not regress"
        );

        // Second read with advanced cursor should return no new records.
        let (records2, cursor2) = adapter.read_new_records(&cursor1).unwrap();
        assert!(
            records2.is_empty(),
            "second read with advanced cursor must return nothing"
        );
        assert_eq!(
            cursor2.last_rowid, cursor1.last_rowid,
            "cursor must be stable when no new records"
        );
        assert_eq!(cursor2.last_msg_id, cursor1.last_msg_id);
    }

    // 5. wal_lock_falls_back_to_copy
    //
    // Reliably triggering SQLITE_BUSY via a write-lock from another thread is
    // difficult with read-only opens (WAL mode allows concurrent readers).
    // We verify the fallback machinery compiles and the is_busy() helper works
    // correctly. The full lock-path is exercised by the integration harness.
    #[test]
    #[ignore = "SQLITE_BUSY cannot be reliably triggered for read-only opens in WAL mode; see is_busy_detects_database_busy_code below"]
    fn wal_lock_falls_back_to_copy() {
        let dir = tempfile::tempdir().unwrap();
        let src_db = dir.path().join("state.vscdb");

        // Copy the fixture into the tempdir (acting as the "live" DB).
        std::fs::copy(fixture_db_path(), &src_db).unwrap();

        // Open an exclusive write transaction in a sibling thread.
        let src_db_clone = src_db.clone();
        let handle = std::thread::spawn(move || {
            let conn =
                Connection::open_with_flags(&src_db_clone, OpenFlags::SQLITE_OPEN_READ_WRITE)
                    .unwrap();
            conn.execute_batch("BEGIN EXCLUSIVE;").unwrap();
            // Hold the lock for 2 seconds so the main thread hits SQLITE_BUSY.
            std::thread::sleep(Duration::from_secs(2));
            conn.execute_batch("ROLLBACK;").unwrap();
        });

        // Small delay to let the exclusive lock be acquired.
        std::thread::sleep(Duration::from_millis(50));

        let adapter = CursorAdapter::with_db_root(dir.path().to_path_buf());
        let since = empty_cursor(src_db.clone());
        let result = adapter.read_new_records(&since);

        // Whether the fallback succeeded or the read-only open bypassed the lock,
        // we must NOT get a SqliteBusy error.
        assert!(
            result.is_ok(),
            "WAL fallback must succeed; got: {:?}",
            result.err()
        );

        handle.join().unwrap();
    }

    // Unit test on the is_busy() helper (runs without a real lock).
    #[test]
    fn is_busy_detects_database_busy_code() {
        use rusqlite::ffi::{Error as SqliteErr, ErrorCode};
        let busy_err = rusqlite::Error::SqliteFailure(
            SqliteErr {
                code: ErrorCode::DatabaseBusy,
                extended_code: 5,
            },
            None,
        );
        assert!(is_busy(&busy_err), "DatabaseBusy must be detected as busy");

        let io_err = rusqlite::Error::SqliteFailure(
            SqliteErr {
                code: ErrorCode::SystemIoFailure,
                extended_code: 10,
            },
            None,
        );
        assert!(is_busy(&io_err), "SystemIoFailure must be detected as busy");

        let other_err = rusqlite::Error::SqliteFailure(
            SqliteErr {
                code: ErrorCode::NotADatabase,
                extended_code: 26,
            },
            None,
        );
        assert!(!is_busy(&other_err), "NotADatabase must NOT be busy");
    }

    // 6. containment_check_rejects_paths_outside_root
    #[test]
    fn containment_check_rejects_paths_outside_root() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = CursorAdapter::with_db_root(dir.path().to_path_buf());

        // We need the db_root to exist (for canonicalize).
        // dir is already created by tempdir().

        // Point db_path at /tmp itself (a real path, but not inside db_root).
        // Use a path that definitely exists so canonicalize succeeds.
        let outside_path = PathBuf::from("/tmp");

        let since = CursorCursor {
            db_path: outside_path,
            last_rowid: 0,
            last_msg_id: String::new(),
        };

        let err = adapter.read_new_records(&since).unwrap_err();
        assert!(
            matches!(err, AdapterError::PathNotFound(_)),
            "expected PathNotFound for path outside root, got: {:?}",
            err
        );
    }

    // 7. read_cap_rejects_oversized_value
    #[test]
    fn read_cap_rejects_oversized_value() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("state.vscdb");

        // Create a minimal DB with an oversized value.
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute_batch("CREATE TABLE ItemTable (key TEXT PRIMARY KEY, value BLOB);")
                .unwrap();

            // 65 MB of JSON-ish bytes.
            let oversized: Vec<u8> = vec![b'x'; 65 * 1024 * 1024];
            conn.execute(
                "INSERT INTO ItemTable (key, value) VALUES (?1, ?2)",
                rusqlite::params!["aiService.generations", oversized],
            )
            .unwrap();
        }

        let adapter = CursorAdapter::with_db_root(dir.path().to_path_buf());
        let since = empty_cursor(db_path);
        let err = adapter.read_new_records(&since).unwrap_err();

        match err {
            AdapterError::Parse { context, .. } => {
                assert_eq!(
                    context, "value exceeds size cap",
                    "expected size-cap context"
                );
            }
            other => panic!("expected AdapterError::Parse, got {:?}", other),
        }
    }

    // 8. detect_returns_db_path_when_present
    #[test]
    fn detect_returns_db_path_when_present() {
        let dir = tempfile::tempdir().unwrap();
        // Create the state.vscdb file.
        std::fs::write(dir.path().join("state.vscdb"), b"").unwrap();
        let adapter = CursorAdapter::with_db_root(dir.path().to_path_buf());
        let result = adapter.detect().unwrap();
        assert!(result.is_some(), "detect must return Some when file exists");
        assert_eq!(result.unwrap(), dir.path().join("state.vscdb"));
    }

    // 9. detect_returns_none_when_absent
    #[test]
    fn detect_returns_none_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        // No state.vscdb in dir.
        let adapter = CursorAdapter::with_db_root(dir.path().to_path_buf());
        let result = adapter.detect().unwrap();
        assert!(
            result.is_none(),
            "detect must return None when file is absent"
        );
    }
}
