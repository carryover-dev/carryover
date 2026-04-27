//! Tool-specific transcript readers (Claude / Cursor / Codex).
//!
//! Each adapter implements the `Adapter` trait with an associated `Cursor` type
//! (decision D1 in `decisions.md`). v0.1 ships three adapters; trait stays
//! `pub(crate)` until v0.4+ when external plugin support lands.
