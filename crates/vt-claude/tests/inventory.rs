// Fixtures use symlinks and POSIX modes.
#![cfg(unix)]

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use vt_claude::{ClaudeAdapter, Inventory, classify, project_inner_files, read_claude_json_stable};
use vt_core::VtEnv;
use vt_core::manifest::{Category, Class, EntryKind, ProjectId, RootKind, Selection, Tool};

const UNSELECTED: &str = "kullanıcı seçimiyle atlandı";
const HOME_PLACEHOLDER: &str = "@HOME@";

/// Copies the synthetic fixture home into a tempdir, fills in the
/// placeholder home path and adds what git cannot hold as fixture files: a
/// project `.git/config` and the `~/.claude/skills` symlink.
fn setup() -> (tempfile::TempDir, VtEnv) {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/home");
    copy_tree(&src, &home, home.to_str().unwrap());

    let git = home.join("code/app/.git");
    fs::create_dir_all(&git).unwrap();
    fs::write(
        git.join("config"),
        "[remote \"origin\"]\n\turl = git@github.com:acme/app.git\n",
    )
    .unwrap();
    std::os::unix::fs::symlink(
        "../../.agents/skills/shared",
        home.join(".claude/skills/shared"),
    )
    .unwrap();

    let env = VtEnv::new(home, "alice".into(), None);
    (tmp, env)
}

fn copy_tree(src: &Path, dst: &Path, home: &str) {
    fs::create_dir_all(dst).unwrap();
    for item in fs::read_dir(src).unwrap() {
        let item = item.unwrap();
        let to = dst.join(item.file_name());
        if item.file_type().unwrap().is_dir() {
            copy_tree(&item.path(), &to, home);
        } else {
            let text = fs::read_to_string(item.path()).unwrap();
            fs::write(&to, text.replace(HOME_PLACEHOLDER, home)).unwrap();
            let mode = fs::metadata(item.path()).unwrap().permissions();
            fs::set_permissions(&to, mode).unwrap();
        }
    }
}

fn selection(categories: &[Category]) -> Selection {
    Selection {
        tools: BTreeSet::from([Tool::Claude]),
        categories: BTreeMap::from([(Tool::Claude, categories.iter().copied().collect())]),
    }
}

const DEFAULT: &[Category] = &[
    Category::Sessions,
    Category::Memory,
    Category::Settings,
    Category::Secrets,
    Category::ProjectFiles,
];

fn rels(inv: &Inventory, root: &RootKind) -> Vec<String> {
    inv.entries
        .iter()
        .filter(|(e, _)| &e.root == root)
        .map(|(e, _)| e.rel.clone())
        .collect()
}

fn skip_reason<'a>(inv: &'a Inventory, rel: &str) -> Option<&'a str> {
    inv.skipped
        .iter()
        .find(|(r, _)| r == rel)
        .map(|(_, reason)| reason.as_str())
}

