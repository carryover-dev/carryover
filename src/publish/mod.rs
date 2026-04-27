//! Dual-write publisher: `~/.carryover/handoff.md` + `<project>/.carryover/handoff.md`
//! (gitignored). Pointer block in AGENTS.md + CLAUDE.md, marker
//! `<!--CARRYOVER:START-->` / `<!--CARRYOVER:END-->`.
//!
//! Privacy split: handoff content stays in `.carryover/handoff.md` (gitignored);
//! AGENTS.md/CLAUDE.md receive only the fixed pointer text (decisions.md D3 + privacy doc).
