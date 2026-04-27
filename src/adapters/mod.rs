//! Tool-specific transcript readers (Claude / Cursor / Codex).
//!
//! Each adapter implements the [`Adapter`] trait with an associated [`Adapter::Cursor`] type
//! (decision D1 in `decisions.md`). v0.1 ships three adapters; trait stays `pub` so future
//! PRs in the same crate can implement it. External plugin visibility is v0.4+ work.

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::path::PathBuf;
use thiserror::Error;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors produced by adapter operations.
#[derive(Debug, Error)]
pub enum AdapterError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("storage: {0}")]
    Storage(#[from] crate::storage::StorageError),

    #[error("serde: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("transcript path not found: {0}")]
    PathNotFound(std::path::PathBuf),

    #[error("partial JSONL line at byte {offset}: {source}")]
    PartialJsonl {
        offset: u64,
        #[source]
        source: serde_json::Error,
    },

    #[error("SQLite WAL locked, copy-fallback failed: {0}")]
    WalCopyFailed(#[source] std::io::Error),

    /// A record could not be parsed into the unified [`crate::storage::LedgerRow`]
    /// schema. The `context` is a static label (e.g. `"missing role field"`) — it
    /// MUST NOT include record bytes; transcript content goes via `#[source]` only,
    /// where the underlying serde error gives line/column without the offending text.
    #[error("parse error at offset {offset}: {context}")]
    Parse {
        offset: u64,
        context: &'static str,
        #[source]
        source: serde_json::Error,
    },
}

// ---------------------------------------------------------------------------
// RawRecord
// ---------------------------------------------------------------------------

/// Opaque tool-specific raw bytes read from a transcript source.
///
/// JSONL adapters store one full line per record; the Cursor adapter stores a
/// JSON-encoded SQLite row blob. The `offset` field drives cursor advancement.
///
/// # Security
///
/// `RawRecord::payload` may contain pasted secrets, credentials, or PII from
/// user sessions. The [`Debug`] impl redacts the payload, and `RawRecord` does
/// **not** derive [`Serialize`]/[`Deserialize`] — it is a transport value
/// inside the daemon, never persisted, never logged with full content.
#[derive(Clone)]
pub struct RawRecord {
    /// Tool that produced this record (e.g. `"claude"`, `"cursor"`, `"codex"`).
    pub tool: String,
    /// Raw bytes from the transcript source.
    ///
    /// **Never log this field directly. Never include it in error messages.**
    /// May contain pasted secrets, credentials, or PII. Treat as opaque;
    /// extract typed fields only after a successful `parse()`.
    pub payload: Vec<u8>,
    /// Byte offset in the source file (for cursor advancement). For
    /// SQLite adapters this is the rusqlite rowid encoded as `u64`.
    pub offset: u64,
}

impl std::fmt::Debug for RawRecord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RawRecord")
            .field("tool", &self.tool)
            .field("offset", &self.offset)
            .field(
                "payload",
                &format_args!("<{} bytes redacted>", self.payload.len()),
            )
            .finish()
    }
}

// ---------------------------------------------------------------------------
// Adapter trait
// ---------------------------------------------------------------------------

/// Contract that every transcript reader must satisfy.
///
/// The associated `Cursor` type lets each adapter define its own bookmark
/// shape. Cursors are persisted as JSON in the `cursors` SQLite table so they
/// must be (de)serializable.
pub trait Adapter: Send + Sync {
    /// Per-adapter cursor (read-position bookmark). Each impl picks its own
    /// shape. Bounded so the daemon can persist cursors via SQLite.
    ///
    /// `Debug` is included on top of the locked D1 minimum to aid daemon
    /// diagnostics; cursors carry only offsets / ids and never transcript
    /// content, so `Debug` is safe to derive on real cursor types.
    type Cursor: Clone + std::fmt::Debug + Send + Sync + Serialize + DeserializeOwned;

    /// Tool name (lowercase, e.g. `"claude"`, `"cursor"`, `"codex"`).
    fn name(&self) -> &'static str;

    /// Detect whether this tool is installed. Returns the binary path if found.
    fn detect(&self) -> Result<Option<PathBuf>, AdapterError>;