#[test]
fn classify_table() {
    use Category::*;
    use Class::*;
    let cases: &[(&str, Option<(Class, Category)>)] = &[
        // Transfer: sessions
        ("projects/-a/s1.jsonl", Some((Transfer, Sessions))),
        (
            "projects/-a/s1/subagents/agent-1.jsonl",
            Some((Transfer, Sessions)),
        ),
        (
            "projects/-a/s1/tool-results/t.txt",
            Some((Transfer, Sessions)),
        ),
        ("history.jsonl", Some((Transfer, Sessions))),
        ("file-history/s1/abc@v1", Some((Transfer, Sessions))),
        ("paste-cache/aa.txt", Some((Transfer, Sessions))),
        ("plans/p.md", Some((Transfer, Sessions))),
        ("tasks/s1/1.json", Some((Transfer, Sessions))),
        ("todos/t.json", Some((Transfer, Sessions))),
        // Transfer: memory
        ("projects/-a/memory/MEMORY.md", Some((Transfer, Memory))),
        ("projects/-a/memory", Some((Transfer, Memory))),
        // Transfer: settings
        ("CLAUDE.md", Some((Transfer, Settings))),
        ("settings.json", Some((Transfer, Settings))),
        ("settings.local.json", Some((Transfer, Settings))),
        ("agents/reviewer.md", Some((Transfer, Settings))),
        ("commands/c.md", Some((Transfer, Settings))),
        ("skills/s/SKILL.md", Some((Transfer, Settings))),
        ("hooks/fmt.sh", Some((Transfer, Settings))),
        ("scripts/x.sh", Some((Transfer, Settings))),
        ("bin/tool", Some((Transfer, Settings))),
        ("plugins/installed_plugins.json", Some((Transfer, Settings))),
        (
            "plugins/known_marketplaces.json",
            Some((Transfer, Settings)),
        ),
        ("plugins/data/demo-mkt/state.db", Some((Transfer, Settings))),
        // Optional
        ("backups/.claude.json.backup.1", Some((Optional, Cache))),
        ("cache/changelog.md", Some((Optional, Cache))),
        ("logs/hook.log", Some((Optional, Cache))),
        ("stats-cache.json", Some((Optional, Cache))),
        ("plugins/cache/demo/plugin.json", Some((Optional, Cache))),
        (
            "plugins/marketplaces/acme/README.md",
            Some((Optional, Cache)),
        ),
        ("settings.json.bak", Some((Optional, Cache))),
        ("mystery.txt", Some((Optional, Cache))),
        // Never
        ("daemon/control.key", Some((Never, Cache))),
        ("daemon.lock", Some((Never, Cache))),
        ("sessions/x.json", Some((Never, Cache))),
        ("jobs/state.json", Some((Never, Cache))),
        ("ide/123.lock", Some((Never, Cache))),
        ("tasks/s1/.lock", Some((Never, Cache))),
        ("history.lock", Some((Never, Cache))),
        // Lock files in user data are user content.
        ("skills/x/bun.lock", Some((Transfer, Settings))),
        ("projects/-a/memory/notes.lock", Some((Transfer, Memory))),
        ("plugins/data/demo/yarn.lock", Some((Transfer, Settings))),
        ("shell-snapshots/snapshot-zsh-1.sh", Some((Never, Cache))),
        ("session-env/s1", Some((Never, Cache))),
        ("telemetry/1p_failed_events.json", Some((Never, Cache))),
        ("downloads/claude-bin", Some((Never, Cache))),
        ("chrome/chrome-native-host", Some((Never, Cache))),
        ("debug/latest", Some((Never, Cache))),
        (".last-cleanup", Some((Never, Cache))),
        ("plugins/synced/acme/x.json", Some((Never, Cache))),
        ("security/agent-sdk-venv/bin/python", Some((Never, Cache))),
        (".device-keys.json", Some((Never, Cache))),
        (".claude/worktrees/wt/README.md", Some((Never, Cache))),
        (".credentials.json", Some((Never, Secrets))),
        // Containers: children are classified one by one.
        ("projects", None),
        ("projects/-a", None),
        ("plugins", None),
    ];
    for (rel, want) in cases {
        assert_eq!(classify(rel), *want, "classify({rel:?})");
    }
}

