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
