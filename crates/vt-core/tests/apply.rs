#![cfg(unix)]

use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};

use serde_json::json;
use vt_core::apply::{
    ApplyPlan, ConflictResolver, Placement, ReplaceScope, Resolution, apply, resolve_json, rollback,
};
use vt_core::manifest::{EntryKind, JsonConflict};
use vt_core::scan::hash_file;

/// Answers every file conflict with the same resolution and counts calls.
struct Fixed {
    res: Resolution,
    calls: u32,
    panic_on: Option<u32>,
}

impl Fixed {
    fn new(res: Resolution) -> Self {
        Self {
            res,
            calls: 0,
            panic_on: None,
        }
    }
}

impl ConflictResolver for Fixed {
    fn file(&mut self, _dest: &Path) -> Resolution {
        self.calls += 1;
        if self.panic_on == Some(self.calls) {
            panic!("test: resolver panic");
        }
        self.res
    }
    fn json(&mut self, _c: &JsonConflict) -> Resolution {
        self.res
    }
}

struct Env {
    _tmp: tempfile::TempDir,
    vt_home: PathBuf,
    dst: PathBuf,
    blobs: PathBuf,
}

fn env() -> Env {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let env = Env {
        vt_home: root.join("vt"),
        dst: root.join("dst"),
        blobs: root.join("blobs"),
        _tmp: tmp,
    };
    fs::create_dir_all(&env.dst).unwrap();
    fs::create_dir_all(&env.blobs).unwrap();
    env
}

const MTIME: i64 = 1_600_000_000_123_456_789;

fn write_with(path: &Path, body: &[u8], mode: u32, mtime_ns: i64) {
    if let Some(p) = path.parent() {
        fs::create_dir_all(p).unwrap();
    }
    fs::write(path, body).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    let t = filetime::FileTime::from_unix_time(
        mtime_ns.div_euclid(1_000_000_000),
        mtime_ns.rem_euclid(1_000_000_000) as u32,
    );
    filetime::set_file_mtime(path, t).unwrap();
}

fn file_placement(e: &Env, index: u32, dest: &Path, body: &[u8], mode: u32) -> Placement {
    let blob = e.blobs.join(format!("blob-{index}"));
    write_with(&blob, body, 0o600, 1);
    Placement {
        entry_index: index,
        expected_hash: Some(*blake3::hash(body).as_bytes()),
        blob,
        dest: dest.to_path_buf(),
        kind: EntryKind::File,
        mtime_ns: MTIME + i64::from(index),
        mode,
    }
}

fn plan(session: &str, placements: Vec<Placement>) -> ApplyPlan {
    ApplyPlan {
        session_id: session.into(),
        source_host: "src".into(),
        placements,
        replace_scopes: Vec::new(),
        json_writes: Vec::new(),
    }
}

fn mtime_ns(md: &fs::Metadata) -> i64 {
    md.mtime() * 1_000_000_000 + md.mtime_nsec()
}

/// (rel, kind/content, mtime, mode) of every object under `root`; dir
/// mtimes are left out because adding and removing children changes them.
fn tree(root: &Path) -> Vec<(String, String, i64, u32)> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, String, i64, u32)>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let rel = path.strip_prefix(root).unwrap().display().to_string();
            let md = fs::symlink_metadata(&path).unwrap();
            if md.file_type().is_symlink() {
                let target = fs::read_link(&path).unwrap().display().to_string();
                out.push((rel, format!("link:{target}"), mtime_ns(&md), 0));
            } else if md.is_dir() {
                out.push((rel, "dir".into(), 0, md.mode() & 0o7777));
                walk(root, &path, out);
            } else {
                let hash = blake3::Hash::from_bytes(hash_file(&path).unwrap()).to_hex();
                out.push((
                    rel,
                    format!("file:{hash}"),
                    mtime_ns(&md),
                    md.mode() & 0o7777,
                ));
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.sort();
    out
}

fn backup_of(e: &Env, session: &str, dest: &Path) -> PathBuf {
    e.vt_home
        .join("backups")
        .join(session)
        .join("abs")
        .join(dest.strip_prefix("/").unwrap())
}