#[test]
fn inventory_respects_selection() {
    let (_tmp, env) = setup();
    let adapter = ClaudeAdapter { env: env.clone() };

    let without_memory: Vec<Category> = DEFAULT
        .iter()
        .copied()
        .filter(|c| *c != Category::Memory)
        .collect();
    let inv = adapter.inventory(&selection(&without_memory)).unwrap();
    assert!(
        inv.entries
            .iter()
            .all(|(e, _)| e.category != Category::Memory),
        "memory entry present while Memory is off"
    );
    assert_eq!(
        skip_reason(&inv, "projects/-Users-src-app/memory"),
        Some(UNSELECTED)
    );
    let claude = rels(&inv, &RootKind::ClaudeDir);
    assert!(claude.contains(&"projects/-Users-src-app/s-app.jsonl".to_owned()));

    // Never-class items are reported, not silently dropped.
    for never in [
        "daemon",
        "sessions",
        "shell-snapshots",
        "tasks/s1/.lock",
        "plugins/synced",
    ] {
        let reason = skip_reason(&inv, never).unwrap_or_else(|| panic!("{never} not reported"));
        assert!(reason.starts_with("asla taşınmaz"), "{never}: {reason}");
        assert!(
            !claude
                .iter()
                .any(|r| r == never || r.starts_with(&format!("{never}/")))
        );
    }
    // Optional items stay out by default and are reported as unselected.
    assert_eq!(skip_reason(&inv, "cache"), Some(UNSELECTED));
    assert_eq!(skip_reason(&inv, "plugins/cache"), Some(UNSELECTED));
    let mystery = skip_reason(&inv, "mystery.txt").unwrap();
    assert!(mystery.contains("sınıflandırılmamış") && mystery.contains(UNSELECTED));

    // Turning Memory and Cache on brings them in.
    let mut all = DEFAULT.to_vec();
    all.push(Category::Cache);
    let inv = adapter.inventory(&selection(&all)).unwrap();
    let claude = rels(&inv, &RootKind::ClaudeDir);
    assert!(claude.contains(&"projects/-Users-src-app/memory/MEMORY.md".to_owned()));
    assert!(claude.contains(&"cache/changelog.md".to_owned()));
    assert!(claude.contains(&"mystery.txt".to_owned()));
    assert!(
        inv.notes
            .iter()
            .any(|(r, n)| r == "mystery.txt" && n.contains("sınıflandırılmamış"))
    );
    assert!(!claude.iter().any(|r| r.starts_with("daemon")));

    // Indexes are sequential in (root, rel) order; hashes are filled later.
    for (i, (e, src)) in inv.entries.iter().enumerate() {
        assert_eq!(e.index as usize, i);
        assert_eq!(e.tool, Tool::Claude);
        assert_eq!(e.hash, None);
        assert!(!e.partial_tail);
        assert!(src.is_absolute());
        assert!(fs::symlink_metadata(src).is_ok(), "{}", src.display());
    }
    let keys: Vec<_> = inv
        .entries
        .iter()
        .map(|(e, _)| (e.root.clone(), e.rel.clone()))
        .collect();
    let mut sorted = keys.clone();
    sorted.sort();
    assert_eq!(keys, sorted);

    // Exec bit and the Claude JSON file come through.
    let hook = inv
        .entries
        .iter()
        .find(|(e, _)| e.rel == "hooks/fmt.sh")
        .unwrap();
    assert_eq!(hook.0.mode & 0o111, 0o111);
    let json = inv
        .entries
        .iter()
        .find(|(e, _)| e.root == RootKind::ClaudeJson)
        .unwrap();
    assert_eq!(json.0.category, Category::Settings);
    assert_eq!(json.1, env.claude_json);

    // Nothing selected for Claude: no entries, everything reported.
    let none = Selection::default();
    let inv = adapter.inventory(&none).unwrap();
    assert!(inv.entries.is_empty());
    assert_eq!(skip_reason(&inv, "CLAUDE.md"), Some(UNSELECTED));
}

#[test]
fn project_refs_from_cwd_and_claude_json() {
    let (_tmp, env) = setup();
    let adapter = ClaudeAdapter { env: env.clone() };
    let refs = adapter.project_refs().unwrap();
    assert_eq!(refs.len(), 2, "{refs:?}");

    let app = refs
        .iter()
        .find(|r| r.id == ProjectId("github.com/acme/app".into()))
        .expect("app project");
    assert_eq!(app.old_path, env.home.join("code/app").to_str().unwrap());
    assert_eq!(app.remote.as_deref(), Some("github.com/acme/app"));
    assert!(app.is_git);

    let plain = refs
        .iter()
        .find(|r| r.id == ProjectId("path:/Users/src/plain".into()))
        .expect("plain project");
    assert_eq!(plain.old_path, "/Users/src/plain");
    assert_eq!(plain.remote, None);
    assert!(!plain.is_git);
}

#[test]
fn inventory_includes_agents_skills_symlink_targets() {
    let (_tmp, env) = setup();
    let adapter = ClaudeAdapter { env: env.clone() };
    let inv = adapter.inventory(&selection(DEFAULT)).unwrap();

    let link = inv
        .entries
        .iter()
        .find(|(e, _)| e.root == RootKind::ClaudeDir && e.rel == "skills/shared")
        .expect("symlink entry");
    assert_eq!(
        link.0.kind,
        EntryKind::Symlink {
            target: "../../.agents/skills/shared".into()
        }
    );
    assert!(
        !rels(&inv, &RootKind::ClaudeDir).contains(&"skills/shared/SKILL.md".to_owned()),
        "symlink was followed"
    );

    let agents = rels(&inv, &RootKind::AgentsDir);
    assert!(
        agents.contains(&"skills/shared/SKILL.md".to_owned()),
        "{agents:?}"
    );
    let skill = inv
        .entries
        .iter()
        .find(|(e, _)| e.root == RootKind::AgentsDir && e.rel == "skills/shared/SKILL.md")
        .unwrap();
    assert_eq!(skill.0.category, Category::Settings);
    assert_eq!(skill.1, env.agents_dir.join("skills/shared/SKILL.md"));

    // Settings off: neither root is inventoried.
    let inv = adapter
        .inventory(&selection(&[Category::Sessions]))
        .unwrap();
    assert!(rels(&inv, &RootKind::AgentsDir).is_empty());
    assert_eq!(skip_reason(&inv, "agents_dir:skills"), Some(UNSELECTED));
}

