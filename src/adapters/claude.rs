//! Claude Code transcript adapter.
//!
//! Reads JSONL files at ~/.claude/projects/<slug>/<uuid>.jsonl and parses them
//! into LedgerRow records. Filters non-conversation type rows that the agent
//! emits for housekeeping (file-history-snapshot, permission-mode,
//! queue-operation, summary).

use crate::adapters::{Adapter, AdapterError, RawRecord};
use crate::storage::LedgerRow;
use serde::{Deserialize, Serialize};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Non-conversation type filter
// ---------------------------------------------------------------------------

const NON_CONVERSATION_TYPES: &[&str] = &[
    "file-history-snapshot",
    "permission-mode",
    "queue-operation",
    "summary",
];

// ---------------------------------------------------------------------------
// Cursor
// ---------------------------------------------------------------------------

/// Read-position cursor for a single Claude transcript file.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ClaudeCursor {
    /// Path to the JSONL transcript file.
    pub file_path: PathBuf,
    /// Byte offset of the first byte NOT yet consumed (exclusive lower bound).
    pub byte_offset: u64,
    /// UUID of the last consumed record, used to detect parent-chain regressions.
    pub last_uuid: Option<String>,
}

// ---------------------------------------------------------------------------
// Adapter
// ---------------------------------------------------------------------------

pub struct ClaudeAdapter {
    /// Optional override for the projects root, useful in tests. Production
    /// uses the default ~/.claude/projects/.
    pub projects_root: Option<PathBuf>,
}

impl ClaudeAdapter {
    pub fn new() -> Self {
        Self {
            projects_root: None,
        }
    }

    /// For tests: override the root to point at a fixture directory or any
    /// caller-supplied path.
    pub fn with_projects_root(root: PathBuf) -> Self {
        Self {
            projects_root: Some(root),
        }
    }

    fn projects_root(&self) -> Result<PathBuf, AdapterError> {
        if let Some(p) = &self.projects_root {
            return Ok(p.clone());
        }
        let home =
            dirs::home_dir().ok_or_else(|| AdapterError::PathNotFound(PathBuf::from("$HOME")))?;
        Ok(home.join(".claude").join("projects"))
    }
}

impl Default for ClaudeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl Adapter for ClaudeAdapter {
    type Cursor = ClaudeCursor;

