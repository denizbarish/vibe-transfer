//! Which projects Claude data belongs to, and the AI files inside them.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::Value;
use vt_core::manifest::{ProjectId, ProjectRef};
use vt_core::remote::{normalize_remote, read_origin};
use vt_core::scan::{ScanResult, scan_tree};

use crate::ClaudeAdapter;
use crate::claude_json::read_claude_json_stable;

/// Lines read from the top of each session jsonl when looking for `cwd`.
const CWD_SCAN_LINES: usize = 50;
/// Deepest subdirectory level searched for nested `CLAUDE.md` files.
const NESTED_CLAUDE_MD_DEPTH: usize = 3;
/// Directories never searched for nested `CLAUDE.md` (code, not AI files).
const PRUNED_DIRS: &[&str] = &[".git", "node_modules", "vendor", ".next", "target", ".venv"];
/// Project root files that belong to Claude Code.
const ROOT_FILES: &[&str] = &["CLAUDE.md", "CLAUDE.local.md", ".mcp.json"];

pub(crate) const UNREADABLE: &str = "okunamadı";

/// Projects found plus what could not be used, keyed like `Inventory`.
pub(crate) struct ProjectScan {
    pub refs: Vec<ProjectRef>,
    pub skipped: Vec<(String, String)>,
    pub notes: Vec<(String, String)>,
    /// Why `~/.claude.json` could not be used, if it exists but did not parse.
    pub claude_json_error: Option<String>,
}

impl ClaudeAdapter {
    /// Projects referenced by Claude data, one per project identity.
    ///
    /// Old paths come, in this order, from session jsonl `cwd` fields (first
    /// 50 lines of each file), `~/.claude.json` `projects` keys and
    /// `githubRepoPaths`. Each path is folded into the nearest enclosing git
    /// root; the id is the normalized `origin` remote, else the
    /// `githubRepoPaths` repo, else `path:<root>`. A `~/.claude.json` that
    /// cannot be read is left out here; `inventory` reports it.
    pub fn project_refs(&self) -> Result<Vec<ProjectRef>> {
        Ok(self.collect_projects()?.refs)
    }

    pub(crate) fn collect_projects(&self) -> Result<ProjectScan> {
        let mut notes = Vec::new();
        let mut candidates = Candidates::default();
        for cwd in session_cwds(&self.env.claude_dir, &mut notes) {
            candidates.add(cwd, None);
        }
        let mut claude_json_error = None;
        let json = if fs::symlink_metadata(&self.env.claude_json).is_ok() {
            read_claude_json_stable(&self.env.claude_json)
                .map_err(|e| claude_json_error = Some(format!("{e:#}")))
                .ok()
        } else {
            None
        };
        if let Some(json) = json {
            if let Some(projects) = json.get("projects").and_then(Value::as_object) {
                for path in projects.keys() {
                    candidates.add(path.clone(), None);
                }
            }
            if let Some(repos) = json.get("githubRepoPaths").and_then(Value::as_object) {
                for (repo, paths) in repos {
                    let remote = normalize_remote(&format!("https://github.com/{repo}"));
                    let paths = paths.as_array().into_iter().flatten();
                    for path in paths.filter_map(Value::as_str) {
                        candidates.add(path.to_owned(), remote.clone());
                    }
                }
            }
        }

        let mut refs: Vec<ProjectRef> = Vec::new();
        let mut skipped = Vec::new();
        // id -> (index in refs, whether its `.git` is a directory)
        let mut by_id: HashMap<ProjectId, (usize, bool)> = HashMap::new();
        for (path, hint) in candidates.list {
            if !Path::new(&path).is_absolute() {
                skipped.push((
                    format!("project:{path}"),
                    "mutlak olmayan proje yolu".into(),
                ));
                continue;
            }
            let (project, main_checkout) = resolve(&path, hint);
            match by_id.get(&project.id) {
                None => {
                    by_id.insert(project.id.clone(), (refs.len(), main_checkout));
                    refs.push(project);
                }
                Some(&(i, kept_main)) => {
                    if refs[i].old_path == project.old_path {
                        continue;
                    }
                    // Prefer the main checkout over a linked worktree.
                    let dropped = if main_checkout && !kept_main {
                        by_id.insert(project.id.clone(), (i, true));
                        std::mem::replace(&mut refs[i].old_path, project.old_path)
                    } else {
                        project.old_path
                    };
                    skipped.push((
                        format!("project:{dropped}"),
                        format!(
                            "aynı proje ({}) başka bir yolda da var; {} kullanıldı",
                            project.id.0, refs[i].old_path
                        ),
                    ));
                }
            }
        }
        Ok(ProjectScan {
            refs,
            skipped,
            notes,
            claude_json_error,
        })
    }
}