#[test]
fn inventory_project_inner_files() {
    let (_tmp, env) = setup();
    let adapter = ClaudeAdapter { env: env.clone() };
    let inv = adapter.inventory(&selection(DEFAULT)).unwrap();

    let app = RootKind::Project(ProjectId("github.com/acme/app".into()));
    let files = rels(&inv, &app);
    for want in [
        ".claude",
        ".claude/settings.json",
        ".mcp.json",
        "CLAUDE.md",
        "docs",
        "docs/CLAUDE.md",
    ] {
        assert!(
            files.contains(&want.to_owned()),
            "{want} missing: {files:?}"
        );
    }
    assert!(!files.iter().any(|r| r.starts_with(".claude/worktrees")));
    assert!(
        !files
            .iter()
            .any(|r| r.starts_with("src") || r.starts_with(".git"))
    );
    assert!(
        inv.entries
            .iter()
            .filter(|(e, _)| e.root == app)
            .all(|(e, _)| e.category == Category::ProjectFiles)
    );
    assert_eq!(inv.projects.len(), 2);

    let missing = skip_reason(&inv, "project:path:/Users/src/plain").unwrap();
    assert!(missing.contains("bu makinede yok"), "{missing}");

    let direct = project_inner_files(&env.home.join("code/app"));
    assert!(direct.contains(&"docs/CLAUDE.md".to_owned()));
    assert!(!direct.iter().any(|r| r.starts_with(".claude/worktrees")));

    // ProjectFiles off: the project is reported, not inventoried.
    let inv = adapter
        .inventory(&selection(&[Category::Settings]))
        .unwrap();
    assert!(rels(&inv, &app).is_empty());
    assert_eq!(
        skip_reason(&inv, "project:github.com/acme/app"),
        Some(UNSELECTED)
    );
}

#[test]
fn read_claude_json_stable_parses_and_reports_bad_json() {
    let tmp = tempfile::tempdir().unwrap();
    let good = tmp.path().join("good.json");
    fs::write(&good, r#"{"projects":{}}"#).unwrap();
    let v = read_claude_json_stable(&good).unwrap();
    assert!(v["projects"].is_object());

    let bad = tmp.path().join("bad.json");
    fs::write(&bad, r#"{"projects":"#).unwrap();
    assert!(read_claude_json_stable(&bad).is_err());
    assert!(read_claude_json_stable(&tmp.path().join("missing.json")).is_err());
}

/// Rewrites `~/.claude.json` through `f`.
fn edit_claude_json(env: &VtEnv, f: impl FnOnce(&mut serde_json::Value)) {
    let mut json: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&env.claude_json).unwrap()).unwrap();
    f(&mut json);
    fs::write(&env.claude_json, json.to_string()).unwrap();
}

#[test]
fn corrupt_claude_json_does_not_abort_inventory() {
    let (_tmp, env) = setup();
    fs::write(&env.claude_json, "{\"projects\":").unwrap();
    let adapter = ClaudeAdapter { env: env.clone() };

    let inv = adapter.inventory(&selection(DEFAULT)).unwrap();
    let reason = skip_reason(&inv, "claude_json").expect("claude_json reported");
    assert!(reason.starts_with("okunamadı"), "{reason}");
    assert!(
        inv.entries
            .iter()
            .all(|(e, _)| e.root != RootKind::ClaudeJson)
    );
    // Projects still come from the session cwd fields.
    let ids: Vec<&str> = inv.projects.iter().map(|p| p.id.0.as_str()).collect();
    assert!(ids.contains(&"github.com/acme/app"), "{ids:?}");
    assert!(ids.contains(&"path:/Users/src/plain"), "{ids:?}");
    assert!(rels(&inv, &RootKind::ClaudeDir).contains(&"CLAUDE.md".to_owned()));
    assert_eq!(adapter.project_refs().unwrap().len(), 2);
}