    /// Read records since the given cursor.
    ///
    /// Returns new records and the advanced cursor. The advanced cursor must be
    /// monotonic (never regresses) to satisfy the correctness invariant.
    fn read_new_records(
        &self,
        since: &Self::Cursor,
    ) -> Result<(Vec<RawRecord>, Self::Cursor), AdapterError>;

    /// Parse raw records into the unified [`crate::storage::LedgerRow`] schema.
    ///
    /// Pure function — no I/O. **Fail-fast semantics:** the first malformed
    /// record causes the entire batch to error with [`AdapterError::Parse`].
    /// The daemon worker is expected to log the error and may retry the batch
    /// one record at a time to isolate the bad row.
    fn parse(
        &self,
        records: Vec<RawRecord>,
    ) -> Result<Vec<crate::storage::LedgerRow>, AdapterError>;
}

// ---------------------------------------------------------------------------
// AdapterKind dispatcher
// ---------------------------------------------------------------------------

/// Type-erased handle for carrying adapters of different concrete types in one
/// collection.
///
/// The associated [`Adapter::Cursor`] type is erased at the enum boundary by
/// (de)serializing cursors as JSON strings. The daemon worker stores cursors
/// in the `cursors` SQLite table as JSON blobs anyway, so the erasure is free.
///
/// Marked `#[non_exhaustive]` so adding variants is non-breaking for any
/// future external `match` site.
#[non_exhaustive]
pub enum AdapterKind {
    /// In-memory adapter for tests and pre-adapter scaffolding.
    Mock(mock::MockAdapter),
    /// Claude Code transcript adapter.
    Claude(claude::ClaudeAdapter),
}

impl AdapterKind {
    /// Delegate to the inner adapter's [`Adapter::name`].
    pub fn name(&self) -> &'static str {
        match self {
            AdapterKind::Mock(a) => a.name(),
            AdapterKind::Claude(a) => a.name(),
        }
    }

    /// Delegate to the inner adapter's [`Adapter::detect`].
    pub fn detect(&self) -> Result<Option<PathBuf>, AdapterError> {
        match self {
            AdapterKind::Mock(a) => a.detect(),
            AdapterKind::Claude(a) => a.detect(),
        }
    }

    /// Erased version of [`Adapter::read_new_records`]: takes the JSON-encoded
    /// cursor the daemon has on hand, returns records plus the JSON-encoded
    /// advanced cursor. An empty `since_json` (`""`) is treated as the cursor's
    /// `Default` value.
    pub fn read_new_records_erased(
        &self,
        since_json: &str,
    ) -> Result<(Vec<RawRecord>, String), AdapterError> {
        match self {
            AdapterKind::Mock(a) => {
                let cursor: <mock::MockAdapter as Adapter>::Cursor = if since_json.is_empty() {
                    Default::default()
                } else {
                    serde_json::from_str(since_json)?
                };
                let (records, advanced) = a.read_new_records(&cursor)?;
                let advanced_json = serde_json::to_string(&advanced)?;
                Ok((records, advanced_json))
            }
            AdapterKind::Claude(a) => {
                let cursor: <claude::ClaudeAdapter as Adapter>::Cursor = if since_json.is_empty() {
                    Default::default()
                } else {
                    serde_json::from_str(since_json)?
                };
                let (records, advanced) = a.read_new_records(&cursor)?;
                let advanced_json = serde_json::to_string(&advanced)?;
                Ok((records, advanced_json))
            }
        }
    }

    /// Delegate to the inner adapter's [`Adapter::parse`]. The cursor type
    /// doesn't appear in `parse`'s signature, so no erasure is needed here.
    pub fn parse(
        &self,
        records: Vec<RawRecord>,
    ) -> Result<Vec<crate::storage::LedgerRow>, AdapterError> {
        match self {
            AdapterKind::Mock(a) => a.parse(records),
            AdapterKind::Claude(a) => a.parse(records),
        }
    }
}

// ---------------------------------------------------------------------------
// Adapters
// ---------------------------------------------------------------------------

pub mod claude;

// ---------------------------------------------------------------------------
// Mock adapter
// ---------------------------------------------------------------------------

/// In-memory adapter for trait integration tests and downstream-PR scaffolding.
pub mod mock {
    use super::*;

