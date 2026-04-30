//! Codex CLI transcript adapter.
//!
//! Reads JSONL files at ~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl and
//! parses them into LedgerRow. Each session normally begins with a
//! `session_meta` line followed by `event_msg` lines. When session_meta is
//! absent the adapter best-effort-parses the event_msg rows; the daemon
//! event log is the right place to surface that condition (v0.2).

use crate::adapters::{Adapter, AdapterError, RawRecord};
use crate::storage::LedgerRow;
use serde::{Deserialize, Serialize};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Read cap
// ---------------------------------------------------------------------------

/// Per-poll read cap. A single `read_new_records` call will never allocate
/// more than this many bytes of transcript into RAM, even if the file has
/// grown by gigabytes since the last cursor advance.
const MAX_READ_BYTES_PER_POLL: u64 = 64 * 1024 * 1024;

// ---------------------------------------------------------------------------
// Cursor
// ---------------------------------------------------------------------------

/// Read-position cursor for a single Codex transcript file.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CodexCursor {
    /// Path to the JSONL transcript file (rollout file).
    pub file_path: PathBuf,
    /// Byte offset of the first byte NOT yet consumed in the rollout file.
    pub byte_offset: u64,
    /// Highest `seq` value seen in `event_msg` rows so far.
    pub last_event_seq: i64,
    /// Byte offset of the first byte NOT yet consumed in `~/.codex/history.jsonl`.
    /// Codex appends user prompts here in real-time, before the rollout file
    /// gets flushed — reading this catches prompts the user just submitted.
    #[serde(default)]
    pub history_offset: u64,
    /// Project directory of the active Codex session. Read from the rollout
    /// file's `session_meta.payload.cwd` so fs-watcher events can route the
    /// handoff back to the correct project.
    #[serde(default)]
    pub project_dir: Option<String>,
}

// ---------------------------------------------------------------------------
// Adapter
// ---------------------------------------------------------------------------

pub struct CodexAdapter {
    /// Optional override for the sessions root, useful in tests. Production
    /// uses the default ~/.codex/sessions/.
    pub sessions_root: Option<PathBuf>,
}

impl CodexAdapter {
    pub fn new() -> Self {
        Self {
            sessions_root: None,
        }
    }

    /// For tests: override the root to point at a fixture directory or any
    /// caller-supplied path.
    pub fn with_sessions_root(root: PathBuf) -> Self {
        Self {
            sessions_root: Some(root),
        }
    }

    fn sessions_root(&self) -> Result<PathBuf, AdapterError> {
        if let Some(p) = &self.sessions_root {
            return Ok(p.clone());
        }
        let home =
            dirs::home_dir().ok_or_else(|| AdapterError::PathNotFound(PathBuf::from("$HOME")))?;
        Ok(home.join(".codex").join("sessions"))
    }
}

impl Default for CodexAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl Adapter for CodexAdapter {
    type Cursor = CodexCursor;

