# AGENTS.md — storage

## Purpose
Maintain the append-only SQLite ledger at `~/.carryover/ledger.sqlite` (WAL mode, single writer thread).

## Architectural decisions that constrain this module
- rusqlite 0.39 with `bundled` feature — links libsqlite3 statically; no runtime dep on system SQLite.
- `serde_json` feature enabled on rusqlite for JSON column helpers.
- WAL mode mandatory — allows concurrent readers while the writer appends.
- Single writer thread pattern: `Arc<Mutex<Connection>>` (no pool). Append-only workload does not need a pool.
- Migrations via `rusqlite_migration` 2.5 — migration list defined at startup; `M` structs carry raw SQL.
- WAL copy fallback (for Cursor adapter, decisions.md gotcha #2): use `std::fs::copy` — NOT `VACUUM INTO`. Copy `-wal` + `-shm` sidecars too.
- D3 (decisions.md): error type is `StorageError` (thiserror). No anyhow in this module.

## Files
- `mod.rs` — connection setup stub; `ledger` + `cursors` + `events` table schema lands in Phase 2 (TASK-06)

## Out of scope for v0.1
- Encryption at rest (`StorageError::Encryption` variant reserved but unused)
- Connection pool (unnecessary for single-writer append-only)
- Remote sync / replication
