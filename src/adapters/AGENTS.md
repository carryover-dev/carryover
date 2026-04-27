# AGENTS.md — adapters

## Purpose
Read tool-specific transcripts (Claude, Cursor, Codex) and normalize them into `LedgerRow` records via the `Adapter` trait.

## Architectural decisions that constrain this module
- D1 (decisions.md): `Adapter` trait uses an associated `Cursor` type — each adapter defines its own bookmark shape (`ClaudeAdapter`, `CursorAdapter`, `CodexAdapter` each has a private `*Cursor` struct). No global `Cursor` enum.
- D3 (decisions.md): error type is `AdapterError` (thiserror). No anyhow in this module.
- RELEASE_ROADMAP.md: v0.1 ships exactly three adapters; trait is `pub(crate)` until v0.4+ plugin support.
- No shared parsing helpers between adapters — each adapter owns its own read logic.

## Files
- `mod.rs` — `Adapter` trait, `RawRecord`, `LedgerRow`, `AdapterError`, and the `AdapterKind` dispatch enum (Phase 2)

## Out of scope for v0.1
- External plugin API (v0.4+)
- Windows path support (`PathSpec::windows` field reserved but unused)
- Any parsing helper shared across adapters
