//! `build_claude_apply_plan` edge cases that need no full export: target
//! confinement of bundle paths, transform skips, and pending templates.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;
use vt_claude::{PlanInput, PlanQuestions, build_claude_apply_plan, resolve_dest_template};
use vt_core::VtEnv;
use vt_core::apply::{Decision, Resolution};
use vt_core::bundle::ReadBundle;
use vt_core::manifest::{
    Category, Entry, EntryKind, FORMAT_VERSION, JsonConflict, Manifest, ProjectId, ProjectRef,
    RootKind, Selection, SourceInfo, Tool,
};
use vt_core::pathmap::Mapping;
use vt_core::pending::DestTemplate;

/// Fails on any question: these plans must not need the user.
struct NoQuestions;

impl PlanQuestions for NoQuestions {
    fn project_decision(&mut self, _: &ProjectRef, _: &Path) -> Result<Decision> {
        anyhow::bail!("unexpected question")
    }
    fn file(&mut self, _: &Path) -> Result<Resolution> {
        anyhow::bail!("unexpected question")
    }
    fn json(&mut self, _: &JsonConflict) -> Result<Resolution> {
        anyhow::bail!("unexpected question")
    }
}

struct Fx {
    _tmp: tempfile::TempDir,
    env: VtEnv,
    blobs: PathBuf,
    work: PathBuf,
}

fn fx() -> Fx {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("dst");
    fs::create_dir_all(&home).unwrap();
    let blobs = tmp.path().join("blobs");
    let work = tmp.path().join("work");
    fs::create_dir_all(&blobs).unwrap();
    fs::create_dir_all(&work).unwrap();
    Fx {
        env: VtEnv::new(home, "bob".into(), None),
        blobs,
        work,
        _tmp: tmp,
    }
}

fn file(fx: &Fx, index: u32, root: RootKind, rel: &str, content: &[u8]) -> Entry {
    fs::write(fx.blobs.join(index.to_string()), content).unwrap();
    Entry {
        index,
        tool: Tool::Claude,
        category: Category::Settings,
        root,
        rel: rel.into(),
        kind: EntryKind::File,
        size: content.len() as u64,
        mtime_ns: 0,
        mode: 0o644,
        hash: Some(*blake3::hash(content).as_bytes()),
        partial_tail: false,
    }
}

fn bundle(fx: &Fx, entries: Vec<Entry>, projects: Vec<ProjectRef>) -> ReadBundle {
    ReadBundle {
        manifest: Manifest {
            format_version: FORMAT_VERSION,
            session_id: "s".into(),
            created_at: "2026-10-30T00:00:00Z".into(),
            source: SourceInfo {
                os: "macos".into(),
                user: "alice".into(),
                home: "/Users/alice".into(),
                hostname: "src".into(),
                tool_versions: BTreeMap::new(),
            },
            selection: Selection::default_for(&[Tool::Claude]),
            entries,
            projects,
            secrets: Vec::new(),
            notes: Vec::new(),
        },
        blobs: fx.blobs.clone(),
        secrets: Vec::new(),
    }
}

fn plan(
    fx: &Fx,
    rb: &ReadBundle,
    mappings: &BTreeMap<ProjectId, Mapping>,
) -> Result<vt_claude::ClaudePlan> {
    let input = PlanInput {
        mappings,
        decision: Decision::Merge,
        take_oauth: false,
        session_id: "t",
        work_dir: &fx.work,
    };
    build_claude_apply_plan(rb, &fx.env, &input, &mut NoQuestions)
}

