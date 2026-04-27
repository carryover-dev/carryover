//! Append-only SQLite ledger at `~/.carryover/ledger.sqlite`. WAL mode.
//!
//! Single writer thread + `Arc<Mutex<Connection>>` (no pool needed for append-only).
//! Migrations via `rusqlite_migration` 2.5. WAL fallback uses `std::fs::copy` —
//! NOT `VACUUM INTO` (see decisions.md gotcha #2). `bundled` feature links libsqlite3
//! statically so the binary has no runtime dep on system SQLite.

mod migrations;

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
use thiserror::Error;

/// Errors produced by the storage subsystem.
#[derive(Debug, Error)]
pub enum StorageError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("migration: {0}")]
    Migration(#[from] rusqlite_migration::Error),

    #[error("ledger path: {0}")]
    LedgerPath(String),
}

/// A single row in the `events` table.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LedgerRow {
    pub session_id: String,
    pub tool: String,
    /// Unix epoch milliseconds.
    pub ts: i64,
    /// One of: `user`, `assistant`, `tool_use`, `tool_result`, `system`, `meta`.
    pub role: String,
    pub content: String,
    /// JSON-encoded array of tool calls, or `None`.
    pub tool_calls_json: Option<String>,
    /// JSON-encoded array of file paths, or `None`.
    pub files_touched_json: Option<String>,
    /// Parent UUID for Claude `parentUuid` chains, or `None`.
    pub parent_id: Option<String>,
}

/// Append-only SQLite ledger.
///
/// Wraps `Arc<Mutex<Connection>>` so it can be cheaply cloned and shared across
/// threads without a connection pool.
#[derive(Clone)]
pub struct Ledger {
    conn: Arc<Mutex<Connection>>,
}

impl Ledger {
    /// Open (or create) the ledger at `path`, apply pending migrations, and
    /// configure WAL mode + recommended pragmas.
    ///
    /// Safe to call repeatedly on the same path — migrations are idempotent.
    pub fn open(path: &Path) -> Result<Self, StorageError> {
        // Ensure the parent directory exists. On unix the parent of the
        // default path is `~/.carryover/`, which `default_path` creates with
        // mode 0o700; for arbitrary caller-supplied paths we fall back to
        // mkdir -p without forcing perms (the caller chose the location).
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }

        let mut conn = Connection::open(path)?;

        // Lock the ledger file to owner-only on unix. Transcripts may
        // contain pasted secrets; world-readable is unacceptable.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        }

        // Busy timeout must be set before any write so concurrent accessors
        // queue rather than immediately returning SQLITE_BUSY.
        conn.busy_timeout(Duration::from_millis(5000))?;

        // WAL mode + safe-but-fast sync + FK enforcement.
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             PRAGMA foreign_keys=ON;",
        )?;

        // Apply any pending migrations (idempotent via PRAGMA user_version).
        migrations::migrations().to_latest(&mut conn)?;

        // Flush statement cache after schema changes to avoid stale prepared
        // statements (rusqlite 0.39 footgun).
        conn.cache_flush()?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Returns `~/.carryover/ledger.sqlite`, creating `~/.carryover/` with
    /// owner-only permissions (0o700) if it does not yet exist.
    pub fn default_path() -> Result<PathBuf, StorageError> {
        let home = dirs::home_dir().ok_or_else(|| {
            StorageError::LedgerPath("could not resolve home directory".to_string())
        })?;
        Self::default_path_in(&home)
    }

    /// Resolve the default ledger path inside an arbitrary home directory.
    /// Internal helper that lets tests target a tempdir without mutating
    /// the process `HOME` environment variable.
    fn default_path_in(home: &Path) -> Result<PathBuf, StorageError> {
        let dir = home.join(".carryover");

        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&dir)?;
        }
        #[cfg(not(unix))]
        {
            std::fs::create_dir_all(&dir)?;
        }

        Ok(dir.join("ledger.sqlite"))
    }

    /// Append a single row and return its auto-increment `id`.
    pub fn insert(&self, row: &LedgerRow) -> Result<i64, StorageError> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO events
                (session_id, tool, ts, role, content,
                 tool_calls_json, files_touched_json, parent_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                row.session_id,
                row.tool,
                row.ts,
                row.role,
                row.content,
                row.tool_calls_json,
                row.files_touched_json,
                row.parent_id,
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Append multiple rows in a single transaction.
    ///
    /// Returns the auto-increment `id` for each row in insertion order.
    /// The entire batch is rolled back if any insert fails.
    pub fn insert_batch(&self, rows: &[LedgerRow]) -> Result<Vec<i64>, StorageError> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let mut ids = Vec::with_capacity(rows.len());
        for row in rows {
            tx.execute(
                "INSERT INTO events
                    (session_id, tool, ts, role, content,
                     tool_calls_json, files_touched_json, parent_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    row.session_id,
                    row.tool,
                    row.ts,
                    row.role,
                    row.content,
                    row.tool_calls_json,
                    row.files_touched_json,
                    row.parent_id,
                ],
            )?;
            ids.push(tx.last_insert_rowid());
        }
        tx.commit()?;
        Ok(ids)
    }

    /// Return all rows for `session_id`, ordered by `ts ASC`.
    pub fn query_session(&self, session_id: &str) -> Result<Vec<LedgerRow>, StorageError> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare_cached(
            "SELECT session_id, tool, ts, role, content,
                    tool_calls_json, files_touched_json, parent_id
             FROM events
             WHERE session_id = ?1
             ORDER BY ts ASC",
        )?;
        let rows = stmt
            .query_map(params![session_id], map_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Return the most-recent `limit` rows for `tool`, ordered by `ts DESC`.
    pub fn query_recent(&self, tool: &str, limit: usize) -> Result<Vec<LedgerRow>, StorageError> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare_cached(
            "SELECT session_id, tool, ts, role, content,
                    tool_calls_json, files_touched_json, parent_id
             FROM events
             WHERE tool = ?1
             ORDER BY ts DESC
             LIMIT ?2",
        )?;
        let rows = stmt
            .query_map(params![tool, limit as i64], map_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }
}

