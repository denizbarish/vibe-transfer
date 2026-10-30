//! `build_claude_apply_plan` edge cases that need no full export: target
//! confinement of bundle paths, transform skips, and pending templates.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;
use vt_claude::{
    LinkInput, PlanInput, PlanQuestions, build_claude_apply_plan, build_link_plan,
    resolve_dest_template,
};
use vt_core::VtEnv;
use vt_core::apply::{Decision, Resolution};
use vt_core::bundle::ReadBundle;
use vt_core::manifest::{
    Category, Entry, EntryKind, FORMAT_VERSION, JsonConflict, Manifest, ProjectId, ProjectRef,
    RootKind, Selection, SourceInfo, Tool,
};
use vt_core::pathmap::Mapping;
use vt_core::pending::{DestTemplate, PendingItem, PendingProject};

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
        subpath: None,
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

fn lost_project() -> (ProjectId, ProjectRef) {
    let id = ProjectId("github.com/acme/lost".into());
    let project = ProjectRef {
        id: id.clone(),
        old_path: "/Users/alice/lost".into(),
        remote: Some(id.0.clone()),
        is_git: true,
    };
    (id, project)
}

fn session_line(cwd: &str) -> Vec<u8> {
    format!("{}\n", serde_json::json!({"type": "user", "cwd": cwd})).into_bytes()
}

#[test]
fn subfolder_session_of_a_pending_project_keeps_its_subpath() {
    let fx = fx();
    let (id, project) = lost_project();
    let slug = "-Users-alice-lost-pkg-web";
    let rb = bundle(
        &fx,
        vec![
            file(
                &fx,
                0,
                RootKind::ClaudeDir,
                &format!("projects/{slug}/s.jsonl"),
                &session_line("/Users/alice/lost/pkg/web"),
            ),
            file(
                &fx,
                1,
                RootKind::ClaudeDir,
                "projects/-Users-alice-lost/memory/MEMORY.md",
                b"- note\n",
            ),
        ],
        vec![project],
    );
    let mut mappings = BTreeMap::new();
    mappings.insert(id.clone(), Mapping::Pending);
    let p = plan(&fx, &rb, &mappings).unwrap();
    let (_, items) = p.pending.iter().find(|(r, _)| r.id == id).unwrap();
    let template = |rel: &str| {
        items
            .iter()
            .find(|i| i.entry.rel.ends_with(rel))
            .map(|i| i.dest_template.clone())
            .unwrap()
    };
    let session = template("s.jsonl");
    assert_eq!(
        session,
        DestTemplate::ClaudeProjectSlugDir {
            rel_in_slug: "s.jsonl".into(),
            subpath: Some("pkg/web".into()),
        }
    );
    // Memory is keyed by the project root: no subpath.
    assert_eq!(
        template("MEMORY.md"),
        DestTemplate::ClaudeProjectSlugDir {
            rel_in_slug: "memory/MEMORY.md".into(),
            subpath: None,
        }
    );
    let root = fx.env.home.join("code/lost");
    let slug_of = |p: &Path| vt_core::pathcodec::claude_slug(p.to_str().unwrap());
    assert_eq!(
        resolve_dest_template(&session, &root, &fx.env).unwrap(),
        Some(
            fx.env
                .claude_dir
                .join("projects")
                .join(slug_of(&root.join("pkg/web")))
                .join("s.jsonl")
        )
    );
    let escape = DestTemplate::ClaudeProjectSlugDir {
        rel_in_slug: "s.jsonl".into(),
        subpath: Some("../x".into()),
    };
    assert!(resolve_dest_template(&escape, &root, &fx.env).is_err());
}

#[test]
fn repo_paths_of_pending_projects_are_held_back() {
    let fx = fx();
    let (id, project) = lost_project();
    let doc = serde_json::json!({
        "projects": {"/Users/alice/lost": {"allowedTools": []}},
        "githubRepoPaths": {
            "acme/lost": ["/Users/alice/lost", "/Users/alice/lost-copy"],
            "acme/only": ["/Users/alice/lost/sub"],
        },
    });
    let rb = bundle(
        &fx,
        vec![file(
            &fx,
            0,
            RootKind::ClaudeJson,
            "",
            doc.to_string().as_bytes(),
        )],
        vec![project],
    );
    let mut mappings = BTreeMap::new();
    mappings.insert(id.clone(), Mapping::Pending);
    let p = plan(&fx, &rb, &mappings).unwrap();
    let (_, written) = p
        .plan
        .json_writes
        .iter()
        .find(|(d, _)| *d == fx.env.claude_json)
        .unwrap();
    let repos = &written["githubRepoPaths"];
    let copy = format!("{}/lost-copy", fx.env.home.display());
    assert_eq!(repos["acme/lost"], serde_json::json!([copy]));
    assert!(repos.get("acme/only").is_none(), "{repos}");
    let (_, items) = p.pending.iter().find(|(r, _)| r.id == id).unwrap();
    let held = items
        .iter()
        .find(|i| i.dest_template == DestTemplate::ClaudeJsonRepoPaths)
        .unwrap();
    let blob: serde_json::Value = serde_json::from_slice(&fs::read(&held.blob).unwrap()).unwrap();
    assert_eq!(
        blob,
        serde_json::json!({
            "acme/lost": ["/Users/alice/lost"],
            "acme/only": ["/Users/alice/lost/sub"],
        })
    );
    assert_eq!(held.source_home.as_deref(), Some("/Users/alice"));
}