#[test]
fn rejects_paths_that_leave_their_root() {
    let fx = fx();
    let bad = [
        (RootKind::ClaudeDir, "../escape.md"),
        (RootKind::ClaudeDir, "agents/../../escape.md"),
        (RootKind::ClaudeDir, "/etc/escape"),
        (RootKind::ClaudeDir, ""),
        (RootKind::ClaudeDir, "agents//x.md"),
        (RootKind::AgentsDir, "skills/./x"),
        (RootKind::ClaudeJson, "x"),
    ];
    for (root, rel) in bad {
        let rb = bundle(&fx, vec![file(&fx, 0, root.clone(), rel, b"x")], Vec::new());
        let err = plan(&fx, &rb, &BTreeMap::new()).unwrap_err();
        assert!(format!("{err:#}").contains("geçersiz"), "{rel}: {err:#}");
    }
    // A project rel escaping its (mapped) root is rejected too.
    let id = ProjectId("github.com/acme/app".into());
    let project = ProjectRef {
        id: id.clone(),
        old_path: "/Users/alice/app".into(),
        remote: Some(id.0.clone()),
        is_git: true,
    };
    let rb = bundle(
        &fx,
        vec![file(
            &fx,
            0,
            RootKind::Project(id.clone()),
            "../../.zshrc",
            b"x",
        )],
        vec![project],
    );
    let mut mappings = BTreeMap::new();
    mappings.insert(id, Mapping::Auto(fx.env.home.join("app")));
    assert!(plan(&fx, &rb, &mappings).is_err());
    assert!(fs::read_dir(&fx.env.home).unwrap().next().is_none());
}

#[test]
fn rejects_relative_or_unchosen_project_roots() {
    let fx = fx();
    let id = ProjectId("github.com/acme/app".into());
    let rb = bundle(&fx, Vec::new(), Vec::new());
    for m in [
        Mapping::Auto(PathBuf::from("rel/app")),
        Mapping::Auto(PathBuf::from("/x/../app")),
        Mapping::Choose(vec![PathBuf::from("/a"), PathBuf::from("/b")]),
    ] {
        let mut mappings = BTreeMap::new();
        mappings.insert(id.clone(), m);
        assert!(plan(&fx, &rb, &mappings).is_err());
    }
}

#[test]
fn binary_and_large_hook_files_are_placed_unchanged_and_reported() {
    let fx = fx();
    let binary = b"\x00\x01/Users/alice/x".to_vec();
    let text = b"#!/bin/sh\ncat /Users/alice/notes\n".to_vec();
    let rb = bundle(
        &fx,
        vec![
            file(&fx, 0, RootKind::ClaudeDir, "hooks/tool.bin", &binary),
            file(&fx, 1, RootKind::ClaudeDir, "hooks/run.sh", &text),
        ],
        Vec::new(),
    );
    let p = plan(&fx, &rb, &BTreeMap::new()).unwrap();
    let skipped: Vec<&str> = p.skipped.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(skipped, vec!["hooks/tool.bin"], "{:?}", p.skipped);
    let bin = p
        .plan
        .placements
        .iter()
        .find(|x| x.entry_index == 0)
        .unwrap();
    assert_eq!(bin.expected_hash, Some(*blake3::hash(&binary).as_bytes()));
    let sh = p
        .plan
        .placements
        .iter()
        .find(|x| x.entry_index == 1)
        .unwrap();
    let rewritten = fs::read_to_string(&sh.blob).unwrap();
    let dst = fx.env.home.to_str().unwrap();
    assert_eq!(rewritten, format!("#!/bin/sh\ncat {dst}/notes\n"));
    assert_eq!(p.transformed.len(), 1);
    assert_eq!(p.transformed[0].kind, "home_paths");
    assert_eq!(
        p.transformed[0].after,
        *blake3::hash(rewritten.as_bytes()).as_bytes()
    );
}

#[test]
fn dest_templates_resolve_below_the_project() {
    let fx = fx();
    let root = fx.env.home.join("code/app");
    let rel = DestTemplate::ProjectRel(".claude/settings.json".into());
    assert_eq!(
        resolve_dest_template(&rel, &root, &fx.env).unwrap(),
        Some(root.join(".claude/settings.json"))
    );
    let slug = DestTemplate::ClaudeProjectSlugDir {
        rel_in_slug: "memory/MEMORY.md".into(),
    };
    let expected = fx
        .env
        .claude_dir
        .join("projects")
        .join(vt_core::pathcodec::claude_slug(root.to_str().unwrap()))
        .join("memory/MEMORY.md");
    assert_eq!(
        resolve_dest_template(&slug, &root, &fx.env).unwrap(),
        Some(expected)
    );
    assert_eq!(
        resolve_dest_template(&DestTemplate::ClaudeJsonProjectsKey, &root, &fx.env).unwrap(),
        None
    );
    let escape = DestTemplate::ProjectRel("../x".into());
    assert!(resolve_dest_template(&escape, &root, &fx.env).is_err());
    assert!(resolve_dest_template(&rel, Path::new("relative"), &fx.env).is_err());
}