    /// In-memory adapter used by trait integration tests and downstream-PR
    /// scaffolding. The pipeline worker tests can dispatch a `MockAdapter`
    /// rather than wait for a real tool adapter to land.
    pub struct MockAdapter {
        /// Tool name returned by [`Adapter::name`].
        pub name: &'static str,
        /// Binary path returned by [`Adapter::detect`].
        pub binary: Option<PathBuf>,
        /// Records available to be read.
        pub records: Vec<RawRecord>,
    }

    /// Read-position cursor for [`MockAdapter`].
    #[derive(Clone, Debug, Default, Serialize, Deserialize)]
    pub struct MockCursor {
        /// Last consumed offset (exclusive lower bound for the next read).
        pub offset: u64,
    }

    impl Adapter for MockAdapter {
        type Cursor = MockCursor;

        fn name(&self) -> &'static str {
            self.name
        }

        fn detect(&self) -> Result<Option<PathBuf>, AdapterError> {
            Ok(self.binary.clone())
        }

        fn read_new_records(
            &self,
            since: &MockCursor,
        ) -> Result<(Vec<RawRecord>, MockCursor), AdapterError> {
            let new: Vec<RawRecord> = self
                .records
                .iter()
                .filter(|r| r.offset > since.offset)
                .cloned()
                .collect();

            let advanced_offset = new.iter().map(|r| r.offset).max().unwrap_or(since.offset);

            Ok((
                new,
                MockCursor {
                    offset: advanced_offset,
                },
            ))
        }

