//! Rewriting path-dependent Claude data for the target machine: project
//! slug directories, JSON path values and session `cwd` fields.

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use serde_json::value::RawValue;
use serde_json::{Map, Value};
use unicode_normalization::UnicodeNormalization;
use vt_core::manifest::JsonConflict;
use vt_core::pathcodec::claude_slug;
use vt_core::pathmap::rewrite_home;

/// Ruling R1: session jsonl `cwd` fields are left as they are, because the
/// spike that would show Claude needs them rewritten was not run (see
/// `docs/spikes/2026-10-29-claude-cwd-rewrite.md`). `rewrite_jsonl_cwd` is
/// only used when this is `true`.
pub const CLAUDE_REWRITE_JSONL_CWD: bool = false;

/// Slug of the target directory for one old `projects/<slug>` directory.
///
/// Every cwd found in the old directory is NFC-normalized and mapped through
/// `mapping` (source project path to target path; a cwd below a mapped
/// project keeps its relative part). NFC and NFD variants of one source path
/// therefore land in the same target directory.
///
/// Returns `None` when there are no cwds, when any cwd has no mapping, or
/// when the cwds map to different target paths: the caller treats the
/// directory as a conflict and keeps it pending.
pub fn target_slug_dir(
    old_slug_dir_cwds: &[String],
    mapping: &BTreeMap<String, PathBuf>,
) -> Option<String> {
    let mut target: Option<String> = None;
    for cwd in old_slug_dir_cwds {
        let mapped = map_project_path(cwd, mapping)?;
        match &target {
            Some(t) if *t != mapped => return None,
            Some(_) => {}
            None => target = Some(mapped),
        }
    }
    target.map(|t| claude_slug(&t))
}

/// Slug of the auto memory directory: Claude keys memory by the git root, so
/// worktree and subdirectory sessions share it.
pub fn memory_slug_for(git_root: &Path) -> String {
    claude_slug(&git_root.to_string_lossy())
}

/// Result of `rewrite_json_paths`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RewriteOutcome {
    /// Strings and keys that changed.
    pub count: u32,
    /// `projects` entries that landed on one target key with different
    /// values; the first entry (by old key) was kept under that key.
    pub conflicts: Vec<JsonConflict>,
}

/// Rewrites absolute paths in a JSON document (`~/.claude.json`, settings,
/// plugin registries, project `.mcp.json`).
///
/// A string value that is one whole absolute path (see `is_whole_path`) is
/// mapped through `project_map` first (exact path or a path below a
/// project) and falls back to `rewrite_home`. A string with embedded paths,
/// such as a hook command or a permission rule, gets `rewrite_home` only.
/// Keys of the top-level `projects` object and the paths in top-level
/// `githubRepoPaths` arrays are always treated as whole paths; those arrays
/// are deduplicated afterwards, keeping the first occurrence.
///
/// When several `projects` keys land on one target key (e.g. NFC and NFD
/// spellings of one folder), equal entries collapse into one; otherwise the
/// first by old key is kept and every other one is returned as a conflict.
/// No source-path key is left behind.
pub fn rewrite_json_paths(
    v: &mut Value,
    src_home: &str,
    dst_home: &str,
    project_map: &BTreeMap<String, PathBuf>,
) -> RewriteOutcome {
    let rw = Rewriter {
        src_home,
        dst_home,
        project_map,
    };
    let mut out = RewriteOutcome::default();
    let Value::Object(top) = v else {
        out.count = rw.value(v);
        return out;
    };
    for (key, value) in top.iter_mut() {
        match (key.as_str(), value) {
            ("projects", Value::Object(projects)) => rw.projects(projects, &mut out),
            ("githubRepoPaths", Value::Object(repos)) => {
                for paths in repos.values_mut() {
                    out.count += rw.repo_paths(paths);
                }
            }
            (_, value) => out.count += rw.value(value),
        }
    }
    out
}

/// One string value under the rule of `rewrite_json_paths` (R12): a whole
/// path is mapped through `project_map` first, then the home rewrite; text
/// with embedded paths gets the home rewrite only. Used for symlink targets.
pub(crate) fn rewrite_path_value(
    s: &str,
    src_home: &str,
    dst_home: &str,
    project_map: &BTreeMap<String, PathBuf>,
) -> String {
    let rw = Rewriter {
        src_home,
        dst_home,
        project_map,
    };
    rw.string(s).unwrap_or_else(|| s.to_owned())
}