fn app_project(fx: &Fx) -> (ProjectId, ProjectRef, BTreeMap<ProjectId, Mapping>) {
    let id = ProjectId("github.com/acme/app".into());
    let project = ProjectRef {
        id: id.clone(),
        old_path: "/Users/alice/app".into(),
        remote: Some(id.0.clone()),
        is_git: true,
    };
    let mut mappings = BTreeMap::new();
    mappings.insert(id.clone(), Mapping::Auto(fx.env.home.join("code/app")));
    (id, project, mappings)
}

fn dir(index: u32, root: RootKind, rel: &str) -> Entry {
    Entry {
        index,
        tool: Tool::Claude,
        category: Category::ProjectFiles,
        root,
        rel: rel.into(),
        kind: EntryKind::Dir,
        size: 0,
        mtime_ns: 0,
        mode: 0o755,
        hash: None,
        partial_tail: false,
    }
}

#[test]
fn project_entries_outside_claude_shapes_reject_the_plan() {
    let fx = fx();
    let (id, project, mappings) = app_project(&fx);
    let root = RootKind::Project(id.clone());
    for rel in [
        ".bashrc",
        ".git/hooks/pre-commit",
        ".ssh/authorized_keys",
        "src/main.rs",
        ".claude/worktrees/wt/x.md",
        "a/b/c/d/CLAUDE.md",
        "node_modules/x/CLAUDE.md",
    ] {
        let rb = bundle(
            &fx,
            vec![file(&fx, 0, root.clone(), rel, b"x")],
            vec![project.clone()],
        );
        let err = plan(&fx, &rb, &mappings).unwrap_err();
        assert!(format!("{err:#}").contains("reddedildi"), "{rel}: {err:#}");
        // Also when the project would only go to the pending store.
        let mut pending = mappings.clone();
        pending.insert(id.clone(), Mapping::Pending);
        assert!(plan(&fx, &rb, &pending).is_err(), "{rel}");
    }
    let ok = vec![
        file(&fx, 0, root.clone(), "CLAUDE.md", b"a"),
        file(&fx, 1, root.clone(), "CLAUDE.local.md", b"b"),
        file(&fx, 2, root.clone(), ".mcp.json", b"{}"),
        file(&fx, 3, root.clone(), ".claude/settings.json", b"{}"),
        file(&fx, 4, root.clone(), ".claude/agents/r.md", b"c"),
        dir(5, root.clone(), "docs"),
        file(&fx, 6, root.clone(), "docs/CLAUDE.md", b"d"),
        file(&fx, 7, root.clone(), "a/b/c/CLAUDE.md", b"e"),
    ];
    let rb = bundle(&fx, ok, vec![project]);
    plan(&fx, &rb, &mappings).unwrap();
    assert!(fs::read_dir(&fx.env.home).unwrap().next().is_none());
}

#[test]
fn control_characters_in_paths_reject_the_plan() {
    let fx = fx();
    for rel in [
        "agents/\u{1b}]0;x\u{7}.md",
        "agents/a\nb.md",
        "agents/\u{9b}31m.md",
    ] {
        let rb = bundle(
            &fx,
            vec![file(&fx, 0, RootKind::ClaudeDir, rel, b"x")],
            Vec::new(),
        );
        let err = plan(&fx, &rb, &BTreeMap::new()).unwrap_err();
        assert!(format!("{err:#}").contains("geçersiz"), "{err:#}");
    }
}