/// Candidate project paths in first-seen order, with an optional remote hint.
#[derive(Default)]
struct Candidates {
    list: Vec<(String, Option<String>)>,
    seen: HashMap<String, usize>,
}

impl Candidates {
    fn add(&mut self, path: String, hint: Option<String>) {
        let path = clean(&path);
        match self.seen.get(&path) {
            Some(&i) => {
                if self.list[i].1.is_none() {
                    self.list[i].1 = hint;
                }
            }
            None => {
                self.seen.insert(path.clone(), self.list.len());
                self.list.push((path, hint));
            }
        }
    }
}

/// Drops a trailing `/` and `.` components without touching the file system.
fn clean(path: &str) -> String {
    let cleaned: PathBuf = Path::new(path).components().collect();
    match cleaned.to_str() {
        Some(s) if !s.is_empty() => s.to_owned(),
        _ => path.to_owned(),
    }
}

/// Builds the project reference for one old path. The flag is false when
/// the git root is a linked worktree or submodule (`.git` is a file).
fn resolve(path: &str, hint: Option<String>) -> (ProjectRef, bool) {
    let git_root = Path::new(path).ancestors().find_map(|dir| {
        fs::symlink_metadata(dir.join(".git"))
            .ok()
            .map(|md| (dir.to_path_buf(), md.is_dir()))
    });
    let (root, remote, is_git, main_checkout) = match git_root {
        Some((root, dot_git_dir)) => {
            let remote = read_origin(&root)
                .and_then(|url| normalize_remote(&url))
                .or(hint);
            (
                root.to_string_lossy().into_owned(),
                remote,
                true,
                dot_git_dir,
            )
        }
        None => (path.to_owned(), hint, false, true),
    };
    let id = ProjectId(remote.clone().unwrap_or_else(|| format!("path:{root}")));
    let project = ProjectRef {
        id,
        old_path: root,
        remote,
        is_git,
    };
    (project, main_checkout)
}

/// `cwd` values from the first lines of every `projects/<slug>/*.jsonl`.
/// Malformed lines are skipped; unreadable files are noted.
fn session_cwds(claude_dir: &Path, notes: &mut Vec<(String, String)>) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for slug in sorted_dir(&claude_dir.join("projects")) {
        if !fs::symlink_metadata(&slug).is_ok_and(|md| md.is_dir()) {
            continue;
        }
        for file in sorted_dir(&slug) {
            let is_jsonl = file.extension().is_some_and(|e| e == "jsonl");
            if !is_jsonl || !fs::symlink_metadata(&file).is_ok_and(|md| md.is_file()) {
                continue;
            }
            if let Err(e) = first_cwds(&file, &mut out, &mut seen) {
                let rel = file.strip_prefix(claude_dir).unwrap_or(&file);
                notes.push((
                    rel.to_string_lossy().into_owned(),
                    format!("proje yolu aranırken {UNREADABLE}: {e}"),
                ));
            }
        }
    }
    out
}

pub(crate) fn first_cwds(
    path: &Path,
    out: &mut Vec<String>,
    seen: &mut HashSet<String>,
) -> std::io::Result<()> {
    let mut reader = BufReader::new(File::open(path)?);
    let mut line = Vec::new();
    for _ in 0..CWD_SCAN_LINES {
        line.clear();
        if reader.read_until(b'\n', &mut line)? == 0 {
            break;
        }
        let Ok(value) = serde_json::from_slice::<Value>(&line) else {
            continue;
        };
        if let Some(cwd) = value.get("cwd").and_then(Value::as_str)
            && seen.insert(cwd.to_owned())
        {
            out.push(cwd.to_owned());
        }
    }
    Ok(())
}