        fn parse(
            &self,
            records: Vec<RawRecord>,
        ) -> Result<Vec<crate::storage::LedgerRow>, AdapterError> {
            let mut rows = Vec::with_capacity(records.len());
            for rec in records {
                let row: crate::storage::LedgerRow =
                    serde_json::from_slice(&rec.payload).map_err(|e| AdapterError::Parse {
                        offset: rec.offset,
                        context: "mock payload not a valid LedgerRow",
                        source: e,
                    })?;
                rows.push(row);
            }
            Ok(rows)
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::mock::{MockAdapter, MockCursor};
    use super::*;
    use crate::storage::LedgerRow;

    fn sample_record(tool: &str, offset: u64) -> RawRecord {
        RawRecord {
            tool: tool.to_string(),
            payload: vec![],
            offset,
        }
    }

    fn ledger_row_record(tool: &str, offset: u64) -> RawRecord {
        let row = LedgerRow {
            session_id: "test-session".to_string(),
            tool: tool.to_string(),
            ts: 1000,
            role: "user".to_string(),
            content: "hello".to_string(),
            tool_calls_json: None,
            files_touched_json: None,
            parent_id: None,
        };
        RawRecord {
            tool: tool.to_string(),
            payload: serde_json::to_vec(&row).unwrap(),
            offset,
        }
    }

    fn make_adapter() -> MockAdapter {
        MockAdapter {
            name: "mock",
            binary: Some(PathBuf::from("/usr/bin/mock")),
            records: vec![
                sample_record("mock", 3),
                sample_record("mock", 5),
                sample_record("mock", 7),
                sample_record("mock", 9),
            ],
        }
    }

    #[test]
    fn mock_adapter_detect() {
        let adapter = MockAdapter {
            name: "mock",
            binary: Some(PathBuf::from("/usr/bin/mock")),
            records: vec![],
        };
        let result = adapter.detect().unwrap();
        assert_eq!(result, Some(PathBuf::from("/usr/bin/mock")));

        let no_binary = MockAdapter {
            name: "mock",
            binary: None,
            records: vec![],
        };
        assert_eq!(no_binary.detect().unwrap(), None);
    }

    #[test]
    fn mock_adapter_reads_new_records_only() {
        let adapter = make_adapter();
        let cursor = MockCursor { offset: 5 };
        let (records, advanced) = adapter.read_new_records(&cursor).unwrap();

        let offsets: Vec<u64> = records.iter().map(|r| r.offset).collect();
        assert_eq!(offsets, vec![7, 9]);
        assert_eq!(advanced.offset, 9);
    }

    #[test]
    fn mock_adapter_cursor_monotonic() {
        let adapter = make_adapter();
        let cursor = MockCursor { offset: 5 };
        let (_, advanced) = adapter.read_new_records(&cursor).unwrap();
        assert!(advanced.offset >= cursor.offset, "cursor must not regress");

        // Second call with the advanced cursor returns empty + same cursor.
        let (second_records, second_cursor) = adapter.read_new_records(&advanced).unwrap();
        assert!(second_records.is_empty());
        assert_eq!(second_cursor.offset, advanced.offset);
    }

    #[test]
    fn mock_adapter_parse_round_trip() {
        let adapter = MockAdapter {
            name: "mock",
            binary: None,
            records: vec![ledger_row_record("mock", 1)],
        };

        let records = vec![ledger_row_record("mock", 1)];
        let rows = adapter.parse(records).unwrap();

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].session_id, "test-session");
        assert_eq!(rows[0].tool, "mock");
        assert_eq!(rows[0].role, "user");
        assert_eq!(rows[0].content, "hello");
    }

    #[test]
    fn mock_adapter_parse_returns_structured_error_on_invalid_json() {
        let adapter = MockAdapter {
            name: "mock",
            binary: None,
            records: vec![],
        };
        let bad = RawRecord {
            tool: "mock".to_string(),
            payload: b"not json".to_vec(),
            offset: 42,
        };
        let err = adapter
            .parse(vec![bad])
            .expect_err("must reject bad payload");
        match err {
            AdapterError::Parse {
                offset, context, ..
            } => {
                assert_eq!(offset, 42);
                assert!(
                    !context.is_empty(),
                    "context should be a non-empty static label"
                );
            }
            other => panic!("expected AdapterError::Parse, got {other:?}"),
        }
    }

    #[test]
    fn raw_record_debug_redacts_payload() {
        let rec = RawRecord {
            tool: "mock".to_string(),
            payload: b"super-secret-token".to_vec(),
            offset: 7,
        };
        let dbg = format!("{rec:?}");
        assert!(!dbg.contains("super-secret-token"), "payload bytes leaked");
        assert!(dbg.contains("18 bytes redacted"), "redaction hint missing");
    }

    #[test]
    fn adapter_error_propagates_storage_error() {
        let storage_err = crate::storage::StorageError::LedgerPath("bad path".to_string());
        let adapter_err = AdapterError::Storage(storage_err);
        assert!(
            matches!(adapter_err, AdapterError::Storage(_)),
            "AdapterError::Storage variant must be present"
        );
    }

    #[test]
    fn adapter_error_propagates_serde_error() {
        let serde_err: serde_json::Error =
            serde_json::from_str::<serde_json::Value>("not json").unwrap_err();
        let adapter_err: AdapterError = serde_err.into();
        assert!(matches!(adapter_err, AdapterError::Serde(_)));
    }

    #[test]
    fn adapter_kind_dispatch_name() {
        let mock = MockAdapter {
            name: "mock-tool",
            binary: None,
            records: vec![],
        };
        let kind = AdapterKind::Mock(mock);
        assert_eq!(kind.name(), "mock-tool");
    }

    #[test]
    fn adapter_kind_erased_dispatch_round_trip() {
        let kind = AdapterKind::Mock(make_adapter());

        // Empty cursor JSON -> default cursor (offset 0) -> all 4 records.
        let (records, advanced_json) = kind.read_new_records_erased("").unwrap();
        assert_eq!(records.len(), 4);

        // Advance using the returned JSON cursor; should now return nothing.
        let (records2, advanced_json2) = kind.read_new_records_erased(&advanced_json).unwrap();
        assert!(records2.is_empty(), "no new records past advanced cursor");
        assert_eq!(
            advanced_json, advanced_json2,
            "cursor must be stable when no new records"
        );

        // Cursor JSON deserializes back to MockCursor at offset 9 (the max).
        let cursor: MockCursor = serde_json::from_str(&advanced_json).unwrap();
        assert_eq!(cursor.offset, 9);
    }

    #[test]
    fn cursor_serde_round_trip() {
        let cursor = MockCursor { offset: 42 };
        let json = serde_json::to_string(&cursor).unwrap();
        let restored: MockCursor = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.offset, cursor.offset);
    }

    #[test]
    fn adapter_kind_dispatches_claude() {
        let a = claude::ClaudeAdapter::new();
        let kind = AdapterKind::Claude(a);
        assert_eq!(kind.name(), "claude");
    }
}
