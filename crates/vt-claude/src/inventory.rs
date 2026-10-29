//! What of the Claude data is transferred, and why the rest is not.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use vt_core::manifest::{Category, Class, Entry, EntryKind, ProjectRef, RootKind, Selection, Tool};
use vt_core::scan::{RawEntry, scan_tree};

use crate::ClaudeAdapter;
use crate::projects::{UNREADABLE, scan_project, unreadable_dir};

const UNSELECTED: &str = "kullanıcı seçimiyle atlandı";
const NEVER: &str = "asla taşınmaz: makineye özgü";
const NEVER_SECRET: &str = "dosya olarak asla taşınmaz: sır, Sırlar kategorisiyle aktarılır";
const UNCLASSIFIED: &str = "sınıflandırılmamış";
const UNCLASSIFIED_NOTE: &str = "sınıflandırılmamış: isteğe bağlı öğe olarak eklendi";
const SPECIAL: &str = "özel dosya";
const EMPTY_DIR: &str = "içinde aktarılacak öğe yok";
const MISSING_PROJECT: &str = "proje klasörü bu makinede yok";
const INSIDE_CLAUDE_DIR: &str = "Claude klasörünün içinde, ayrıca taşınmaz";

/// Everything the Claude adapter would transfer for a selection.
///
/// `skipped` and `notes` hold `(key, reason)`. The key is the `rel` path for
/// `~/.claude` items, `claude_json` for `~/.claude.json`, and
/// `<root tag>:<rel>` (or just the tag for a whole root) otherwise, e.g.
/// `agents_dir:skills` or `project:github.com/acme/app`.
#[derive(Debug, Clone, Default)]
pub struct Inventory {
    /// Entries sorted by root then `rel`, with sequential `index`; the path
    /// is the absolute source. `hash` is filled when the bundle is written.
    pub entries: Vec<(Entry, PathBuf)>,
    pub projects: Vec<ProjectRef>,
    /// Items left out, with the reason.
    pub skipped: Vec<(String, String)>,
    /// Included items worth a report line (e.g. unclassified ones).
    pub notes: Vec<(String, String)>,
}

/// Classification of a `~/.claude` path, before the user's selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Rule {
    /// A directory whose children are classified one by one.
    Container,
    Item {
        class: Class,
        category: Category,
        /// False when the path is not in the research table.
        listed: bool,
    },
}

const fn item(class: Class, category: Category) -> Rule {
    Rule::Item {
        class,
        category,
        listed: true,
    }
}

const UNLISTED: Rule = Rule::Item {
    class: Class::Optional,
    category: Category::Cache,
    listed: false,
};

/// Class and category of a path relative to `~/.claude` (research table,
/// section 6). `None` marks a container directory (`projects`,
/// `projects/<slug>`, `plugins`) whose children are classified separately.
/// Paths outside the table are optional cache.
pub fn classify(rel_in_claude_dir: &str) -> Option<(Class, Category)> {
    match rule(rel_in_claude_dir) {
        Rule::Container => None,
        Rule::Item {
            class, category, ..
        } => Some((class, category)),
    }
}

fn rule(rel: &str) -> Rule {
    use Category::{Cache, Memory, Secrets, Sessions, Settings};
    use Class::{Never, Optional, Transfer};

    if rel.is_empty() {
        return Rule::Container;
    }
    let parts: Vec<&str> = rel.split('/').collect();
    let first = parts[0];
    let second = parts.get(1).copied();
    if first == ".credentials.json" {
        return item(Never, Secrets);
    }
    if is_never(first, second, parts[parts.len() - 1]) {
        return item(Never, Cache);
    }
    match first {
        "projects" => match parts.get(2) {
            None => Rule::Container,
            Some(&"memory") => item(Transfer, Memory),
            Some(_) => item(Transfer, Sessions),
        },
        "history.jsonl" | "file-history" | "paste-cache" | "plans" | "tasks" | "todos" => {
            item(Transfer, Sessions)
        }
        "CLAUDE.md" | "agents" | "commands" | "skills" | "hooks" | "scripts" | "bin" | "docs"
        | "references" => item(Transfer, Settings),
        _ if parts.len() == 1 && first.starts_with("settings") && first.ends_with(".json") => {
            item(Transfer, Settings)
        }
        "plugins" => match second {
            None => Rule::Container,
            Some("installed_plugins.json" | "known_marketplaces.json" | "data") => {
                item(Transfer, Settings)
            }
            Some("cache" | "marketplaces") => item(Optional, Cache),
            Some(_) => UNLISTED,
        },
        "backups" | "cache" | "logs" | "stats-cache.json" | "context-mode" | "feedback"
        | ".git" | ".gitignore" | ".gitmodules" => item(Optional, Cache),
        _ if first.starts_with(".caveman-") || first.starts_with("security_warnings_state_") => {
            item(Optional, Cache)
        }
        _ => UNLISTED,
    }
}

