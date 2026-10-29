//! Claude Code adapter: what Claude data exists on this machine, how each
//! item is classified, and which project it belongs to.

mod claude_json;
mod inventory;
mod projects;

use vt_core::VtEnv;

pub use claude_json::read_claude_json_stable;
pub use inventory::{Inventory, classify};
pub use projects::project_inner_files;

/// Claude Code adapter bound to one (possibly synthetic) environment.
#[derive(Debug, Clone)]
pub struct ClaudeAdapter {
    pub env: VtEnv,
}