#[test]
fn symlink_into_a_project_follows_the_project_mapping() {
    let fx = fx();
    let (_, project, mappings) = app_project(&fx);
    let link = |index: u32, rel: &str, target: &str| Entry {
        index,
        tool: Tool::Claude,
        category: Category::Settings,
        root: RootKind::ClaudeDir,
        rel: rel.into(),
        kind: EntryKind::Symlink {
            target: target.into(),
        },
        size: 0,
        mtime_ns: 0,
        mode: 0o755,
        hash: None,
        partial_tail: false,
    };
    let rb = bundle(
        &fx,
        vec![
            link(0, "skills/app-skill", "/Users/alice/app/.claude/skills/x"),
            link(1, "skills/other", "/Users/alice/tools/y"),
            link(2, "skills/rel", "../shared/z"),
        ],
        vec![project],
    );
    let p = plan(&fx, &rb, &mappings).unwrap();
    let target = |i: u32| match &p
        .plan
        .placements
        .iter()
        .find(|x| x.entry_index == i)
        .unwrap()
        .kind
    {
        EntryKind::Symlink { target } => target.clone(),
        other => panic!("{other:?}"),
    };
    let home = fx.env.home.to_str().unwrap();
    assert_eq!(target(0), format!("{home}/code/app/.claude/skills/x"));
    assert_eq!(target(1), format!("{home}/tools/y"));
    assert_eq!(target(2), "../shared/z");
}

#[test]
fn case_and_normalization_variants_of_denied_names_are_rejected() {
    let fx = fx();
    let (id, project, mappings) = app_project(&fx);
    let root = RootKind::Project(id);
    for rel in [
        ".claude/WORKTREES/x.md",
        ".claude/Worktrees/x.md",
        ".Claude/settings.json",
        ".CLAUDE/agents/r.md",
        ".GIT/CLAUDE.md",
        "Node_Modules/x/CLAUDE.md",
    ] {
        let rb = bundle(
            &fx,
            vec![file(&fx, 0, root.clone(), rel, b"x")],
            vec![project.clone()],
        );
        let err = plan(&fx, &rb, &mappings).unwrap_err();
        assert!(format!("{err:#}").contains("reddedildi"), "{rel}: {err:#}");
    }
}

#[test]
fn home_mapped_project_never_writes_into_the_live_claude_dir() {
    let fx = fx();
    let live = fx.env.claude_dir.join("settings.json");
    fs::create_dir_all(&fx.env.claude_dir).unwrap();
    fs::write(&live, b"{\"live\":true}").unwrap();
    let id = ProjectId(format!("path:{}", "/Users/alice"));
    let project = ProjectRef {
        id: id.clone(),
        old_path: "/Users/alice".into(),
        remote: None,
        is_git: false,
    };
    let root = RootKind::Project(id.clone());
    let mut mappings = BTreeMap::new();
    // The user approved the target home as this project's root.
    mappings.insert(id, Mapping::Auto(fx.env.home.clone()));

    for rel in [".claude/settings.json", ".claude"] {
        let entry = if rel == ".claude" {
            dir(0, root.clone(), rel)
        } else {
            file(&fx, 0, root.clone(), rel, b"{\"evil\":true}")
        };
        let rb = bundle(&fx, vec![entry], vec![project.clone()]);
        let err = plan(&fx, &rb, &mappings).unwrap_err();
        assert!(format!("{err:#}").contains("Claude klasörü"), "{err:#}");
    }
    assert_eq!(fs::read(&live).unwrap(), b"{\"live\":true}");

    // Root-level files of the approved home project are planned normally.
    let rb = bundle(
        &fx,
        vec![file(&fx, 0, root, "CLAUDE.md", b"# home\n")],
        vec![project],
    );
    let p = plan(&fx, &rb, &mappings).unwrap();
    assert_eq!(p.plan.placements[0].dest, fx.env.home.join("CLAUDE.md"));
}

#[test]
fn never_class_names_in_project_claude_dir_are_rejected() {
    let fx = fx();
    let (id, project, mappings) = app_project(&fx);
    let root = RootKind::Project(id);
    for rel in [
        ".claude/.credentials.json",
        ".claude/.Credentials.json",
        ".claude/sessions/1.json",
        ".claude/.device-keys.json",
        ".claude/scheduled_tasks.lock",
        ".claude/.claude.json.tmp.1",
        &format!(".claude/.credential{}.json", '\u{17F}'),
        &format!(".claude/se{}ions/x", '\u{DF}'),
        &format!(".claude/se{}ion-env", '\u{1E9E}'),
        &format!(".claude/.la{}-cleanup", '\u{FB05}'),
    ] {
        let rb = bundle(
            &fx,
            vec![file(&fx, 0, root.clone(), rel, b"x")],
            vec![project.clone()],
        );
        let err = plan(&fx, &rb, &mappings).unwrap_err();
        assert!(format!("{err:#}").contains("reddedildi"), "{rel}: {err:#}");
    }
}