/// Machine-specific paths that are never transferred.
fn is_never(first: &str, second: Option<&str>, name: &str) -> bool {
    name.ends_with(".lock")
        || first.starts_with("daemon")
        || first.starts_with(".claude.json.tmp.")
        || first == ".claude.json.backup"
        || matches!(
            first,
            "sessions"
                | "jobs"
                | "ide"
                | "shell-snapshots"
                | "session-env"
                | "telemetry"
                | "downloads"
                | "chrome"
                | "debug"
                | ".last-cleanup"
                | ".device-keys.json"
        )
        || matches!(
            (first, second),
            ("plugins", Some("synced"))
                | ("security", Some("agent-sdk-venv"))
                | (".claude", Some("worktrees"))
        )
}

fn selected(sel: &Selection, category: Category) -> bool {
    sel.tools.contains(&Tool::Claude)
        && sel
            .categories
            .get(&Tool::Claude)
            .is_some_and(|c| c.contains(&category))
}

/// What happens to one scanned path.
enum Decision {
    Include(Category, Option<&'static str>),
    Container,
    Skip(String),
    /// Not Claude data, or handled as another root: left out silently.
    Outside,
}

fn decide(rule: Rule, sel: &Selection) -> Decision {
    match rule {
        Rule::Container => Decision::Container,
        Rule::Item {
            class: Class::Never,
            category: Category::Secrets,
            ..
        } => Decision::Skip(NEVER_SECRET.into()),
        Rule::Item {
            class: Class::Never,
            ..
        } => Decision::Skip(NEVER.into()),
        Rule::Item {
            category, listed, ..
        } => {
            if selected(sel, category) {
                Decision::Include(category, (!listed).then_some(UNCLASSIFIED_NOTE))
            } else if listed {
                Decision::Skip(UNSELECTED.into())
            } else {
                Decision::Skip(format!("{UNCLASSIFIED}; {UNSELECTED}"))
            }
        }
    }
}

impl ClaudeAdapter {
    /// Lists the Claude items to transfer for `sel`: `~/.claude`,
    /// `~/.claude.json`, `~/.agents/skills` (with Settings) and the Claude
    /// files of each project that exists here (with ProjectFiles). Symlinks
    /// are recorded, never followed. Every item left out is in `skipped`.
    pub fn inventory(&self, sel: &Selection) -> Result<Inventory> {
        let env = &self.env;
        let mut c = Collector {
            sel,
            entries: Vec::new(),
            skipped: Vec::new(),
            notes: Vec::new(),
        };

        if env.claude_dir.is_dir() {
            // With CLAUDE_CONFIG_DIR, `.claude.json` sits inside the Claude dir
            // but is inventoried as its own root.
            let json_rel = env
                .claude_json
                .strip_prefix(&env.claude_dir)
                .ok()
                .and_then(Path::to_str)
                .map(str::to_owned);
            c.add_tree(&RootKind::ClaudeDir, &env.claude_dir, &|rel| {
                if json_rel.as_deref() == Some(rel) {
                    Decision::Outside
                } else {
                    decide(rule(rel), sel)
                }
            });
        }

        c.add_claude_json(&env.claude_json);

        if fs::symlink_metadata(env.agents_dir.join("skills")).is_ok() {
            let settings = item(Class::Transfer, Category::Settings);
            c.add_tree(&RootKind::AgentsDir, &env.agents_dir, &|rel| {
                if rel.split('/').next() == Some("skills") {
                    decide(settings, sel)
                } else {
                    Decision::Outside
                }
            });
        }

        let projects = self.collect_projects()?;
        c.skipped.extend(projects.skipped);
        c.notes.extend(projects.notes);
        for project in &projects.refs {
            c.add_project(project, &env.claude_dir);
        }

        let mut entries = c.entries;
        entries.sort_by(|(a, _), (b, _)| (&a.root, &a.rel).cmp(&(&b.root, &b.rel)));
        for (i, (entry, _)) in entries.iter_mut().enumerate() {
            entry.index = u32::try_from(i).context("too many entries")?;
        }
        Ok(Inventory {
            entries,
            projects: projects.refs,
            skipped: c.skipped,
            notes: c.notes,
        })
    }
}

struct Collector<'a> {
    sel: &'a Selection,
    entries: Vec<(Entry, PathBuf)>,
    skipped: Vec<(String, String)>,
    notes: Vec<(String, String)>,
}

