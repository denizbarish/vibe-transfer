//! Matching source projects to target paths (spec 7) and home rewriting.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};

use crate::manifest::ProjectRef;
use crate::remote::{normalize_remote, read_origin};

/// Directory names never descended into while indexing the target.
/// Hidden names (leading `.`, e.g. `.Trash`) are skipped at every depth too.
const PRUNED_NAMES: &[&str] = &["node_modules"];

/// Names skipped only directly under an index root (the home folder):
/// `~/Library` is huge and partly privacy-protected on macOS, while a
/// project folder named `Library` deeper down is still indexed.
const PRUNED_TOP_NAMES: &[&str] = &["Library"];

/// Git repositories on the target, keyed by normalized `origin` remote.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TargetIndex {
    /// Repository roots per remote, sorted and deduplicated.
    pub by_remote: BTreeMap<String, Vec<PathBuf>>,
    /// Directories that could not be read (e.g. denied by macOS privacy
    /// controls) and were skipped.
    pub skipped_dirs: u32,
}

/// Indexes git repositories under `roots`, at most `max_depth` levels below
/// each root (the root itself is depth 0). Symlinks are never followed,
/// pruned and hidden directories are skipped, unreadable directories are
/// counted in `skipped_dirs`, and a repository root is not descended into.
/// `Library` is pruned only directly under each root.
pub fn build_target_index(roots: &[PathBuf], max_depth: usize) -> TargetIndex {
    let mut idx = TargetIndex::default();
    for root in roots {
        visit(root, 0, max_depth, &mut idx);
    }
    for paths in idx.by_remote.values_mut() {
        paths.sort();
        paths.dedup();
    }
    idx
}

fn visit(dir: &Path, depth: usize, max_depth: usize, idx: &mut TargetIndex) {
    if fs::symlink_metadata(dir.join(".git")).is_ok() {
        if let Some(remote) = read_origin(dir).and_then(|url| normalize_remote(&url)) {
            idx.by_remote
                .entry(remote)
                .or_default()
                .push(dir.to_path_buf());
        }
        return;
    }
    if depth >= max_depth {
        return;
    }
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => {
            idx.skipped_dirs += 1;
            return;
        }
    };
    for entry in entries.flatten() {
        // `DirEntry::file_type` does not follow symlinks.
        let Ok(ft) = entry.file_type() else { continue };
        if !ft.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            // Non-UTF-8 names cannot be pruned by name; still descend.
            visit(&entry.path(), depth + 1, max_depth, idx);
            continue;
        };
        if name.starts_with('.')
            || PRUNED_NAMES.contains(&name)
            || (depth == 0 && PRUNED_TOP_NAMES.contains(&name))
        {
            continue;
        }
        visit(&entry.path(), depth + 1, max_depth, idx);
    }
}

/// Where a source project lands on the target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mapping {
    Auto(PathBuf),
    /// Several target repositories share the remote; the user picks one.
    Choose(Vec<PathBuf>),
    /// No match; the project's data waits in the pending store.
    Pending,
}

/// Applies the matching rules of spec 7 in order. `r.remote` is already
/// normalized (see `ProjectRef`). A path under the source home maps to the
/// same place under `dst_home` only when that folder exists there.
pub fn map_project(r: &ProjectRef, idx: &TargetIndex, src_home: &str, dst_home: &Path) -> Mapping {
    if let Some(remote) = &r.remote {
        return match idx.by_remote.get(remote).map(Vec::as_slice) {
            Some([one]) => Mapping::Auto(one.clone()),
            Some(many) if !many.is_empty() => Mapping::Choose(many.to_vec()),
            _ => Mapping::Pending,
        };
    }
    if r.is_git {
        return Mapping::Pending;
    }
    let old = Path::new(&r.old_path);
    if !old.is_absolute() || old.components().any(|c| c == Component::ParentDir) {
        return Mapping::Pending;
    }
    if let Some(rest) = usable_home(src_home).and_then(|home| strip_home(&r.old_path, home)) {
        let rest = rest.trim_start_matches('/');
        let path = if rest.is_empty() {
            dst_home.to_path_buf()
        } else {
            dst_home.join(rest)
        };
        // The plan never creates a project folder (spec 3, 7): a folder
        // missing here waits in the pending store until `link`.
        return if path.is_dir() {
            Mapping::Auto(path)
        } else {
            Mapping::Pending
        };
    }
    if old.is_dir() {
        return Mapping::Auto(old.to_path_buf());
    }
    Mapping::Pending
}

/// `path` relative to `home` when it is `home` itself or below it.
fn strip_home<'a>(path: &'a str, home: &str) -> Option<&'a str> {
    let rest = path.strip_prefix(home)?;
    (rest.is_empty() || rest.starts_with('/')).then_some(rest)
}

/// `home` without one trailing `/`; `None` for an empty or root home,
/// which would match every path, so no home rewriting applies.
fn usable_home(home: &str) -> Option<&str> {
    let home = trim_home(home);
    (!home.is_empty() && home != "/").then_some(home)
}

/// Drops one trailing `/`, except for the root itself.
fn trim_home(home: &str) -> &str {
    match home.strip_suffix('/') {
        Some(trimmed) if !trimmed.is_empty() => trimmed,
        _ => home,
    }
}

/// Replaces `src_home` with `dst_home` wherever the character after it is
/// `/`, `"`, `'`, a space, or the end of the string. Other occurrences
/// (e.g. `/Users/alice2` for `/Users/alice`) stay untouched. An empty or
/// root `src_home` rewrites nothing.
pub fn rewrite_home(s: &str, src_home: &str, dst_home: &str) -> String {
    let Some(src) = usable_home(src_home) else {
        return s.to_owned();
    };
    let dst = trim_home(dst_home);
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(pos) = rest.find(src) {
        let after = &rest[pos + src.len()..];
        let boundary = matches!(after.chars().next(), None | Some('/' | '"' | '\'' | ' '));
        if boundary {
            out.push_str(&rest[..pos]);
            out.push_str(dst);
            rest = after;
        } else {
            // Advance one character so overlapping candidates are still seen.
            let step = pos + rest[pos..].chars().next().map_or(1, char::len_utf8);
            out.push_str(&rest[..step]);
            rest = &rest[step..];
        }
    }
    out.push_str(rest);
    out
}