#[test]
fn non_utf8_name_does_not_lose_other_entries() {
    use std::os::unix::ffi::OsStrExt;
    let (_tmp, env) = setup();
    let bad = env
        .claude_dir
        .join("projects")
        .join(std::ffi::OsStr::from_bytes(b"bad-\xff-name"));
    if let Err(e) = fs::create_dir(&bad) {
        // APFS refuses non-UTF-8 names (EILSEQ); Linux file systems allow them.
        eprintln!("non-UTF-8 names unsupported here ({e}); test skipped");
        return;
    }
    fs::write(bad.join("s.jsonl"), "{}\n").unwrap();
    let adapter = ClaudeAdapter { env };

    let inv = adapter.inventory(&selection(DEFAULT)).unwrap();
    let claude = rels(&inv, &RootKind::ClaudeDir);
    assert!(claude.contains(&"projects/-Users-src-app/s-app.jsonl".to_owned()));
    assert!(claude.contains(&"CLAUDE.md".to_owned()));
    let reason = skip_reason(&inv, "projects/bad-\u{FFFD}-name").expect("bad name reported");
    assert!(reason.contains("UTF-8"), "{reason}");
}

#[test]
fn home_as_project_does_not_duplicate_or_walk() {
    let (_tmp, env) = setup();
    let home = env.home.clone();
    fs::write(home.join("CLAUDE.md"), "# Home\n").unwrap();
    fs::write(home.join(".mcp.json"), "{}\n").unwrap();
    fs::create_dir_all(home.join("Library/notes")).unwrap();
    fs::write(home.join("Library/notes/CLAUDE.md"), "# Lib\n").unwrap();
    fs::create_dir_all(home.join("code/notes")).unwrap();
    fs::write(home.join("code/notes/CLAUDE.md"), "# Notes\n").unwrap();
    edit_claude_json(&env, |json| {
        let projects = json["projects"].as_object_mut().unwrap();
        projects.insert(home.to_str().unwrap().into(), serde_json::json!({}));
        projects.insert(
            home.join("code").to_str().unwrap().into(),
            serde_json::json!({}),
        );
    });
    let adapter = ClaudeAdapter { env: env.clone() };
    let inv = adapter.inventory(&selection(DEFAULT)).unwrap();

    let home_root = RootKind::Project(ProjectId(format!("path:{}", home.display())));
    let home_files = rels(&inv, &home_root);
    assert_eq!(
        home_files,
        vec![".mcp.json", "CLAUDE.md"],
        "home: {home_files:?}"
    );

    // A parent project skips nested project roots but keeps its own files.
    let code_root = RootKind::Project(ProjectId(format!("path:{}", home.join("code").display())));
    let code_files = rels(&inv, &code_root);
    assert_eq!(
        code_files,
        vec!["notes", "notes/CLAUDE.md"],
        "code: {code_files:?}"
    );

    // The app project still has its own files, once.
    let app = RootKind::Project(ProjectId("github.com/acme/app".into()));
    assert!(rels(&inv, &app).contains(&"CLAUDE.md".to_owned()));
}

#[test]
fn container_category_is_fixed_precedence() {
    let (_tmp, env) = setup();
    let mut all = DEFAULT.to_vec();
    all.push(Category::Cache);
    let inv = ClaudeAdapter { env }.inventory(&selection(&all)).unwrap();
    let category = |rel: &str| {
        inv.entries
            .iter()
            .find(|(e, _)| e.root == RootKind::ClaudeDir && e.rel == rel)
            .map(|(e, _)| e.category)
    };
    // `projects` holds Sessions and Memory: Sessions wins (Category order).
    assert_eq!(category("projects"), Some(Category::Sessions));
    assert_eq!(
        category("projects/-Users-src-app"),
        Some(Category::Sessions)
    );
    // `plugins` holds Settings and Cache: Settings wins.
    assert_eq!(category("plugins"), Some(Category::Settings));
}