/// Map a rusqlite `Row` to a `LedgerRow`.
fn map_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<LedgerRow> {
    Ok(LedgerRow {
        session_id: row.get(0)?,
        tool: row.get(1)?,
        ts: row.get(2)?,
        role: row.get(3)?,
        content: row.get(4)?,
        tool_calls_json: row.get(5)?,
        files_touched_json: row.get(6)?,
        parent_id: row.get(7)?,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn sample_row(session_id: &str, ts: i64) -> LedgerRow {
        LedgerRow {
            session_id: session_id.to_string(),
            tool: "claude".to_string(),
            ts,
            role: "user".to_string(),
            content: "hello world".to_string(),
            tool_calls_json: None,
            files_touched_json: None,
            parent_id: None,
        }
    }

    #[test]
    fn open_creates_db() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ledger.sqlite");
        let ledger = Ledger::open(&path).expect("open should succeed");
        // Basic sanity: insert + read back
        let id = ledger.insert(&sample_row("s1", 1000)).unwrap();
        assert!(id > 0);
    }

    #[test]
    fn open_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ledger.sqlite");
        Ledger::open(&path).expect("first open");
        Ledger::open(&path).expect("second open should not fail or re-apply migrations");
    }

    #[test]
    fn insert_and_query_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Ledger::open(&dir.path().join("ledger.sqlite")).unwrap();

        let original = LedgerRow {
            session_id: "sess-abc".to_string(),
            tool: "cursor".to_string(),
            ts: 9999,
            role: "assistant".to_string(),
            content: "some response".to_string(),
            tool_calls_json: Some(r#"["edit"]"#.to_string()),
            files_touched_json: Some(r#"["src/main.rs"]"#.to_string()),
            parent_id: Some("parent-uuid".to_string()),
        };

        ledger.insert(&original).unwrap();
        let rows = ledger.query_session("sess-abc").unwrap();
        assert_eq!(rows.len(), 1);

        let got = &rows[0];
        assert_eq!(got.session_id, original.session_id);
        assert_eq!(got.tool, original.tool);
        assert_eq!(got.ts, original.ts);
        assert_eq!(got.role, original.role);
        assert_eq!(got.content, original.content);
        assert_eq!(got.tool_calls_json, original.tool_calls_json);
        assert_eq!(got.files_touched_json, original.files_touched_json);
        assert_eq!(got.parent_id, original.parent_id);
    }

    #[test]
    fn insert_batch_atomic() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Ledger::open(&dir.path().join("ledger.sqlite")).unwrap();

        let rows: Vec<LedgerRow> = (0..100).map(|i| sample_row("batch-sess", i)).collect();

        let ids = ledger.insert_batch(&rows).unwrap();
        assert_eq!(ids.len(), 100);

        // Rowids must be strictly increasing (sequential autoincrement).
        for w in ids.windows(2) {
            assert_eq!(w[1], w[0] + 1, "rowids should be sequential");
        }

        // All rows readable.
        let stored = ledger.query_session("batch-sess").unwrap();
        assert_eq!(stored.len(), 100);
    }

    #[test]
    fn concurrent_writes() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Arc::new(Ledger::open(&dir.path().join("ledger.sqlite")).unwrap());

        let handles: Vec<_> = (0..2)
            .map(|t| {
                let l = Arc::clone(&ledger);
                std::thread::spawn(move || {
                    let rows: Vec<LedgerRow> = (0..500)
                        .map(|i| {
                            let mut r = sample_row("concurrent-sess", i + t * 500);
                            r.content = format!("thread {} row {}", t, i);
                            r
                        })
                        .collect();
                    l.insert_batch(&rows).expect("no SQLITE_BUSY expected")
                })
            })
            .collect();

        for h in handles {
            h.join().expect("thread panicked");
        }

        let total = ledger.query_session("concurrent-sess").unwrap();
        assert_eq!(total.len(), 1000, "all 1000 rows should be present");
    }

    #[test]
    fn wal_mode_active() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Ledger::open(&dir.path().join("ledger.sqlite")).unwrap();

        let mode: String = {
            let conn = ledger.conn.lock().unwrap();
            conn.query_row("PRAGMA journal_mode", [], |r| r.get(0))
                .unwrap()
        };
        assert_eq!(mode, "wal");
    }

    #[test]
    fn query_session_orders_by_ts() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Ledger::open(&dir.path().join("ledger.sqlite")).unwrap();

        // Insert deliberately out of order.
        let rows = vec![
            sample_row("order-sess", 300),
            sample_row("order-sess", 100),
            sample_row("order-sess", 200),
        ];
        ledger.insert_batch(&rows).unwrap();

        let result = ledger.query_session("order-sess").unwrap();
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].ts, 100);
        assert_eq!(result[1].ts, 200);
        assert_eq!(result[2].ts, 300);
    }

    #[test]
    fn default_path_creates_carryover_dir() {
        let dir = tempfile::tempdir().unwrap();
        let path = Ledger::default_path_in(dir.path()).expect("default_path_in should succeed");

        let expected_dir = dir.path().join(".carryover");
        assert!(expected_dir.exists(), ".carryover dir should be created");
        assert_eq!(path, expected_dir.join("ledger.sqlite"));
    }

    #[cfg(unix)]
    #[test]
    fn default_path_in_sets_owner_only_perms() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        Ledger::default_path_in(dir.path()).unwrap();
        let mode = std::fs::metadata(dir.path().join(".carryover"))
            .unwrap()
            .permissions()
            .mode();
        // strip file-type bits, keep perm bits
        assert_eq!(mode & 0o777, 0o700, "expected 0o700 on ~/.carryover/");
    }

    #[cfg(unix)]
    #[test]
    fn open_sets_ledger_file_to_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ledger.sqlite");
        Ledger::open(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "expected 0o600 on ledger.sqlite");
    }

    #[test]
    fn query_recent_orders_filters_and_limits() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Ledger::open(&dir.path().join("ledger.sqlite")).unwrap();

        // Insert 5 rows for "claude" and 3 for "cursor", interleaved.
        let mut rows = Vec::new();
        for i in 0..5 {
            let mut r = sample_row("recent-sess", 1000 + i * 10);
            r.tool = "claude".to_string();
            r.content = format!("claude {}", i);
            rows.push(r);
        }
        for i in 0..3 {
            let mut r = sample_row("recent-sess", 2000 + i * 10);
            r.tool = "cursor".to_string();
            r.content = format!("cursor {}", i);
            rows.push(r);
        }
        ledger.insert_batch(&rows).unwrap();

        // Limit smaller than the total — must return only matching tool, ts DESC.
        let recent = ledger.query_recent("claude", 3).unwrap();
        assert_eq!(recent.len(), 3, "limit honored");
        assert!(
            recent.iter().all(|r| r.tool == "claude"),
            "tool filter exclusive"
        );
        // ts DESC: 1040, 1030, 1020
        assert_eq!(recent[0].ts, 1040);
        assert_eq!(recent[1].ts, 1030);
        assert_eq!(recent[2].ts, 1020);

        // Limit larger than match-set returns all matching rows.
        let all_cursor = ledger.query_recent("cursor", 100).unwrap();
        assert_eq!(all_cursor.len(), 3);

        // Unknown tool returns empty.
        let none = ledger.query_recent("codex", 10).unwrap();
        assert!(none.is_empty());
    }
}
