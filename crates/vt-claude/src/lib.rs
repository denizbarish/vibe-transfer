//! Claude Code adapter: what Claude data exists on this machine, how each
//! item is classified, which project it belongs to, and how its paths are
//! rewritten and merged on the target.

mod claude_json;
mod inventory;
mod merge;
mod projects;
mod transform;

use vt_core::VtEnv;

pub use claude_json::read_claude_json_stable;
pub use inventory::{Inventory, classify};
pub use merge::{merge_claude_json, merge_memory_md};
pub use projects::project_inner_files;
pub use transform::{
    CLAUDE_REWRITE_JSONL_CWD, memory_slug_for, rewrite_json_paths, rewrite_jsonl_cwd,
    target_slug_dir,
};

/// Claude Code adapter bound to one (possibly synthetic) environment.
#[derive(Debug, Clone)]
pub struct ClaudeAdapter {
    pub env: VtEnv,
}
