//! Claude Code adapter: what Claude data exists on this machine, how each
//! item is classified, which project it belongs to, and how its paths are
//! rewritten and merged on the target.

mod claude_json;
mod inventory;
mod merge;
mod plan;
mod projects;
mod settings_refs;
mod transform;

use vt_core::VtEnv;

pub use claude_json::read_claude_json_stable;
pub use inventory::{Inventory, classify, item_key};
pub use merge::{
    claude_json_untransferred_keys, merge_claude_json, merge_json_settings, merge_memory_md,
};
pub use plan::{
    ClaudePlan, KIND_CLAUDE_JSON_MERGE, KIND_HOME_PATHS, KIND_JSON_MERGE, KIND_JSON_PATHS,
    KIND_MEMORY_MERGE, KIND_SYMLINK_TARGET, LinkInput, PlanInput, PlanQuestions, SessionStats,
    build_claude_apply_plan, build_link_plan, resolve_dest_template,
};
pub use projects::project_inner_files;
pub use transform::{
    CLAUDE_REWRITE_JSONL_CWD, RewriteOutcome, memory_slug_for, rewrite_json_paths,
    rewrite_jsonl_cwd, target_slug_dir,
};

/// Claude Code adapter bound to one (possibly synthetic) environment.
#[derive(Debug, Clone)]
pub struct ClaudeAdapter {
    pub env: VtEnv,
}