#[test]
fn apply_places_and_preserves_mtime_mode() {
    let e = env();
    let file = e.dst.join(".claude/a/b.txt");
    let mut p = file_placement(&e, 1, &file, b"hello", 0o640);
    p.mtime_ns = MTIME;
    let dir = e.dst.join(".claude/skills");
    let link = e.dst.join(".claude/link");
    let placements = vec![
        Placement {
            entry_index: 0,
            blob: PathBuf::new(),
            dest: dir.clone(),
            kind: EntryKind::Dir,
            mtime_ns: MTIME,
            mode: 0o750,
            expected_hash: None,
        },
        p,
        Placement {
            entry_index: 2,
            blob: PathBuf::new(),
            dest: link.clone(),
            kind: EntryKind::Symlink {
                target: "a/b.txt".into(),
            },
            mtime_ns: MTIME,
            mode: 0o777,
            expected_hash: None,
        },
    ];
    let mut r = Fixed::new(Resolution::KeepTarget);
    let report = apply(&plan("s1", placements), &e.vt_home, &mut r).unwrap();

    assert_eq!(report.placed, 2);
    assert_eq!(fs::read(&file).unwrap(), b"hello");
    let md = fs::metadata(&file).unwrap();
    assert_eq!(md.mode() & 0o7777, 0o640);
    assert_eq!(mtime_ns(&md), MTIME);

    let dmd = fs::metadata(&dir).unwrap();
    assert!(dmd.is_dir());
    assert_eq!(dmd.mode() & 0o7777, 0o750);

    assert_eq!(fs::read_link(&link).unwrap(), Path::new("a/b.txt"));
    assert_eq!(mtime_ns(&fs::symlink_metadata(&link).unwrap()), MTIME);
    assert_eq!(r.calls, 0);

    // No temp files are left beside the targets.
    let names: Vec<String> = tree(&e.dst).into_iter().map(|t| t.0).collect();
    assert!(names.iter().all(|n| !n.contains(".vt-tmp-")), "{names:?}");

    let journal = e.vt_home.join("journal/s1.jsonl");
    let jmd = fs::metadata(&journal).unwrap();
    assert_eq!(jmd.mode() & 0o777, 0o600);
    let text = fs::read_to_string(&journal).unwrap();
    assert!(text.contains(r#""op":"place""#), "{text}");
    assert!(text.contains(r#""op":"mkdir""#), "{text}");
}

#[test]
fn identical_file_skipped() {
    let e = env();
    let dest = e.dst.join("same.txt");
    write_with(&dest, b"same", 0o644, 42_000_000_000);
    let before = tree(&e.dst);
    let p = file_placement(&e, 1, &dest, b"same", 0o600);
    let mut r = Fixed::new(Resolution::KeepSource);
    let report = apply(&plan("s1", vec![p]), &e.vt_home, &mut r).unwrap();
    assert_eq!(report.skipped_identical, 1);
    assert_eq!(report.placed, 0);
    assert!(report.backed_up.is_empty());
    assert_eq!(r.calls, 0);
    assert_eq!(tree(&e.dst), before);
}

#[test]
fn keep_both_writes_suffix() {
    let e = env();
    let dest = e.dst.join("settings.json");
    let bare = e.dst.join("CLAUDE");
    write_with(&dest, b"target", 0o644, MTIME);
    write_with(&bare, b"target", 0o644, MTIME);
    let mut r = Fixed::new(Resolution::KeepBoth);
    let placements = vec![
        file_placement(&e, 1, &dest, b"source", 0o600),
        file_placement(&e, 2, &bare, b"source", 0o600),
    ];
    let report = apply(&plan("s1", placements), &e.vt_home, &mut r).unwrap();

    let both = e.dst.join("settings.from-src.json");
    let bare_both = e.dst.join("CLAUDE.from-src");
    assert_eq!(fs::read(&dest).unwrap(), b"target");
    assert_eq!(fs::read(&both).unwrap(), b"source");
    assert_eq!(fs::read(&bare_both).unwrap(), b"source");
    assert_eq!(report.kept_both, vec![both.clone(), bare_both]);
    assert!(report.backed_up.is_empty());

    // A second import with the suffixed name taken counts upward.
    let p = file_placement(&e, 3, &dest, b"source two", 0o600);
    let report = apply(&plan("s2", vec![p]), &e.vt_home, &mut r).unwrap();
    let second = e.dst.join("settings.from-src-2.json");
    assert_eq!(report.kept_both, vec![second.clone()]);
    assert_eq!(fs::read(&second).unwrap(), b"source two");
    assert_eq!(fs::read(&both).unwrap(), b"source");
}

#[test]
fn keep_target_leaves_dest_and_reports() {
    let e = env();
    let dest = e.dst.join("x.txt");
    write_with(&dest, b"target", 0o644, MTIME);
    let before = tree(&e.dst);
    let mut r = Fixed::new(Resolution::KeepTarget);
    let p = file_placement(&e, 1, &dest, b"source", 0o600);
    let report = apply(&plan("s1", vec![p]), &e.vt_home, &mut r).unwrap();
    assert_eq!(r.calls, 1);
    assert_eq!(report.placed, 0);
    assert_eq!(report.conflicts.len(), 1);
    assert_eq!(tree(&e.dst), before);
}

#[test]
fn replace_scope_moves_extra_target_files_to_backup() {
    let e = env();
    let scope = e.dst.join(".claude/skills");
    let keep = scope.join("keep.md");
    let extra = scope.join("extra.md");
    let nested = scope.join("sub/extra2.md");
    let outside_dir = e.dst.join("outside");
    let outside_file = outside_dir.join("precious.txt");
    let linked_dir = scope.join("linked");
    let sibling = e.dst.join(".claude/settings.json");
    write_with(&keep, b"keep", 0o644, MTIME);
    write_with(&extra, b"extra", 0o640, MTIME);
    write_with(&nested, b"extra2", 0o644, MTIME);
    write_with(&outside_file, b"precious", 0o644, MTIME);
    write_with(&sibling, b"{}", 0o600, MTIME);
    symlink(&outside_dir, &linked_dir).unwrap();

    let mut p = plan("s1", Vec::new());
    p.replace_scopes.push(ReplaceScope {
        dir: scope.clone(),
        keep: BTreeSet::from([keep.clone()]),
    });
    let mut r = Fixed::new(Resolution::KeepSource);
    let report = apply(&p, &e.vt_home, &mut r).unwrap();

    assert_eq!(fs::read(&keep).unwrap(), b"keep");
    assert!(fs::symlink_metadata(&extra).is_err());
    assert!(fs::symlink_metadata(&nested).is_err());
    assert!(fs::symlink_metadata(&linked_dir).is_err());
    // Nothing outside the scope is touched, symlinked dirs are not followed.
    assert_eq!(fs::read(&outside_file).unwrap(), b"precious");
    assert_eq!(fs::read(&sibling).unwrap(), b"{}");

    // Not deleted: the backup holds them with mode and mtime.
    let b = backup_of(&e, "s1", &extra);
    assert_eq!(fs::read(&b).unwrap(), b"extra");
    let md = fs::metadata(&b).unwrap();
    assert_eq!(md.mode() & 0o7777, 0o640);
    assert_eq!(mtime_ns(&md), MTIME);
    assert_eq!(fs::read(backup_of(&e, "s1", &nested)).unwrap(), b"extra2");
    let bl = backup_of(&e, "s1", &linked_dir);
    assert_eq!(fs::read_link(&bl).unwrap(), outside_dir);
    let mut backed = report.backed_up.clone();
    backed.sort();
    let mut want = vec![extra, nested, linked_dir];
    want.sort();
    assert_eq!(backed, want);

    let bdir = e.vt_home.join("backups/s1");
    assert_eq!(fs::metadata(&bdir).unwrap().mode() & 0o777, 0o700);
}

/// A target with existing files that the import replaces, new files and
/// new directories.
fn mixed_plan(e: &Env, session: &str) -> ApplyPlan {
    let mut placements = Vec::new();
    for i in 1..=4u32 {
        let dest = e.dst.join(format!(".claude/agents/a{i}.md"));
        write_with(&dest, format!("old {i}").as_bytes(), 0o644, MTIME - 7);
        placements.push(file_placement(
            e,
            i,
            &dest,
            format!("new {i}").as_bytes(),
            0o600,
        ));
    }
    for i in 5..=7u32 {
        let dest = e.dst.join(format!(".claude/projects/p{i}/s.jsonl"));
        placements.push(file_placement(e, i, &dest, b"session", 0o600));
    }
    let mut p = plan(session, placements);
    let cj = e.dst.join(".claude.json");
    write_with(&cj, br#"{"old":true}"#, 0o644, MTIME - 3);
    p.json_writes.push((cj, json!({"new": true})));
    p
}

#[test]
fn rollback_after_partial_apply_restores_exact_tree() {
    let e = env();
    let p = mixed_plan(&e, "s1");
    let before = tree(&e.dst);
    let mut r = Fixed::new(Resolution::KeepSource);
    r.panic_on = Some(3);
    let res = catch_unwind(AssertUnwindSafe(|| apply(&p, &e.vt_home, &mut r)));
    assert!(res.is_err(), "resolver panic must propagate");
    // Two replacements happened before the panic.
    assert_eq!(
        fs::read(e.dst.join(".claude/agents/a1.md")).unwrap(),
        b"new 1"
    );
    assert_ne!(tree(&e.dst), before);

    let report = rollback(&e.vt_home, "s1").unwrap();
    assert!(report.restored >= 2, "{report:?}");
    assert_eq!(tree(&e.dst), before);
}

#[test]
fn rollback_is_idempotent() {
    let e = env();
    let p = mixed_plan(&e, "s1");
    let before = tree(&e.dst);
    let mut r = Fixed::new(Resolution::KeepSource);
    let report = apply(&p, &e.vt_home, &mut r).unwrap();
    assert_eq!(report.placed, 8);
    assert_eq!(report.backed_up.len(), 5);
    let cj = e.dst.join(".claude.json");
    assert_eq!(fs::read_to_string(&cj).unwrap(), "{\n  \"new\": true\n}\n");
    assert_eq!(fs::metadata(&cj).unwrap().mode() & 0o7777, 0o644);
    assert_ne!(tree(&e.dst), before);

    let first = rollback(&e.vt_home, "s1").unwrap();
    assert_eq!(tree(&e.dst), before);
    assert_eq!(first.restored, 5);
    let second = rollback(&e.vt_home, "s1").unwrap();
    assert_eq!(tree(&e.dst), before);
    assert_eq!(second.restored, 0);
    assert_eq!(second.removed, 0);
}

#[test]
fn expected_hash_mismatch_rolls_back_automatically() {
    let e = env();
    let mut p = mixed_plan(&e, "s1");
    let before = tree(&e.dst);
    p.placements[5].expected_hash = Some([7u8; 32]);
    let mut r = Fixed::new(Resolution::KeepSource);
    let err = apply(&p, &e.vt_home, &mut r).unwrap_err();
    assert!(format!("{err:#}").contains("hash"), "{err:#}");
    assert_eq!(tree(&e.dst), before);
    // Rolling back again later changes nothing.
    rollback(&e.vt_home, "s1").unwrap();
    assert_eq!(tree(&e.dst), before);
}

#[test]
fn symlink_at_dest_is_backed_up_not_followed() {
    let e = env();
    let outside = e.dst.join("dotfiles/settings.json");
    write_with(&outside, b"linked target", 0o644, MTIME);
    let dest = e.dst.join(".claude/settings.json");
    fs::create_dir_all(dest.parent().unwrap()).unwrap();
    symlink(&outside, &dest).unwrap();
    let before = tree(&e.dst);

    let p = file_placement(&e, 1, &dest, b"source", 0o600);
    let mut r = Fixed::new(Resolution::KeepSource);
    apply(&plan("s1", vec![p]), &e.vt_home, &mut r).unwrap();
    assert_eq!(r.calls, 1);
    assert_eq!(fs::read(&outside).unwrap(), b"linked target");
    assert!(fs::symlink_metadata(&dest).unwrap().is_file());
    assert_eq!(fs::read_link(backup_of(&e, "s1", &dest)).unwrap(), outside);

    rollback(&e.vt_home, "s1").unwrap();
    assert_eq!(tree(&e.dst), before);
}

#[test]
fn json_keep_both_acts_as_keep_target_and_is_reported() {
    let c = JsonConflict {
        path: "theme".into(),
        source: json!("dark"),
        target: json!("light"),
    };
    let mut r = Fixed::new(Resolution::KeepBoth);
    let mut notes = Vec::new();
    assert_eq!(resolve_json(&mut r, &c, &mut notes), Resolution::KeepTarget);
    assert_eq!(notes.len(), 1);
    assert!(notes[0].contains("theme"), "{notes:?}");
    let mut r = Fixed::new(Resolution::KeepSource);
    assert_eq!(resolve_json(&mut r, &c, &mut notes), Resolution::KeepSource);
    assert_eq!(notes.len(), 1);
}

#[test]
fn rejects_unsafe_session_and_relative_dest() {
    let e = env();
    let mut r = Fixed::new(Resolution::KeepSource);
    assert!(apply(&plan("../x", Vec::new()), &e.vt_home, &mut r).is_err());
    assert!(rollback(&e.vt_home, "a/b").is_err());
    let p = file_placement(&e, 1, Path::new("rel/x.txt"), b"x", 0o600);
    assert!(apply(&plan("s1", vec![p]), &e.vt_home, &mut r).is_err());
    let p = file_placement(&e, 1, &e.dst.join("a/../x.txt"), b"x", 0o600);
    assert!(apply(&plan("s2", vec![p]), &e.vt_home, &mut r).is_err());
    assert!(fs::read_dir(&e.dst).unwrap().next().is_none());
}

#[test]
fn reused_session_id_is_refused() {
    let e = env();
    let mut r = Fixed::new(Resolution::KeepSource);
    apply(&plan("s1", Vec::new()), &e.vt_home, &mut r).unwrap();
    assert!(apply(&plan("s1", Vec::new()), &e.vt_home, &mut r).is_err());
}

fn link_placement(index: u32, dest: &Path, target: &Path) -> Placement {
    Placement {
        entry_index: index,
        blob: PathBuf::new(),
        dest: dest.to_path_buf(),
        kind: EntryKind::Symlink {
            target: target.display().to_string(),
        },
        mtime_ns: MTIME,
        mode: 0o777,
        expected_hash: None,
    }
}

fn dir_placement(index: u32, dest: &Path) -> Placement {
    Placement {
        entry_index: index,
        blob: PathBuf::new(),
        dest: dest.to_path_buf(),
        kind: EntryKind::Dir,
        mtime_ns: MTIME,
        mode: 0o755,
        expected_hash: None,
    }
}

#[test]
fn plan_rejects_dest_below_planned_symlink() {
    let e = env();
    let outside = e.blobs.join("outside");
    fs::create_dir_all(&outside).unwrap();
    let link = e.dst.join(".claude/escape");
    let mut r = Fixed::new(Resolution::KeepSource);

    let p = plan(
        "s1",
        vec![
            link_placement(0, &link, &outside),
            file_placement(&e, 1, &link.join("f.txt"), b"x", 0o600),
        ],
    );
    let err = apply(&p, &e.vt_home, &mut r).unwrap_err();
    assert!(format!("{err:#}").contains("symlink"), "{err:#}");

    let mut p = plan("s2", vec![link_placement(0, &link, &outside)]);
    p.json_writes.push((link.join("x.json"), json!({})));
    assert!(apply(&p, &e.vt_home, &mut r).is_err());

    let mut p = plan("s3", vec![link_placement(0, &link, &outside)]);
    p.replace_scopes.push(ReplaceScope {
        dir: link.join("sub"),
        keep: BTreeSet::new(),
    });
    assert!(apply(&p, &e.vt_home, &mut r).is_err());

    assert!(fs::read_dir(&e.dst).unwrap().next().is_none());
    assert!(fs::read_dir(&outside).unwrap().next().is_none());
    assert!(!e.vt_home.join("journal/s1.jsonl").exists());
}

#[test]
fn plan_rejects_paths_overlapping_vt_home() {
    let e = env();
    let mut r = Fixed::new(Resolution::KeepSource);
    let inside = file_placement(&e, 1, &e.vt_home.join("backups/x"), b"x", 0o600);
    assert!(apply(&plan("s1", vec![inside]), &e.vt_home, &mut r).is_err());
    let mut p = plan("s2", Vec::new());
    p.replace_scopes.push(ReplaceScope {
        dir: e.vt_home.parent().unwrap().to_path_buf(),
        keep: BTreeSet::new(),
    });
    assert!(apply(&p, &e.vt_home, &mut r).is_err());
    let mut p = plan("s3", Vec::new());
    p.json_writes.push((e.vt_home.clone(), json!({})));
    assert!(apply(&p, &e.vt_home, &mut r).is_err());
    // Through a symlinked alias of vt_home as well.
    let alias = e.dst.join("alias");
    fs::create_dir_all(&e.vt_home).unwrap();
    symlink(&e.vt_home, &alias).unwrap();
    let p = file_placement(&e, 2, &alias.join("x"), b"x", 0o600);
    assert!(apply(&plan("s4", vec![p]), &e.vt_home, &mut r).is_err());
    assert!(!e.vt_home.join("x").exists());
}

#[test]
fn dir_placement_on_symlink_to_dir_is_used_as_is() {
    let e = env();
    let stow = e.dst.join("stow/claude");
    fs::create_dir_all(&stow).unwrap();
    let claude = e.dst.join(".claude");
    symlink(&stow, &claude).unwrap();
    let before = tree(&e.dst);

    let p = plan(
        "s1",
        vec![
            dir_placement(0, &claude),
            file_placement(&e, 1, &claude.join("a.txt"), b"a", 0o600),
        ],
    );
    let mut r = Fixed::new(Resolution::KeepSource);
    let report = apply(&p, &e.vt_home, &mut r).unwrap();
    assert!(report.conflicts.is_empty(), "{:?}", report.conflicts);
    assert!(
        fs::symlink_metadata(&claude)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(fs::read(stow.join("a.txt")).unwrap(), b"a");
    let journal = fs::read_to_string(e.vt_home.join("journal/s1.jsonl")).unwrap();
    assert!(!journal.contains(r#""op":"mkdir""#), "{journal}");

    rollback(&e.vt_home, "s1").unwrap();
    assert_eq!(tree(&e.dst), before);
}

#[test]
fn dir_placement_on_symlink_to_file_backs_it_up() {
    let e = env();
    let file = e.dst.join("real.txt");
    write_with(&file, b"real", 0o644, MTIME);
    let dest = e.dst.join("d");
    symlink(&file, &dest).unwrap();
    let before = tree(&e.dst);

    let mut r = Fixed::new(Resolution::KeepSource);
    let report = apply(
        &plan("s1", vec![dir_placement(0, &dest)]),
        &e.vt_home,
        &mut r,
    )
    .unwrap();
    assert!(fs::symlink_metadata(&dest).unwrap().is_dir());
    assert_eq!(report.backed_up, vec![dest.clone()]);
    assert_eq!(report.conflicts.len(), 1);
    assert_eq!(fs::read_link(backup_of(&e, "s1", &dest)).unwrap(), file);

    rollback(&e.vt_home, "s1").unwrap();
    assert_eq!(tree(&e.dst), before);
}

#[test]
fn symlinked_replace_scope_is_not_cleared() {
    let e = env();
    let real = e.dst.join("stow/skills");
    write_with(&real.join("mine.md"), b"mine", 0o644, MTIME);
    let scope = e.dst.join(".claude/skills");
    fs::create_dir_all(scope.parent().unwrap()).unwrap();
    symlink(&real, &scope).unwrap();
    let before = tree(&e.dst);

    let mut p = plan("s1", Vec::new());
    p.replace_scopes.push(ReplaceScope {
        dir: scope.clone(),
        keep: BTreeSet::new(),
    });
    let mut r = Fixed::new(Resolution::KeepSource);
    let report = apply(&p, &e.vt_home, &mut r).unwrap();
    assert_eq!(tree(&e.dst), before);
    assert!(report.backed_up.is_empty());
    assert_eq!(report.conflicts.len(), 1);
    assert!(
        report.conflicts[0].contains("replace kapsamı symlink, temizlenmedi"),
        "{:?}",
        report.conflicts
    );
}

#[test]
fn keep_both_identical_variant_is_skipped() {
    let e = env();
    let dest = e.dst.join("settings.json");
    write_with(&dest, b"target", 0o644, MTIME);
    write_with(
        &e.dst.join("settings.from-src.json"),
        b"source",
        0o644,
        MTIME,
    );
    let before = tree(&e.dst);
    let mut r = Fixed::new(Resolution::KeepBoth);
    let p = file_placement(&e, 1, &dest, b"source", 0o600);
    let report = apply(&plan("s1", vec![p]), &e.vt_home, &mut r).unwrap();
    assert_eq!(report.skipped_identical, 1);
    assert!(report.conflicts.is_empty(), "{:?}", report.conflicts);
    assert!(report.kept_both.is_empty());
    assert_eq!(report.placed, 0);
    assert_eq!(tree(&e.dst), before);
}

/// Whether `dir`'s file system treats `a` and `b` as the same name.
fn same_name(dir: &Path, a: &str, b: &str) -> bool {
    let probe = dir.join("probe");
    fs::create_dir_all(&probe).unwrap();
    fs::write(probe.join(a), b"").unwrap();
    let same = probe.join(b).exists();
    fs::remove_dir_all(&probe).unwrap();
    same
}

/// Places a symlink named `link_name` and then a file below `file_dir`, a
/// different spelling of the same name. Wherever the file system folds the
/// two, the write must fail and nothing may land outside.
fn variant_escape(link_name: &str, file_dir: &str) {
    let e = env();
    let outside = e.blobs.join("outside");
    fs::create_dir_all(&outside).unwrap();
    let folds = same_name(&e.blobs, link_name, file_dir);
    let claude = e.dst.join(".claude");
    fs::create_dir_all(&claude).unwrap();
    let before = tree(&e.dst);
    let p = plan(
        "s1",
        vec![
            link_placement(0, &claude.join(link_name), &outside),
            file_placement(&e, 1, &claude.join(file_dir).join("f.txt"), b"x", 0o600),
        ],
    );
    let mut r = Fixed::new(Resolution::KeepSource);
    let res = apply(&p, &e.vt_home, &mut r);
    assert!(fs::read_dir(&outside).unwrap().next().is_none());
    if !folds {
        eprintln!("SKIPPED: file system keeps {link_name:?} and {file_dir:?} apart");
        return;
    }
    let err = res.unwrap_err();
    assert!(format!("{err:#}").contains("symlink"), "{err:#}");
    assert_eq!(tree(&e.dst), before);
}

#[test]
fn case_variant_of_placed_symlink_is_not_written_through() {
    variant_escape("Escape", "escape");
}

#[test]
fn normalization_variant_of_placed_symlink_is_not_written_through() {
    // NFC link name, NFD directory name.
    variant_escape("caf\u{e9}", "cafe\u{301}");
}

#[test]
fn case_variant_of_vt_home_is_rejected() {
    let e = env();
    let root = e.vt_home.parent().unwrap().to_path_buf();
    let folds = same_name(&e.blobs, "vt", "VT");
    let p = file_placement(&e, 1, &root.join("VT/x.txt"), b"x", 0o600);
    let mut r = Fixed::new(Resolution::KeepSource);
    let res = apply(&plan("s1", vec![p]), &e.vt_home, &mut r);
    if !folds {
        eprintln!("SKIPPED: file system is case-sensitive");
        return;
    }
    assert!(res.is_err());
    assert!(!e.vt_home.join("x.txt").exists());
}
