use rusqlite_migration::{Migrations, M};

/// Returns the embedded migration set for the ledger database.
///
/// Migrations are append-only and applied atomically via `PRAGMA user_version`.
/// Never reorder or edit existing entries — always append new `M::up(...)` calls.
pub(crate) fn migrations() -> Migrations<'static> {
    Migrations::new(vec![M::up(
        "
            CREATE TABLE events (
                id                  INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id          TEXT NOT NULL,
                tool                TEXT NOT NULL,
                ts                  INTEGER NOT NULL,
                role                TEXT NOT NULL,
                content             TEXT NOT NULL,
                tool_calls_json     TEXT,
                files_touched_json  TEXT,
                parent_id           TEXT
            );

            CREATE INDEX idx_events_session_ts ON events(session_id, ts);
            CREATE INDEX idx_events_tool_ts    ON events(tool, ts);

            CREATE TABLE cursors (
                tool        TEXT NOT NULL,
                session_id  TEXT NOT NULL,
                cursor_json BLOB NOT NULL,
                PRIMARY KEY (tool, session_id)
            );
        ",
    )])
}