/// Rewrites the top-level `cwd` string of one jsonl line, leaving every
/// other byte of the line as it was. `None` when the line is not a JSON
/// object, has no string `cwd`, or `map` gives no different path.
pub fn rewrite_jsonl_cwd(line: &str, map: &dyn Fn(&str) -> Option<String>) -> Option<String> {
    let fields: HashMap<Cow<'_, str>, &RawValue> = serde_json::from_str(line).ok()?;
    let raw = fields.get("cwd")?.get();
    let old: String = serde_json::from_str(raw).ok()?;
    let new = map(&old).filter(|new| *new != old)?;
    // `raw` borrows from `line`; its offset locates the value's bytes.
    let start = (raw.as_ptr() as usize).checked_sub(line.as_ptr() as usize)?;
    let end = start + raw.len();
    if line.get(start..end) != Some(raw) {
        return None;
    }
    let encoded = serde_json::to_string(&new).ok()?;
    Some(format!("{}{encoded}{}", &line[..start], &line[end..]))
}

/// Target path for `path` via the longest matching project in `mapping`,
/// comparing NFC forms. A path below a project keeps its relative part.
fn map_project_path(path: &str, mapping: &BTreeMap<String, PathBuf>) -> Option<String> {
    let path: String = path.nfc().collect();
    let mut best: Option<(usize, String)> = None;
    for (src, dst) in mapping {
        let src: String = src.nfc().collect();
        let src = src
            .strip_suffix('/')
            .filter(|s| !s.is_empty())
            .unwrap_or(&src);
        let Some(rest) = path.strip_prefix(src) else {
            continue;
        };
        if !(rest.is_empty() || rest.starts_with('/')) {
            continue;
        }
        if best.as_ref().is_some_and(|(len, _)| *len >= src.len()) {
            continue;
        }
        let Some(dst) = dst.to_str() else { continue };
        let dst = dst
            .strip_suffix('/')
            .filter(|d| !d.is_empty())
            .unwrap_or(dst);
        best = Some((src.len(), format!("{dst}{rest}")));
    }
    best.map(|(_, target)| target)
}

/// One rewrite run: source and target homes plus the project mapping.
struct Rewriter<'a> {
    src_home: &'a str,
    dst_home: &'a str,
    project_map: &'a BTreeMap<String, PathBuf>,
}

impl Rewriter<'_> {
    /// A whole path: project mapping first, then the home rewrite; `None`
    /// when unchanged.
    fn path(&self, s: &str) -> Option<String> {
        map_project_path(s, self.project_map)
            .or_else(|| Some(rewrite_home(s, self.src_home, self.dst_home)))
            .filter(|new| new != s)
    }

    /// Any string value; `None` when unchanged.
    fn string(&self, s: &str) -> Option<String> {
        if is_whole_path(s) {
            self.path(s)
        } else {
            Some(rewrite_home(s, self.src_home, self.dst_home)).filter(|new| new != s)
        }
    }

    /// Rewrites every string value (not keys) under `v`.
    fn value(&self, v: &mut Value) -> u32 {
        match v {
            Value::String(s) => match self.string(s) {
                Some(new) => {
                    *s = new;
                    1
                }
                None => 0,
            },
            Value::Array(items) => items.iter_mut().map(|item| self.value(item)).sum(),
            Value::Object(map) => map.values_mut().map(|item| self.value(item)).sum(),
            _ => 0,
        }
    }

    /// Renames `projects` keys and rewrites their entries; entries that land
    /// on one key collapse or become conflicts.
    fn projects(&self, projects: &mut Map<String, Value>, out: &mut RewriteOutcome) {
        // `Map` is sorted, so the first entry per target key is the one with
        // the smallest old key.
        for (old_key, mut entry) in std::mem::take(projects) {
            out.count += self.value(&mut entry);
            let new_key = match self.path(&old_key) {
                Some(new_key) => {
                    out.count += 1;
                    new_key
                }
                None => old_key,
            };
            match projects.get(&new_key) {
                None => {
                    projects.insert(new_key, entry);
                }
                Some(kept) if *kept == entry => {}
                Some(kept) => out.conflicts.push(JsonConflict {
                    path: format!("projects.{new_key}"),
                    source: entry,
                    target: kept.clone(),
                }),
            }
        }
    }

    /// Rewrites one `githubRepoPaths` value and drops duplicate paths.
    fn repo_paths(&self, paths: &mut Value) -> u32 {
        let Value::Array(items) = paths else {
            return self.value(paths);
        };
        let mut count = 0;
        let mut kept: Vec<Value> = Vec::with_capacity(items.len());
        for mut item in std::mem::take(items) {
            match &mut item {
                Value::String(s) => {
                    if let Some(new) = self.path(s) {
                        *s = new;
                        count += 1;
                    }
                }
                other => count += self.value(other),
            }
            if !kept.contains(&item) {
                kept.push(item);
            }
        }
        *items = kept;
        count
    }
}

/// Whether `s` is one whole absolute path rather than text with paths in
/// it: it starts with `/`, has no control characters, and no other path
/// starts inside it (a `/` right after a space, quote, `=` or `(`).
fn is_whole_path(s: &str) -> bool {
    s.starts_with('/')
        && !s.chars().any(char::is_control)
        && !s
            .as_bytes()
            .windows(2)
            .any(|w| w[1] == b'/' && matches!(w[0], b' ' | b'"' | b'\'' | b'=' | b'('))
}
