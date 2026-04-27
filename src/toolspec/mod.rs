//! Per-tool version table. Newer-than-known versions fall back to last-known HookSet
//! with a structured warning event written to `~/.carryover/events.jsonl` (decision D2).
//!
//! Key types: `VersionRange`, `HookSet`, `PathSpec`, `ToolSpec`, `FallbackKind`.
//! `resolve_hookset()` iterates highest→lowest; on no match emits `FallbackKind::NewerThanKnown`.
//! Older-than-known → refuse install with a clear error message.