/// Children of `dir` sorted by name; empty when it cannot be read.
fn sorted_dir(dir: &Path) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = match fs::read_dir(dir) {
        Ok(read) => read.filter_map(|e| e.ok().map(|e| e.path())).collect(),
        Err(_) => Vec::new(),
    };
    paths.sort();
    paths
}

/// Claude Code files inside a project, as `/`-separated relative paths:
/// `CLAUDE.md`, `CLAUDE.local.md`, `.mcp.json`, everything under `.claude/`
/// except `.claude/worktrees/`, and `CLAUDE.md` in subdirectories up to 3
/// levels deep. Directories are not listed. Codex files (`AGENTS.md`,
/// `.codex/`) are not Claude's. An unreadable project gives an empty list;
/// the inventory reports such errors instead.
pub fn project_inner_files(project_root: &Path) -> Vec<String> {
    let opts = ProjectScanOptions {
        skip_dot_claude: false,
        nested: true,
        excluded: &[],
    };
    match scan_project(project_root, &opts) {
        Ok((scan, _)) => scan
            .entries
            .into_iter()
            .filter(|e| e.kind != vt_core::manifest::EntryKind::Dir)
            .map(|e| e.rel)
            .collect(),
        Err(_) => Vec::new(),
    }
}

/// How [`scan_project`] walks one project.
pub(crate) struct ProjectScanOptions<'a> {
    /// Leave out `.claude/`: the home directory as a project, whose
    /// `.claude/` is the Claude dir itself (already its own root).
    pub skip_dot_claude: bool,
    /// Search subdirectories for nested `CLAUDE.md` files.
    pub nested: bool,
    /// Other project roots; never walked into.
    pub excluded: &'a [&'a Path],
}

/// Scans the Claude files of a project. Directories walked only to find
/// nested `CLAUDE.md` files are part of the result; callers keep those that
/// lead to an included file. The second value lists unreadable directories
/// under `.claude/`.
pub(crate) fn scan_project(
    root: &Path,
    opts: &ProjectScanOptions,
) -> Result<(ScanResult, Vec<(String, String)>)> {
    let unreadable = RefCell::new(Vec::new());
    let exclude = |rel: &str| {
        let parts: Vec<&str> = rel.split('/').collect();
        let path = root.join(rel);
        if opts.excluded.contains(&path.as_path()) {
            return true;
        }
        if parts[0] == ".claude" {
            if opts.skip_dot_claude || parts.get(1) == Some(&"worktrees") {
                return true;
            }
            if let Some(err) = unreadable_dir(&path) {
                unreadable
                    .borrow_mut()
                    .push((rel.to_owned(), format!("{UNREADABLE}: {err}")));
                return true;
            }
            return false;
        }
        let Ok(md) = fs::symlink_metadata(&path) else {
            return true;
        };
        let name = parts[parts.len() - 1];
        if md.is_dir() {
            // Search locations only: an unreadable one is not reported.
            return !opts.nested
                || parts.len() > NESTED_CLAUDE_MD_DEPTH
                || PRUNED_DIRS.contains(&name)
                || fs::read_dir(&path).is_err();
        }
        if parts.len() == 1 {
            !ROOT_FILES.contains(&name)
        } else {
            name != "CLAUDE.md"
        }
    };
    let scan = scan_tree(root, &exclude)?;
    Ok((scan, unreadable.into_inner()))
}

/// The read error of a directory that cannot be listed; `None` for
/// readable directories and anything that is not a directory.
pub(crate) fn unreadable_dir(path: &Path) -> Option<std::io::Error> {
    let md = fs::symlink_metadata(path).ok()?;
    if !md.is_dir() {
        return None;
    }
    fs::read_dir(path).err()
}