    fn name(&self) -> &'static str {
        "codex"
    }

    /// Detect by checking whether the sessions directory exists.
    fn detect(&self) -> Result<Option<PathBuf>, AdapterError> {
        let root = self.sessions_root()?;
        if root.exists() {
            Ok(Some(root))
        } else {
            Ok(None)
        }
    }

    /// Read new complete lines from the cursor's file starting at byte_offset.
    ///
    /// A partial line at the tail (no trailing `\n`) is NOT consumed — the
    /// adapter emits `AdapterError::PartialJsonl` with the offset of the
    /// partial line's first byte. If the file ends exactly on a newline
    /// boundary (or is empty past the cursor), returns normally with an
    /// empty-or-populated vec and an advanced cursor.
    fn read_new_records(
        &self,
        since: &Self::Cursor,
    ) -> Result<(Vec<RawRecord>, Self::Cursor), AdapterError> {
        let file_path = &since.file_path;

        // Empty path means nothing has been configured yet — return empty.
        if file_path.as_os_str().is_empty() {
            return Ok((
                vec![],
                CodexCursor {
                    file_path: file_path.clone(),
                    byte_offset: 0,
                    last_event_seq: since.last_event_seq,
                    history_offset: since.history_offset,
                    project_dir: since.project_dir.clone(),
                },
            ));
        }

        // Containment + symlink guard: `cursor.file_path` is persisted to the
        // SQLite cursors table and could be tampered with. Without this check
        // a tampered cursor turns the adapter into an arbitrary-file-read
        // primitive (e.g. /etc/shadow, ~/.ssh/id_rsa). canonicalize() resolves
        // symlinks before the prefix check, closing symlink-escape as well.
        let root = self.sessions_root()?;
        let canonical_root = root
            .canonicalize()
            .map_err(|_| AdapterError::PathNotFound(root.clone()))?;
        let canonical_file = file_path
            .canonicalize()
            .map_err(|_| AdapterError::PathNotFound(file_path.clone()))?;
        if !canonical_file.starts_with(&canonical_root) {
            return Err(AdapterError::PathNotFound(file_path.clone()));
        }

        let (line_records, new_offset) =
            read_complete_lines(file_path, since.byte_offset, self.name())?;

        let mut last_event_seq = since.last_event_seq;

        // Codex transcripts begin with a session_meta line that carries the
        // session_id. When reading incrementally (offset > 0) we skip past it
        // and parse() loses session context. Inject a synthetic session_meta
        // record at the top of the batch ONLY when there are real records to
        // process — never inject for an empty batch (preserves idempotency).
        let mut records: Vec<RawRecord> = Vec::with_capacity(line_records.len() + 1);
        if since.byte_offset > 0 && !line_records.is_empty() {
            if let Some(meta_line) = peek_first_line(file_path) {
                records.push(RawRecord {
                    tool: self.name().to_string(),
                    payload: meta_line.into_bytes(),
                    offset: 0,
                });
            }
        }

        records.extend(line_records.into_iter().map(|(end_offset, bytes)| {
            // Track last_event_seq from event_msg rows as they come in.
            if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                if v.get("type").and_then(|t| t.as_str()) == Some("event_msg") {
                    if let Some(seq) = v.get("seq").and_then(|s| s.as_i64()) {
                        if seq > last_event_seq {
                            last_event_seq = seq;
                        }
                    }
                }
            }
            RawRecord {
                tool: self.name().to_string(),
                payload: bytes,
                offset: end_offset,
            }
        }));

        // Also read ~/.codex/history.jsonl for real-time user prompts. Codex
        // appends to this file on every prompt submission, BEFORE the rollout
        // file is flushed. Without this, recent prompts are missed.
        // Skip in tests: when sessions_root is overridden, the adapter is
        // pointing at a fixture and shouldn't read the dev machine's history.
        let mut new_history_offset = since.history_offset;
        if self.sessions_root.is_none() {
            if let Some(home) = dirs::home_dir() {
                let history_path = home.join(".codex").join("history.jsonl");
                if history_path.exists() {
                    if let Ok((hist_lines, advanced)) =
                        read_complete_lines(&history_path, since.history_offset, self.name())
                    {
                        new_history_offset = advanced;
                        for (end_offset, bytes) in hist_lines {
                            // Wrap in a marker so parse() knows it's history-format.
                            // Format: {"_codex_history": true, "line": <original>}
                            let mut wrapped = b"{\"_codex_history\":true,\"line\":".to_vec();
                            // Escape the original bytes as JSON string content.
                            let original = String::from_utf8_lossy(&bytes);
                            let escaped = serde_json::to_string(&original.trim())
                                .unwrap_or_else(|_| "\"\"".to_string());
                            wrapped.extend_from_slice(escaped.as_bytes());
                            wrapped.push(b'}');
                            records.push(RawRecord {
                                tool: self.name().to_string(),
                                payload: wrapped,
                                offset: end_offset,
                            });
                        }
                    }
                }
            }
        }

        // Peek the rollout's session_meta line for cwd → routes handoff to
        // the correct project on subsequent fs-watcher events.
        let new_project_dir = peek_session_cwd(file_path).or(since.project_dir.clone());

        let advanced = CodexCursor {
            file_path: file_path.clone(),
            byte_offset: new_offset,
            last_event_seq,
            history_offset: new_history_offset,
            project_dir: new_project_dir,
        };

        Ok((records, advanced))
    }

    /// Parse raw records into LedgerRow.
    ///
    /// `session_meta` lines are consumed for context (session_id capture) but
    /// do not produce a LedgerRow. `event_msg` lines produce one LedgerRow
    /// each. Unknown record types are silently skipped (defensive: future
    /// Codex versions may add new types).
    ///
    /// If no `session_meta` is present (fixture #2), the adapter
    /// best-effort-parses the event_msg rows using the session_id embedded in
    /// each event_msg itself.
    /// Missing session_meta is recorded in the daemon event log (v0.2).
    fn parse(&self, records: Vec<RawRecord>) -> Result<Vec<LedgerRow>, AdapterError> {
        let mut rows = Vec::with_capacity(records.len());
        // Captured from session_meta.payload.id; applied to subsequent event_msg
        // lines that don't carry session_id at the top level (new Codex schema).
        let mut current_session_id: String = String::new();

        for rec in records {
            let v: serde_json::Value = match serde_json::from_slice(&rec.payload) {
                Ok(v) => v,
                Err(_) => continue, // skip corrupt JSON line
            };

            // History-format wrapper: {"_codex_history": true, "line": "<json>"}
            // Each line is `{session_id, ts, text}` from ~/.codex/history.jsonl.
            if v.get("_codex_history").and_then(|b| b.as_bool()) == Some(true) {
                let inner_str = v.get("line").and_then(|l| l.as_str()).unwrap_or("");
                if inner_str.is_empty() {
                    continue;
                }
                let inner: serde_json::Value = match serde_json::from_str(inner_str) {
                    Ok(x) => x,
                    Err(_) => continue,
                };
                let sid = inner
                    .get("session_id")
                    .and_then(|s| s.as_str())
                    .unwrap_or("")
                    .to_string();
                let ts_secs = inner.get("ts").and_then(|t| t.as_i64()).unwrap_or(0);
                let text = inner
                    .get("text")
                    .and_then(|t| t.as_str())
                    .unwrap_or("")
                    .to_string();
                if sid.is_empty() || text.is_empty() {
                    continue;
                }
                rows.push(LedgerRow {
                    session_id: sid,
                    tool: "codex".to_string(),
                    ts: ts_secs * 1000, // history.jsonl uses unix seconds
                    role: "user".to_string(),
                    content: text,
                    tool_calls_json: None,
                    files_touched_json: None,
                    parent_id: None,
                });
                continue;
            }

            let row_type = v.get("type").and_then(|t| t.as_str()).unwrap_or("");

            match row_type {
                "session_meta" => {
                    // New schema: payload.id is the session id; old schema may put
                    // it at top-level "session_id".
                    if let Some(sid) = v
                        .get("payload")
                        .and_then(|p| p.get("id"))
                        .and_then(|i| i.as_str())
                        .or_else(|| v.get("session_id").and_then(|s| s.as_str()))
                    {
                        current_session_id = sid.to_string();
                    }
                    continue;
                }
                "event_msg" => {
                    // Try new schema first: payload.{type, message}
                    let payload = v.get("payload");
                    let payload_type = payload
                        .and_then(|p| p.get("type"))
                        .and_then(|t| t.as_str())
                        .unwrap_or("");
                    let role_from_payload = match payload_type {
                        "user_message" => Some("user"),
                        "agent_message" => Some("assistant"),
                        _ => None,
                    };

                    let session_id_top = v.get("session_id").and_then(|s| s.as_str()).unwrap_or("");

                    if let Some(role) = role_from_payload {
                        // New schema path
                        let session_id = if !session_id_top.is_empty() {
                            session_id_top.to_string()
                        } else if !current_session_id.is_empty() {
                            current_session_id.clone()
                        } else {
                            continue; // can't tag a row without a session
                        };
                        let content = payload
                            .and_then(|p| p.get("message"))
                            .and_then(|m| m.as_str())
                            .unwrap_or("")
                            .to_string();
                        if content.is_empty() {
                            continue;
                        }
                        let ts = match parse_iso_timestamp(v.get("timestamp")) {
                            Some(t) => t,
                            None => match parse_timestamp(v.get("ts"), rec.offset) {
                                Ok(t) => t,
                                Err(_) => continue,
                            },
                        };
                        rows.push(LedgerRow {
                            session_id,
                            tool: "codex".to_string(),
                            ts,
                            role: role.to_string(),
                            content,
                            tool_calls_json: None,
                            files_touched_json: None,
                            parent_id: None,
                        });
                        continue;
                    }

                    // Old schema fallback: top-level role/content/session_id
                    let session_id = if !session_id_top.is_empty() {
                        session_id_top.to_string()
                    } else if !current_session_id.is_empty() {
                        current_session_id.clone()
                    } else {
                        continue;
                    };
                    let ts = match parse_timestamp(v.get("ts"), rec.offset) {
                        Ok(t) => t,
                        Err(_) => continue,
                    };
                    let role = match v.get("role").and_then(|r| r.as_str()) {
                        Some(r) => r.to_string(),
                        None => continue,
                    };
                    let content = match v.get("content") {
                        None => String::new(),
                        Some(serde_json::Value::String(s)) => s.clone(),
                        Some(arr) => match serde_json::to_string(arr) {
                            Ok(s) => s,
                            Err(_) => continue,
                        },
                    };
                    let tool_calls_val = v.get("tool_calls");
                    let tool_calls_json = extract_tool_calls(tool_calls_val);
                    let files_touched_json = extract_files_touched(tool_calls_val);
                    rows.push(LedgerRow {
                        session_id,
                        tool: "codex".to_string(),
                        ts,
                        role,
                        content,
                        tool_calls_json,
                        files_touched_json,
                        parent_id: None,
                    });
                }
                _ => continue,
            }
        }

        Ok(rows)
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// `(end_offset_exclusive, line_bytes_including_newline)` pairs from a file read.
type LineRecords = Vec<(u64, Vec<u8>)>;

/// Read all complete (newline-terminated) lines from `file_path` starting at
/// `from_offset`. Returns `(records, new_offset)` where each record is
/// `(end_offset_exclusive, line_bytes_including_newline)`.
///
/// If a partial line (bytes without a trailing `\n`) is detected at the tail,
/// returns `AdapterError::PartialJsonl` with the offset of the partial line's
/// first byte. The partial bytes are not included in records.
fn read_complete_lines(
    file_path: &Path,
    from_offset: u64,
    tool: &str,
) -> Result<(LineRecords, u64), AdapterError> {
    let mut file = std::fs::File::open(file_path)?;
    file.seek(SeekFrom::Start(from_offset))?;

    let mut buf = Vec::new();
    file.take(MAX_READ_BYTES_PER_POLL).read_to_end(&mut buf)?;

    if buf.is_empty() {
        return Ok((vec![], from_offset));
    }

    let mut records = Vec::new();
    let mut cursor = 0usize;
    let mut byte_pos = from_offset;

    while cursor < buf.len() {
        match buf[cursor..].iter().position(|&b| b == b'\n') {
            Some(nl_rel) => {
                let line_end = cursor + nl_rel + 1; // include the '\n'
                let line = buf[cursor..line_end].to_vec();
                byte_pos += line.len() as u64;
                records.push((byte_pos, line));
                cursor = line_end;
            }
            None => {
                // Remaining bytes form a partial line — no trailing newline.
                // Use a synthetic serde_json::Error rather than calling
                // from_slice on the partial bytes: the offset is the
                // load-bearing diagnostic, and feeding record content
                // through serde_json risks payload bytes appearing in
                // {:?} output of some serde_json versions. The contract
                // on AdapterError::Parse forbids transcript content in
                // error chains; we apply the same discipline here.
                let partial_offset = from_offset + cursor as u64;
                // tool is available for future structured logging.
                let _ = tool;
                return Err(AdapterError::PartialJsonl {
                    offset: partial_offset,
                    source: make_missing_field_error(),
                });
            }
        }
    }

    Ok((records, byte_pos))
}

/// Parse the `ts` field. Fixtures use unix epoch milliseconds (i64).
fn parse_timestamp(val: Option<&serde_json::Value>, offset: u64) -> Result<i64, AdapterError> {
    match val {
        Some(serde_json::Value::Number(n)) => n.as_i64().ok_or_else(|| AdapterError::Parse {
            offset,
            context: "ts field is not a valid i64",
            source: make_missing_field_error(),
        }),
        Some(serde_json::Value::String(s)) => s.parse::<i64>().map_err(|_| AdapterError::Parse {
            offset,
            context: "ts field string is not parseable as i64",
            source: make_missing_field_error(),
        }),
        _ => Err(AdapterError::Parse {
            offset,
            context: "missing or non-numeric ts field",
            source: make_missing_field_error(),
        }),
    }
}

/// Read just the first line of a file (for session_meta peeking).
fn peek_first_line(path: &Path) -> Option<String> {
    use std::io::{BufRead, BufReader};
    let f = std::fs::File::open(path).ok()?;
    let mut reader = BufReader::new(f);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    let trimmed = line.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Read the first line of a Codex rollout file and extract `payload.cwd`
/// from `session_meta`. Returns None if the file is missing/empty/malformed.
fn peek_session_cwd(path: &Path) -> Option<String> {
    use std::io::{BufRead, BufReader};
    let f = std::fs::File::open(path).ok()?;
    let mut reader = BufReader::new(f);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    let v: serde_json::Value = serde_json::from_str(line.trim()).ok()?;
    if v.get("type").and_then(|t| t.as_str()) != Some("session_meta") {
        return None;
    }
    v.get("payload")
        .and_then(|p| p.get("cwd"))
        .and_then(|c| c.as_str())
        .map(String::from)
}

/// Parse an ISO-8601 timestamp string (e.g. "2026-04-29T12:11:52.589Z") into
/// unix epoch milliseconds. Returns None if the value is missing or unparseable.
fn parse_iso_timestamp(val: Option<&serde_json::Value>) -> Option<i64> {
    let s = val.and_then(|v| v.as_str())?;
    let dt = chrono::DateTime::parse_from_rfc3339(s).ok()?;
    Some(dt.timestamp_millis())
}

/// Extract `tool_calls` array if present, returning JSON-serialized form.
fn extract_tool_calls(tool_calls: Option<&serde_json::Value>) -> Option<String> {
    let arr = tool_calls?.as_array()?;
    if arr.is_empty() {
        return None;
    }
    serde_json::to_string(arr).ok()
}

/// Extract file paths from `tool_calls[*].input.path` fields.
/// Returns JSON-serialized array of path strings, or None if none found.
fn extract_files_touched(tool_calls: Option<&serde_json::Value>) -> Option<String> {
    let arr = tool_calls?.as_array()?;
    let paths: Vec<&str> = arr
        .iter()
        .filter_map(|item| {
            item.get("input")
                .and_then(|inp| inp.get("path"))
                .and_then(|p| p.as_str())
        })
        .collect();

    if paths.is_empty() {
        return None;
    }

    serde_json::to_string(&paths).ok()
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

    fn fixture_path(name: &str) -> PathBuf {
        PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/codex")).join(name)
    }

    fn fixture_root() -> PathBuf {
        PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/codex"))
    }

    /// Build an adapter whose `sessions_root` is the fixture directory, so
    /// the containment check in `read_new_records` accepts cursor paths to
    /// the fixture files.
    fn adapter() -> CodexAdapter {
        CodexAdapter::with_sessions_root(fixture_root())
    }

    fn cursor_for(name: &str) -> CodexCursor {
        CodexCursor {
            file_path: fixture_path(name),
            byte_offset: 0,
            last_event_seq: 0,
            history_offset: 0,
            project_dir: None,
        }
    }

    // 1. Happy-path: session_meta + 3 event_msg → 3 LedgerRows.
    #[test]
    fn parses_simple_session() {
        let a = adapter();
        let (records, _cursor) = a
            .read_new_records(&cursor_for("1-simple-session.jsonl"))
            .unwrap();
        let rows = a.parse(records).unwrap();

        assert_eq!(rows.len(), 3, "expected 3 LedgerRows");
        assert_eq!(rows[0].role, "user");
        assert_eq!(rows[1].role, "assistant");
        assert_eq!(rows[2].role, "user");

        for row in &rows {
            assert_eq!(row.session_id, "codex-session-001");
            assert_eq!(row.tool, "codex");
        }
    }

    // 2. Missing session_meta: best-effort parse; session_id from event rows.
    #[test]
    fn missing_session_meta_succeeds_best_effort() {
        let a = adapter();
        let (records, _cursor) = a
            .read_new_records(&cursor_for("2-missing-session-meta.jsonl"))
            .unwrap();
        let rows = a.parse(records).unwrap();

        assert!(!rows.is_empty(), "must parse at least one row");
        for row in &rows {
            assert_eq!(
                row.session_id, "codex-session-002",
                "session_id must come from event_msg rows"
            );
        }
    }

    // 3. Multiple sessions back-to-back; both session_ids present.
    #[test]
    fn multiple_sessions_in_one_file() {
        let a = adapter();
        let (records, _cursor) = a
            .read_new_records(&cursor_for("3-multiple-sessions.jsonl"))
            .unwrap();
        let rows = a.parse(records).unwrap();

        let has_003a = rows.iter().any(|r| r.session_id == "codex-session-003a");
        let has_003b = rows.iter().any(|r| r.session_id == "codex-session-003b");
        assert!(has_003a, "expected rows from codex-session-003a");
        assert!(has_003b, "expected rows from codex-session-003b");
    }

    // 4. tool_calls extraction and files_touched_json.
    #[test]
    fn extracts_tool_calls_and_files() {
        let a = adapter();
        let (records, _cursor) = a
            .read_new_records(&cursor_for("4-tool-use-event.jsonl"))
            .unwrap();
        let rows = a.parse(records).unwrap();

        let tool_call_row = rows
            .iter()
            .find(|r| r.tool_calls_json.is_some())
            .expect("at least one row must have tool_calls_json");

        assert!(tool_call_row.tool_calls_json.is_some());

        // The read_file call has input.path — must appear in files_touched_json.
        let file_row = rows
            .iter()
            .find(|r| r.files_touched_json.is_some())
            .expect("at least one row must have files_touched_json");

        let files = file_row.files_touched_json.as_ref().unwrap();
        assert!(
            files.contains("/synthetic/path/6/Cargo.toml"),
            "expected path in files_touched_json, got: {}",
            files
        );
    }

    // 5. Partial line tail emits PartialJsonl at the correct byte offset
    //    AND the error chain does not echo any partial-line bytes.
    #[test]
    fn partial_line_tail_returns_error_with_offset() {
        let a = adapter();
        let cursor = cursor_for("5-partial-line-tail.jsonl");
        let err = a.read_new_records(&cursor).unwrap_err();

        match &err {
            AdapterError::PartialJsonl { offset, .. } => {
                assert_eq!(
                    *offset, 785,
                    "partial line must start at byte 785, got {}",
                    offset
                );
            }
            other => panic!("expected AdapterError::PartialJsonl, got {:?}", other),
        }

        // Read the actual partial bytes from the fixture and assert that no
        // contiguous chunk of them appears in either Display or Debug
        // rendering of the error chain. This proves the partial-tail path
        // does not feed record content into serde_json::Error internals.
        let raw = std::fs::read(fixture_path("5-partial-line-tail.jsonl")).unwrap();
        let partial = std::str::from_utf8(&raw[785..])
            .expect("synthetic fixture is utf-8")
            .trim_end();
        let probe = &partial[..partial.len().min(40)];
        let display = format!("{}", err);
        let debug = format!("{:?}", err);
        assert!(
            !display.contains(probe),
            "PartialJsonl Display leaked partial bytes: {display}"
        );
        assert!(
            !debug.contains(probe),
            "PartialJsonl Debug leaked partial bytes: {debug}"
        );
    }

    // 6. Cursor advances monotonically; second read returns nothing new.
    #[test]
    fn cursor_advances_monotonically() {
        let a = adapter();
        let cursor0 = cursor_for("1-simple-session.jsonl");
        let (records1, cursor1) = a.read_new_records(&cursor0).unwrap();
        assert!(!records1.is_empty(), "first read must return records");
        assert!(
            cursor1.byte_offset >= cursor0.byte_offset,
            "cursor must not regress"
        );

        let (records2, cursor2) = a.read_new_records(&cursor1).unwrap();
        assert!(records2.is_empty(), "no new records on second read");
        assert_eq!(
            cursor2.byte_offset, cursor1.byte_offset,
            "cursor must be stable"
        );
    }

    // 7. Containment check rejects cursor paths outside sessions_root.
    #[test]
    fn containment_check_rejects_paths_outside_root() {
        let a = adapter();
        // Point cursor at a real file that exists but is outside the fixture root.
        let bad_cursor = CodexCursor {
            file_path: PathBuf::from("/etc/hostname"),
            byte_offset: 0,
            last_event_seq: 0,
            history_offset: 0,
            project_dir: None,
        };
        let err = a.read_new_records(&bad_cursor).unwrap_err();
        assert!(
            matches!(err, AdapterError::PathNotFound(_)),
            "expected PathNotFound, got {:?}",
            err
        );
    }

    // 8. Read cap clamps oversized reads — synthesize a file > MAX_READ_BYTES_PER_POLL.
    #[test]
    fn read_cap_clamps_oversized_read() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("big.jsonl");

        // Write enough complete lines to exceed the cap, then one partial line.
        {
            use std::io::Write;
            let mut f = std::fs::File::create(&file_path).unwrap();
            // Each line is ~1 KiB. We need > 64 MiB = 65536 lines.
            let line = format!(
                "{{\"type\":\"event_msg\",\"session_id\":\"s\",\"seq\":1,\"role\":\"user\",\"content\":\"{}\",\"ts\":1}}\n",
                "x".repeat(980)
            );
            let line_bytes = line.as_bytes();
            // Write enough to exceed the cap.
            let cap = MAX_READ_BYTES_PER_POLL as usize;
            let mut written = 0;
            while written < cap + line_bytes.len() {
                f.write_all(line_bytes).unwrap();
                written += line_bytes.len();
            }
            // Partial tail: no newline.
            f.write_all(b"{\"truncated\":true").unwrap();
        }

        let a = CodexAdapter::with_sessions_root(dir.path().to_path_buf());
        let cursor = CodexCursor {
            file_path: file_path.clone(),
            byte_offset: 0,
            last_event_seq: 0,
            history_offset: 0,
            project_dir: None,
        };
        // Because the cap truncates at the boundary, the partial tail beyond
        // the cap (or the actual partial line within the capped window) must
        // surface as PartialJsonl.
        let result = a.read_new_records(&cursor);
        assert!(
            matches!(result, Err(AdapterError::PartialJsonl { .. })),
            "expected PartialJsonl for oversized read with partial tail, got {:?}",
            result
        );
    }

    // 9. detect() returns Some when sessions root exists.
    #[test]
    fn detect_returns_root_when_present() {
        let dir = tempfile::tempdir().unwrap();
        let a = CodexAdapter::with_sessions_root(dir.path().to_path_buf());
        let result = a.detect().unwrap();
        assert!(result.is_some(), "detect must return Some when dir exists");
        assert_eq!(result.unwrap(), dir.path());
    }

    // 10. detect() returns None when sessions root does not exist.
    #[test]
    fn detect_returns_none_when_absent() {
        let a = CodexAdapter::with_sessions_root(PathBuf::from(
            "/tmp/carryover-test-nonexistent-codex-dir-xyz-987654",
        ));
        let result = a.detect().unwrap();
        assert!(
            result.is_none(),
            "detect must return None when dir is absent"
        );
    }
}