    fn name(&self) -> &'static str {
        "claude"
    }

    /// Detect by checking whether the projects directory exists.
    fn detect(&self) -> Result<Option<PathBuf>, AdapterError> {
        let root = self.projects_root()?;
        if root.exists() {
            Ok(Some(root))
        } else {
            Ok(None)
        }
    }

    /// Read new complete lines from the cursor's file starting at byte_offset.
    ///
    /// A partial line at the tail (no trailing `\n`) is silently left
    /// unconsumed — the returned cursor stops at the last complete newline so
    /// the partial bytes are re-read on the next poll once the writer finishes
    /// the line. If the file ends exactly on a newline boundary (or is empty
    /// past the cursor), returns normally with an empty-or-populated vec and
    /// an advanced cursor.
    fn read_new_records(
        &self,
        since: &Self::Cursor,
    ) -> Result<(Vec<RawRecord>, Self::Cursor), AdapterError> {
        let file_path = &since.file_path;

        // No cursor yet (or file no longer exists): discover the most recently
        // modified JSONL transcript and start reading from offset 0.
        if file_path.as_os_str().is_empty() || !file_path.exists() {
            let root = self.projects_root()?;
            let discovered = find_newest_transcript(&root);
            match discovered {
                None => {
                    return Ok((
                        vec![],
                        ClaudeCursor {
                            file_path: PathBuf::new(),
                            byte_offset: 0,
                            last_uuid: since.last_uuid.clone(),
                        },
                    ));
                }
                Some(p) => {
                    return self.read_new_records(&ClaudeCursor {
                        file_path: p,
                        byte_offset: 0,
                        last_uuid: None,
                    });
                }
            }
        }

        // Containment + symlink guard: `cursor.file_path` is persisted to the
        // SQLite cursors table and could be tampered with. Without this check
        // a tampered cursor turns the adapter into an arbitrary-file-read
        // primitive (e.g. /etc/shadow, ~/.ssh/id_rsa). canonicalize() resolves
        // symlinks before the prefix check, closing symlink-escape as well.
        let root = self.projects_root()?;
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

        let records: Vec<RawRecord> = line_records
            .into_iter()
            .map(|(end_offset, bytes)| RawRecord {
                tool: self.name().to_string(),
                payload: bytes,
                offset: end_offset,
            })
            .collect();

        let last_uuid = records
            .iter()
            .rev()
            .find_map(|r| extract_uuid_from_payload(&r.payload))
            .or_else(|| since.last_uuid.clone());

        let advanced = ClaudeCursor {
            file_path: file_path.clone(),
            byte_offset: new_offset,
            last_uuid,
        };

        Ok((records, advanced))
    }

    /// Parse raw records into LedgerRow. Skips non-conversation types.
    /// Skip-on-error: malformed records (corrupt JSON lines) are silently
    /// skipped so a single bad line doesn't block the entire transcript.
    fn parse(&self, records: Vec<RawRecord>) -> Result<Vec<LedgerRow>, AdapterError> {
        let mut rows = Vec::with_capacity(records.len());

        for rec in records {
            let v: serde_json::Value = match serde_json::from_slice(&rec.payload) {
                Ok(v) => v,
                Err(_) => continue, // skip corrupt JSON line
            };

            // Filter housekeeping rows.
            let row_type = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
            if NON_CONVERSATION_TYPES.contains(&row_type) {
                continue;
            }

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

            // Claude JSONL uses "timestamp" (ISO-8601); fall back to "ts" (epoch ms).
            // Records without any timestamp (e.g. last-prompt, attachment) are skipped.
            let ts_val = v.get("timestamp").or_else(|| v.get("ts"));
            if ts_val.is_none() {
                continue;
            }
            let ts = parse_timestamp(ts_val, rec.offset)?;

            // Claude JSONL wraps role+content inside a "message" object.
            // Fall back to top-level role/content for older or alternative formats.
            let msg = v.get("message");
            let role = msg
                .and_then(|m| m.get("role"))
                .or_else(|| v.get("role"))
                .and_then(|r| r.as_str())
                .or_else(|| v.get("type").and_then(|t| t.as_str()))
                .ok_or_else(|| AdapterError::Parse {
                    offset: rec.offset,
                    context: "missing role and type fields",
                    source: make_missing_field_error(),
                })?
                .to_string();

            // content lives at message.content (string or array) or top-level content.
            let content_val = msg
                .and_then(|m| m.get("content"))
                .or_else(|| v.get("content"));
            let content = match content_val {
                None => String::new(),
                Some(serde_json::Value::String(s)) => s.clone(),
                Some(arr) => serde_json::to_string(arr).map_err(|e| AdapterError::Parse {
                    offset: rec.offset,
                    context: "failed to serialize content array",
                    source: e,
                })?,
            };

            let tool_calls_json = extract_tool_calls(content_val);
            let files_touched_json = extract_files_touched(content_val);
            let parent_id = v
                .get("parentUuid")
                .and_then(|p| p.as_str())
                .map(|s| s.to_string());

            rows.push(LedgerRow {
                session_id,
                tool: "claude".to_string(),
                ts,
                role,
                content,
                tool_calls_json,
                files_touched_json,
                parent_id,
            });
        }

        Ok(rows)
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Find the most recently modified `.jsonl` file across all project
/// subdirectories under `projects_root`. Returns `None` if the root doesn't
/// exist or contains no JSONL files.
fn find_newest_transcript(projects_root: &Path) -> Option<PathBuf> {
    let read_dir = std::fs::read_dir(projects_root).ok()?;
    let mut newest: Option<(std::time::SystemTime, PathBuf)> = None;
    for project_entry in read_dir.flatten() {
        let project_path = project_entry.path();
        if !project_path.is_dir() {
            continue;
        }
        let Ok(inner) = std::fs::read_dir(&project_path) else {
            continue;
        };
        for file_entry in inner.flatten() {
            let p = file_entry.path();
            if p.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            if let Ok(meta) = p.metadata() {
                if let Ok(modified) = meta.modified() {
                    if newest.as_ref().map(|(t, _)| modified > *t).unwrap_or(true) {
                        newest = Some((modified, p));
                    }
                }
            }
        }
    }
    newest.map(|(_, p)| p)
}

/// `(end_offset_exclusive, line_bytes_including_newline)` pairs from a file read.
type LineRecords = Vec<(u64, Vec<u8>)>;

/// Per-poll read cap. A single `read_new_records` call will never allocate
/// more than this many bytes of transcript into RAM, even if the file has
/// grown by gigabytes since the last cursor advance. Lines longer than this
/// surface as `AdapterError::PartialJsonl` (or get split across polls if the
/// writer eventually closes the line).
const MAX_READ_BYTES_PER_POLL: u64 = 64 * 1024 * 1024;

/// Read all complete (newline-terminated) lines from `file_path` starting at
/// `from_offset`. Returns `(records, new_offset)` where each record is
/// `(end_offset_exclusive, line_bytes_including_newline)`.
///
/// If a partial line (bytes without a trailing `\n`) is detected at the tail,
/// the partial bytes are silently left unconsumed: the returned cursor stops at
/// the last complete newline so the partial content is re-read on the next poll
/// once the writer finishes the line.
fn read_complete_lines(
    file_path: &Path,
    from_offset: u64,
    tool: &str,
) -> Result<(LineRecords, u64), AdapterError> {
    let _ = tool;
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
                // Partial tail — stop here; the cursor stays at byte_pos
                // (end of the last complete line) so the partial bytes are
                // re-read on the next poll.
                break;
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
        Some(serde_json::Value::String(s)) => {
            // Try integer string first, then ISO-8601 (Claude JSONL format).
            if let Ok(n) = s.parse::<i64>() {
                return Ok(n);
            }
            // Parse ISO-8601 like "2026-04-28T08:35:19.123Z" → epoch ms.
            if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
                return Ok(dt.timestamp_millis());
            }
            Err(AdapterError::Parse {
                offset,
                context: "ts/timestamp field is not parseable as epoch ms or ISO-8601",
                source: make_missing_field_error(),
            })
        }
        _ => Err(AdapterError::Parse {
            offset,
            context: "missing or non-numeric ts field",
            source: make_missing_field_error(),
        }),
    }
}

