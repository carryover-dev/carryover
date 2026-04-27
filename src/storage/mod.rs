//! Append-only SQLite ledger at `~/.carryover/ledger.sqlite`. WAL mode.
//!
//! Single writer thread + `Arc<Mutex<Connection>>` (no pool needed for append-only).
//! Migrations via `rusqlite_migration` 2.5. WAL fallback uses `std::fs::copy` —
//! NOT `VACUUM INTO` (see decisions.md gotcha #2). `bundled` feature links libsqlite3
//! statically so the binary has no runtime dep on system SQLite.