fn pending_item(fx: &Fx, index: u32, entry: Entry, template: DestTemplate) -> PendingItem {
    PendingItem {
        blob: fx.blobs.join(index.to_string()),
        entry,
        dest_template: template,
        source_home: Some("/Users/alice".into()),
    }
}

#[test]
fn link_plan_places_pending_items_below_the_new_root() {
    let fx = fx();
    let (_, project) = lost_project();
    let root = fx.env.home.join("code/lost");
    fs::create_dir_all(&root).unwrap();
    let id = project.id.clone();
    let md = file(
        &fx,
        0,
        RootKind::Project(id.clone()),
        "CLAUDE.md",
        b"# lost\n",
    );
    let session = file(
        &fx,
        1,
        RootKind::ClaudeDir,
        "projects/-Users-alice-lost-sub/s.jsonl",
        &session_line("/Users/alice/lost/sub"),
    );
    let keys = file(
        &fx,
        2,
        RootKind::ClaudeJson,
        "",
        serde_json::json!({"/Users/alice/lost": {"allowedTools": ["Bash"]}})
            .to_string()
            .as_bytes(),
    );
    let repos = file(
        &fx,
        3,
        RootKind::ClaudeJson,
        "",
        serde_json::json!({"acme/lost": ["/Users/alice/lost"]})
            .to_string()
            .as_bytes(),
    );
    let pending = PendingProject {
        project: project.clone(),
        items: vec![
            pending_item(&fx, 0, md, DestTemplate::ProjectRel("CLAUDE.md".into())),
            pending_item(
                &fx,
                1,
                session,
                DestTemplate::ClaudeProjectSlugDir {
                    rel_in_slug: "s.jsonl".into(),
                    subpath: Some("sub".into()),
                },
            ),
            pending_item(&fx, 2, keys, DestTemplate::ClaudeJsonProjectsKey),
            pending_item(&fx, 3, repos, DestTemplate::ClaudeJsonRepoPaths),
        ],
    };
    // The target already knows another repo path.
    fs::write(
        &fx.env.claude_json,
        serde_json::json!({"githubRepoPaths": {"acme/lost": ["/elsewhere"]}}).to_string(),
    )
    .unwrap();
    let input = LinkInput {
        project: &pending,
        root: &root,
        session_id: "l",
        work_dir: &fx.work,
    };
    let p = build_link_plan(&fx.env, &input, &mut NoQuestions).unwrap();
    let dests: Vec<&Path> = p.plan.placements.iter().map(|x| x.dest.as_path()).collect();
    let slug = vt_core::pathcodec::claude_slug(root.join("sub").to_str().unwrap());
    assert!(
        dests.contains(&root.join("CLAUDE.md").as_path()),
        "{dests:?}"
    );
    let session_dest = fx
        .env
        .claude_dir
        .join("projects")
        .join(slug)
        .join("s.jsonl");
    assert!(dests.contains(&session_dest.as_path()), "{dests:?}");
    let (_, doc) = p
        .plan
        .json_writes
        .iter()
        .find(|(d, _)| *d == fx.env.claude_json)
        .unwrap();
    let key = root.to_str().unwrap();
    assert_eq!(
        doc["projects"][key]["allowedTools"],
        serde_json::json!(["Bash"])
    );
    assert_eq!(
        doc["githubRepoPaths"]["acme/lost"],
        serde_json::json!(["/elsewhere", key])
    );
    assert!(p.pending.is_empty());
}

#[test]
fn link_plan_rejects_items_outside_claude_shapes() {
    let fx = fx();
    let (id, project) = lost_project();
    let root = fx.env.home.join("code/lost");
    fs::create_dir_all(&root).unwrap();
    let bad = [
        (
            file(&fx, 0, RootKind::Project(id.clone()), ".bashrc", b"x"),
            DestTemplate::ProjectRel(".bashrc".into()),
        ),
        (
            file(&fx, 0, RootKind::Project(id.clone()), "CLAUDE.md", b"x"),
            DestTemplate::ProjectRel(".git/hooks/pre-commit".into()),
        ),
        (
            file(&fx, 0, RootKind::ClaudeDir, "projects/x/s.jsonl", b"x"),
            DestTemplate::ClaudeProjectSlugDir {
                rel_in_slug: "../../settings.json".into(),
                subpath: None,
            },
        ),
    ];
    for (entry, template) in bad {
        let pending = PendingProject {
            project: project.clone(),
            items: vec![pending_item(&fx, 0, entry, template.clone())],
        };
        let input = LinkInput {
            project: &pending,
            root: &root,
            session_id: "l",
            work_dir: &fx.work,
        };
        assert!(
            build_link_plan(&fx.env, &input, &mut NoQuestions).is_err(),
            "{template:?}"
        );
    }
}

#[test]
fn claude_dir_entry_naming_the_claude_json_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("dst");
    let config = home.join("cfg");
    fs::create_dir_all(&config).unwrap();
    let mut fx = fx();
    fx.env = VtEnv::new(home, "bob".into(), Some(config.clone()));
    assert!(fx.env.claude_json.starts_with(&fx.env.claude_dir));
    let rel = fx
        .env
        .claude_json
        .strip_prefix(&fx.env.claude_dir)
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    for variant in [rel.clone(), rel.to_uppercase()] {
        let rb = bundle(
            &fx,
            vec![file(&fx, 0, RootKind::ClaudeDir, &variant, b"{}")],
            Vec::new(),
        );
        let err = plan(&fx, &rb, &BTreeMap::new()).unwrap_err();
        assert!(
            format!("{err:#}").contains("yalnız birleştirilerek yazılır"),
            "{variant}: {err:#}"
        );
    }
}