/// Extract `tool_use` entries from a content array.
/// Returns JSON-serialized array of tool_use blocks, or None if there are none.
fn extract_tool_calls(content: Option<&serde_json::Value>) -> Option<String> {
    let arr = content?.as_array()?;
    let tool_uses: Vec<&serde_json::Value> = arr
        .iter()
        .filter(|item| {
            item.get("type")
                .and_then(|t| t.as_str())
                .map(|t| t == "tool_use")
                .unwrap_or(false)
        })
        .collect();

    if tool_uses.is_empty() {
        return None;
    }

    serde_json::to_string(&tool_uses).ok()
}

/// Extract file paths from tool_use `input.path` fields within a content array.
/// Returns JSON-serialized array of path strings, or None if none found.
fn extract_files_touched(content: Option<&serde_json::Value>) -> Option<String> {
    let arr = content?.as_array()?;
    let paths: Vec<&str> = arr
        .iter()
        .filter(|item| {
            item.get("type")
                .and_then(|t| t.as_str())
                .map(|t| t == "tool_use")
                .unwrap_or(false)
        })
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

/// Extract the UUID from a raw payload without full deserialization.
fn extract_uuid_from_payload(payload: &[u8]) -> Option<String> {
    let v: serde_json::Value = serde_json::from_slice(payload).ok()?;
    v.get("uuid")?.as_str().map(|s| s.to_string())
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
        PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/claude"
        ))
        .join(name)
    }

    fn fixture_root() -> PathBuf {
        PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/claude"
        ))
    }

    /// Build an adapter whose `projects_root` is the fixture directory, so
    /// the containment check in `read_new_records` accepts cursor paths to
    /// the fixture files.
    fn adapter() -> ClaudeAdapter {
        ClaudeAdapter::with_projects_root(fixture_root())
    }

    fn cursor_for(name: &str) -> ClaudeCursor {
        ClaudeCursor {
            file_path: fixture_path(name),
            byte_offset: 0,
            last_uuid: None,
        }
    }

    // 1. Happy-path: 3 conversation rows, roles preserved.
    #[test]
    fn parses_simple_conversation() {
        let a = adapter();
        let (records, _cursor) = a
            .read_new_records(&cursor_for("1-simple-conversation.jsonl"))
            .unwrap();
        let rows = a.parse(records).unwrap();

        assert_eq!(rows.len(), 3, "expected 3 LedgerRows");
        assert_eq!(rows[0].role, "user");
        assert_eq!(rows[1].role, "assistant");
        assert_eq!(rows[2].role, "user");

        // All belong to the same session.
        for row in &rows {
            assert_eq!(row.session_id, "session-simple-001");
            assert_eq!(row.tool, "claude");
        }
    }

    // 2. Parent chain: every row's parent_id matches the previous uuid.
    #[test]
    fn preserves_parent_chain() {
        let a = adapter();
        let (records, _) = a
            .read_new_records(&cursor_for("2-parentuuid-chain-deep.jsonl"))
            .unwrap();
        let rows = a.parse(records).unwrap();

        assert!(
            rows.len() >= 3,
            "need at least 3 rows to verify chain depth >=2"
        );

        // Row 0 is root — no parent.
        assert!(
            rows[0].parent_id.is_none(),
            "root row must have no parent_id"
        );

        // Rows 1..n each have a non-None parent_id.
        for (i, row) in rows.iter().enumerate().skip(1) {
            assert!(row.parent_id.is_some(), "row {} must have a parent_id", i);
        }

        // Verify chain: rows[1].parent_id == rows[0]'s uuid
        // We read the uuid directly from fixture JSON to cross-check.
        let raw_lines =
            std::fs::read_to_string(fixture_path("2-parentuuid-chain-deep.jsonl")).unwrap();
        let values: Vec<serde_json::Value> = raw_lines
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();

        for (i, row) in rows.iter().enumerate().skip(1) {
            let expected_parent = values[i]
                .get("parentUuid")
                .and_then(|p| p.as_str())
                .unwrap();
            assert_eq!(
                row.parent_id.as_deref(),
                Some(expected_parent),
                "row {} parent_id mismatch",
                i
            );
        }
    }

    // 3. Tool use: tool_calls_json populated; files_touched_json has paths.
    #[test]
    fn extracts_tool_use_and_result() {
        let a = adapter();
        let (records, _) = a
            .read_new_records(&cursor_for("3-tool-use-and-result.jsonl"))
            .unwrap();
        let rows = a.parse(records).unwrap();

        // Row 1 (index 1) is the first assistant turn with a tool_use block.
        let tool_use_row = rows
            .iter()
            .find(|r| r.tool_calls_json.is_some())
            .expect("at least one row must have tool_calls_json");

        assert!(tool_use_row.tool_calls_json.is_some());
        assert!(
            tool_use_row.files_touched_json.is_some(),
            "tool_use row with path must have files_touched_json"
        );

        // Sanity: the path appears in files_touched_json.
        let files = tool_use_row.files_touched_json.as_ref().unwrap();
        assert!(
            files.contains("/synthetic/path/1/main.rs"),
            "expected path in files_touched_json, got: {}",
            files
        );
    }

    // 4. Non-conversation types are filtered out.
    #[test]
    fn filters_non_conversation_types() {
        let a = adapter();
        let (records, _) = a
            .read_new_records(&cursor_for("4-non-conversation-types.jsonl"))
            .unwrap();
        let total = records.len();
        let rows = a.parse(records).unwrap();

        // Fixture has 9 lines: 5 "say" + 4 housekeeping.
        assert_eq!(total, 9, "expected 9 raw records");
        assert_eq!(
            rows.len(),
            5,
            "expected 5 conversation rows after filtering"
        );

        // Every surviving row must have role user or assistant.
        for row in &rows {
            assert!(
                row.role == "user" || row.role == "assistant",
                "unexpected role: {}",
                row.role
            );
        }
    }

    // 5. Partial tail is silently skipped; complete lines are returned and the
    //    cursor stops at the last complete newline (offset 798).
    #[test]
    fn partial_line_tail_returns_complete_lines_and_stops_cursor() {
        let a = adapter();
        let cursor = cursor_for("5-partial-line-tail.jsonl");
        let (records, advanced) = a.read_new_records(&cursor).unwrap();

        // The fixture has 3 complete lines before the partial tail.
        assert!(
            !records.is_empty(),
            "should return the complete lines before partial tail"
        );
        assert_eq!(
            advanced.byte_offset, 798,
            "cursor must stop at last complete newline (798), got {}",
            advanced.byte_offset
        );

        // A second read from the advanced cursor must return nothing new
        // (partial tail is still incomplete).
        let (records2, advanced2) = a.read_new_records(&advanced).unwrap();
        assert!(records2.is_empty(), "no new complete lines on second read");
        assert_eq!(advanced2.byte_offset, advanced.byte_offset, "cursor stable");
    }

    // 6. Cursor is monotonic across two reads on a stable file (using fixture 1).
    #[test]
    fn cursor_monotonic_across_reads() {
        // Use a file with only complete lines so first read succeeds.
        let a = adapter();
        let cursor0 = cursor_for("1-simple-conversation.jsonl");
        let (records1, cursor1) = a.read_new_records(&cursor0).unwrap();
        assert!(!records1.is_empty(), "first read should return records");
        assert!(
            cursor1.byte_offset >= cursor0.byte_offset,
            "cursor must not regress"
        );

        // Second read from advanced cursor must return nothing new.
        let (records2, cursor2) = a.read_new_records(&cursor1).unwrap();
        assert!(records2.is_empty(), "no new records on second read");
        assert_eq!(
            cursor2.byte_offset, cursor1.byte_offset,
            "cursor must be stable"
        );
    }

    // 7. detect() returns Some when projects root exists.
    #[test]
    fn detect_returns_root_when_present() {
        let dir = tempfile::tempdir().unwrap();
        let a = ClaudeAdapter::with_projects_root(dir.path().to_path_buf());
        let result = a.detect().unwrap();
        assert!(result.is_some(), "detect must return Some when dir exists");
        assert_eq!(result.unwrap(), dir.path());
    }

    // 8. detect() returns None when projects root does not exist.
    #[test]
    fn detect_returns_none_when_absent() {
        let a = ClaudeAdapter::with_projects_root(PathBuf::from(
            "/tmp/carryover-test-nonexistent-dir-xyz-987654",
        ));
        let result = a.detect().unwrap();
        assert!(
            result.is_none(),
            "detect must return None when dir is absent"
        );
    }
}