fn key(root: &RootKind, rel: &str) -> String {
    match (root, rel) {
        (RootKind::ClaudeDir, _) => rel.to_owned(),
        (_, "") => root.tag(),
        _ => format!("{}:{rel}", root.tag()),
    }
}

impl Collector<'_> {
    fn skip(&mut self, root: &RootKind, rel: &str, reason: impl Into<String>) {
        self.skipped.push((key(root, rel), reason.into()));
    }

    /// Scans `base` and adds what `decide` includes. Excluded subtrees are
    /// reported once, at their top.
    fn add_tree(&mut self, root: &RootKind, base: &Path, decide_rel: &dyn Fn(&str) -> Decision) {
        let pruned = RefCell::new(Vec::new());
        let exclude = |rel: &str| match decide_rel(rel) {
            Decision::Skip(reason) => {
                pruned.borrow_mut().push((rel.to_owned(), reason));
                true
            }
            Decision::Outside => true,
            Decision::Include(..) | Decision::Container => match unreadable_dir(&base.join(rel)) {
                Some(err) => {
                    pruned
                        .borrow_mut()
                        .push((rel.to_owned(), format!("{UNREADABLE}: {err}")));
                    true
                }
                None => false,
            },
        };
        let scan = match scan_tree(base, &exclude) {
            Ok(scan) => scan,
            Err(e) => {
                self.skip(root, "", format!("{UNREADABLE}: {e:#}"));
                return;
            }
        };
        // `scan_tree` visits in read_dir order; report in a stable order.
        let mut pruned = pruned.into_inner();
        pruned.sort();
        for (rel, reason) in pruned {
            self.skip(root, &rel, reason);
        }
        for special in scan.skipped {
            self.skip(root, &special.rel, format!("{SPECIAL}: {}", special.reason));
        }

        let mut included = Vec::new();
        let mut containers = Vec::new();
        for raw in scan.entries {
            let mut decision = decide_rel(&raw.rel);
            if matches!(decision, Decision::Container) && raw.kind != EntryKind::Dir {
                decision = decide(UNLISTED, self.sel);
            }
            match decision {
                Decision::Include(category, note) => {
                    if let Some(note) = note {
                        self.notes.push((key(root, &raw.rel), note.into()));
                    }
                    included.push((raw, category));
                }
                Decision::Container => containers.push(raw),
                Decision::Skip(reason) => self.skip(root, &raw.rel, reason),
                Decision::Outside => {}
            }
        }
        self.finish(root, base, included, containers, true);
    }

    /// Keeps the container directories that lead to an included entry (with
    /// that entry's category) and records all included entries. The other
    /// containers are reported when `report_empty` is set.
    fn finish(
        &mut self,
        root: &RootKind,
        base: &Path,
        mut included: Vec<(RawEntry, Category)>,
        containers: Vec<RawEntry>,
        report_empty: bool,
    ) {
        let mut ancestors: HashMap<String, Category> = HashMap::new();
        for (raw, category) in &included {
            let mut rel = raw.rel.as_str();
            while let Some((parent, _)) = rel.rsplit_once('/') {
                ancestors.entry(parent.to_owned()).or_insert(*category);
                rel = parent;
            }
        }
        for raw in containers {
            match ancestors.get(&raw.rel) {
                Some(category) => included.push((raw, *category)),
                None if report_empty => self.skip(root, &raw.rel, EMPTY_DIR),
                None => {}
            }
        }
        for (raw, category) in included {
            let path = if raw.rel.is_empty() {
                base.to_path_buf()
            } else {
                base.join(&raw.rel)
            };
            let entry = Entry {
                index: 0,
                tool: Tool::Claude,
                category,
                root: root.clone(),
                rel: raw.rel,
                kind: raw.kind,
                size: raw.size,
                mtime_ns: raw.mtime_ns,
                mode: raw.mode,
                hash: None,
                partial_tail: false,
            };
            self.entries.push((entry, path));
        }
    }

    /// `~/.claude.json` as a single-file root with an empty `rel`.
    fn add_claude_json(&mut self, path: &Path) {
        let root = RootKind::ClaudeJson;
        if fs::symlink_metadata(path).is_err() {
            return;
        }
        if !selected(self.sel, Category::Settings) {
            self.skip(&root, "", UNSELECTED);
            return;
        }
        let (Some(parent), Some(name)) = (path.parent(), path.file_name().and_then(|n| n.to_str()))
        else {
            self.skip(&root, "", format!("{UNREADABLE}: geçersiz yol"));
            return;
        };
        let found = scan_tree(parent, &|rel| rel != name)
            .map(|scan| scan.entries.into_iter().find(|e| e.rel == name));
        match found {
            Ok(Some(mut raw)) => {
                raw.rel = String::new();
                self.finish(
                    &root,
                    path,
                    vec![(raw, Category::Settings)],
                    Vec::new(),
                    true,
                );
            }
            Ok(None) => self.skip(&root, "", format!("{SPECIAL}: other")),
            Err(e) => self.skip(&root, "", format!("{UNREADABLE}: {e:#}")),
        }
    }

    /// Claude files inside one project, when it exists on this machine.
    fn add_project(&mut self, project: &ProjectRef, claude_dir: &Path) {
        let root_kind = RootKind::Project(project.id.clone());
        if !selected(self.sel, Category::ProjectFiles) {
            self.skip(&root_kind, "", UNSELECTED);
            return;
        }
        let root = Path::new(&project.old_path);
        if !root.is_dir() {
            self.skip(&root_kind, "", MISSING_PROJECT);
            return;
        }
        if root.starts_with(claude_dir) {
            self.skip(&root_kind, "", INSIDE_CLAUDE_DIR);
            return;
        }
        let skip_dot_claude = same_path(&root.join(".claude"), claude_dir);
        let (scan, unreadable) = match scan_project(root, skip_dot_claude) {
            Ok(found) => found,
            Err(e) => {
                self.skip(&root_kind, "", format!("{UNREADABLE}: {e:#}"));
                return;
            }
        };
        for (rel, reason) in unreadable {
            self.skip(&root_kind, &rel, reason);
        }
        for special in scan.skipped {
            self.skip(
                &root_kind,
                &special.rel,
                format!("{SPECIAL}: {}", special.reason),
            );
        }
        let (containers, included): (Vec<_>, Vec<_>) = scan.entries.into_iter().partition(|e| {
            e.kind == EntryKind::Dir && e.rel != ".claude" && !e.rel.starts_with(".claude/")
        });
        let included = included
            .into_iter()
            .map(|raw| (raw, Category::ProjectFiles))
            .collect();
        // Search-only directories without a CLAUDE.md are not Claude data.
        self.finish(&root_kind, root, included, containers, false);
    }
}

fn same_path(a: &Path, b: &Path) -> bool {
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}
