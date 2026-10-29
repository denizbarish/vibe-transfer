//! Rewriting path-dependent Claude data for the target machine: project
//! slug directories, JSON path values and session `cwd` fields.

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use serde_json::Value;
use serde_json::value::RawValue;
use unicode_normalization::UnicodeNormalization;
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

/// Rewrites absolute paths in a JSON document (`~/.claude.json`, settings,
/// plugin registries, project `.mcp.json`) and returns how many strings and
/// keys changed.
///
/// Every string value gets `rewrite_home`. Keys of the top-level `projects`
/// object and the paths in top-level `githubRepoPaths` arrays are mapped
/// through `project_map` first (exact path or a path below it); only paths
/// without a project mapping fall back to `rewrite_home`. A `projects` key
/// whose new name would collide with another entry keeps its old name.
pub fn rewrite_json_paths(
    v: &mut Value,
    src_home: &str,
    dst_home: &str,
    project_map: &BTreeMap<String, PathBuf>,
) -> u32 {
    let rewrite = |s: &str| rewrite_path(s, src_home, dst_home, project_map);
    let Value::Object(top) = v else {
        return rewrite_strings(v, src_home, dst_home);
    };
    let mut count = 0;
    for (key, value) in top.iter_mut() {
        match (key.as_str(), value) {
            ("projects", Value::Object(projects)) => {
                count += rename_keys(projects, &rewrite);
                for entry in projects.values_mut() {
                    count += rewrite_strings(entry, src_home, dst_home);
                }
            }
            ("githubRepoPaths", Value::Object(repos)) => {
                for paths in repos.values_mut() {
                    let Value::Array(paths) = paths else {
                        count += rewrite_strings(paths, src_home, dst_home);
                        continue;
                    };
                    for p in paths {
                        match p {
                            Value::String(s) => {
                                if let Some(new) = rewrite(s) {
                                    *s = new;
                                    count += 1;
                                }
                            }
                            other => count += rewrite_strings(other, src_home, dst_home),
                        }
                    }
                }
            }
            (_, value) => count += rewrite_strings(value, src_home, dst_home),
        }
    }
    count
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

/// Project mapping first, then the home rewrite; `None` when unchanged.
fn rewrite_path(
    s: &str,
    src_home: &str,
    dst_home: &str,
    project_map: &BTreeMap<String, PathBuf>,
) -> Option<String> {
    map_project_path(s, project_map)
        .or_else(|| Some(rewrite_home(s, src_home, dst_home)))
        .filter(|new| new != s)
}

/// Applies `rewrite_home` to every string value (not keys) under `v`.
fn rewrite_strings(v: &mut Value, src_home: &str, dst_home: &str) -> u32 {
    match v {
        Value::String(s) => {
            let new = rewrite_home(s, src_home, dst_home);
            if new == *s {
                0
            } else {
                *s = new;
                1
            }
        }
        Value::Array(items) => items
            .iter_mut()
            .map(|item| rewrite_strings(item, src_home, dst_home))
            .sum(),
        Value::Object(map) => map
            .values_mut()
            .map(|item| rewrite_strings(item, src_home, dst_home))
            .sum(),
        _ => 0,
    }
}

/// Renames the keys of `map` with `rewrite`. Keys whose new name would be
/// shared with another entry keep their old name, so no entry is lost.
/// Returns how many keys were renamed.
fn rename_keys(
    map: &mut serde_json::Map<String, Value>,
    rewrite: &dyn Fn(&str) -> Option<String>,
) -> u32 {
    let old: Vec<String> = map.keys().cloned().collect();
    let mut new: Vec<String> = old
        .iter()
        .map(|k| rewrite(k).unwrap_or_else(|| k.clone()))
        .collect();
    // Revert colliding renames until every key is unique. Old keys are
    // unique, so this ends.
    loop {
        let mut seen: HashMap<&str, usize> = HashMap::new();
        for k in &new {
            *seen.entry(k.as_str()).or_default() += 1;
        }
        let clashing: Vec<usize> = (0..new.len())
            .filter(|&i| seen[new[i].as_str()] > 1 && new[i] != old[i])
            .collect();
        if clashing.is_empty() {
            break;
        }
        for i in clashing {
            new[i] = old[i].clone();
        }
    }
    let mut renamed = 0;
    let entries = std::mem::take(map);
    for ((_, value), (old_key, new_key)) in entries.into_iter().zip(old.iter().zip(new)) {
        if new_key != *old_key {
            renamed += 1;
        }
        map.insert(new_key, value);
    }
    renamed
}
