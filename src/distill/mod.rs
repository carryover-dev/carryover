//! Distillation pipeline: pure-code extractors that turn LedgerRow batches
//! into the components of the 50-line handoff payload.
//!
//! Always-on extractors (this PR):
//! - task: condenses the latest user prompt into one line
//! - open_questions: detects unresolved decisions/TODOs across the session
//! - next_action: extracts the final actionable instruction
//!
//! Coding-only extractors (recent_files / failed_approaches / git_context)
//! ship in a follow-up PR; they activate only when the session contains
//! tool_use rows.

pub mod next_action;
pub mod open_questions;
pub mod task;