#[test]
fn project_claude_user_content_is_accepted() {
    let fx = fx();
    let (id, project, mappings) = app_project(&fx);
    let root = RootKind::Project(id);
    for rel in [
        ".claude/daemon-runbook.md",
        ".claude/Daemon/x",
        ".claude/bun.lock",
        ".claude/debug/notes.md",
        ".claude/downloads/a.md",
        ".claude/chrome/a.md",
        ".claude/tasks/t/.lock",
        ".claude/agents/ağ-uzmanı.md",
    ] {
        let rb = bundle(
            &fx,
            vec![file(&fx, 0, root.clone(), rel, b"x")],
            vec![project.clone()],
        );
        let p = plan(&fx, &rb, &mappings).unwrap_or_else(|e| panic!("{rel}: {e:#}"));
        assert_eq!(p.plan.placements.len(), 1, "{rel}");
    }
}

#[test]
fn never_class_claude_dir_entries_reject_the_plan() {
    let fx = fx();
    for rel in [
        ".credentials.json".to_owned(),
        ".Credentials.json".to_owned(),
        ".device-keys.json".to_owned(),
        "sessions/1.json".to_owned(),
        "Sessions/1.json".to_owned(),
        format!("se{}ions/x", '\u{DF}'),
        format!(".la{}-cleanup", '\u{FB06}'),
        "tasks/t/.LOCK".to_owned(),
    ] {
        let rb = bundle(
            &fx,
            vec![
                file(&fx, 0, RootKind::ClaudeDir, "agents/a.md", b"a"),
                file(&fx, 1, RootKind::ClaudeDir, &rel, b"x"),
            ],
            Vec::new(),
        );
        let err = plan(&fx, &rb, &BTreeMap::new()).unwrap_err();
        assert!(format!("{err:#}").contains("reddedildi"), "{rel}: {err:#}");
        assert!(
            fs::read_dir(&fx.env.home).unwrap().next().is_none(),
            "{rel}"
        );
    }
    // Ordinary Claude dir content still plans.
    let rb = bundle(
        &fx,
        vec![
            file(&fx, 0, RootKind::ClaudeDir, "agents/ağ-uzmanı.md", b"a"),
            file(&fx, 1, RootKind::ClaudeDir, "tasks/t/plan.md", b"p"),
        ],
        Vec::new(),
    );
    assert_eq!(
        plan(&fx, &rb, &BTreeMap::new())
            .unwrap()
            .plan
            .placements
            .len(),
        2
    );
}

#[test]
fn replace_keeps_case_variants_of_never_files_in_the_target() {
    let fx = fx();
    let lock = fx.env.claude_dir.join("tasks/t/.LOCK");
    fs::create_dir_all(lock.parent().unwrap()).unwrap();
    fs::write(&lock, "1").unwrap();
    fs::write(fx.env.claude_dir.join("tasks/t/old.md"), "old").unwrap();
    let rb = bundle(
        &fx,
        vec![
            dir(1, RootKind::ClaudeDir, "tasks"),
            file(&fx, 0, RootKind::ClaudeDir, "tasks/u/plan.md", b"p"),
        ],
        Vec::new(),
    );
    let input = PlanInput {
        mappings: &BTreeMap::new(),
        decision: Decision::Replace,
        take_oauth: false,
        session_id: "t",
        work_dir: &fx.work,
    };
    let p = build_claude_apply_plan(&rb, &fx.env, &input, &mut NoQuestions).unwrap();
    let scope = p
        .plan
        .replace_scopes
        .iter()
        .find(|s| s.dir == fx.env.claude_dir.join("tasks"))
        .expect("tasks scope");
    assert!(scope.keep.contains(&lock), "{:?}", scope.keep);
    assert!(
        !scope
            .keep
            .contains(&fx.env.claude_dir.join("tasks/t/old.md"))
    );
}
