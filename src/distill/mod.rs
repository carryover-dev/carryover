//! Distillation pipeline: pure-code extractors that turn LedgerRow batches
//! into the components of the 50-line handoff payload.
//!
//! Always-on extractors:
//! - task: condenses the latest user prompt into one line
//! - open_questions: detects unresolved decisions/TODOs across the session
//! - next_action: extracts the final actionable instruction
//!
//! Coding-only extractors (activate only when the session contains tool_use rows):
//! - recent_files: lists the most recently touched source files
//! - failed_approaches: surfaces tool errors and retry patterns
//! - git_context: captures HEAD sha and diff stat for the project directory

pub mod cursor_activity;
pub mod failed_approaches;
pub mod git_context;
pub mod next_action;
pub mod open_questions;
pub mod progress_log;
pub mod recent_files;
pub mod task;
pub mod util;
