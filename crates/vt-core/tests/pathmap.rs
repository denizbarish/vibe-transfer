use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use vt_core::manifest::{Category, Entry, EntryKind, ProjectId, ProjectRef, RootKind, Tool};
use vt_core::pathmap::{Mapping, TargetIndex, build_target_index, map_project, rewrite_home};
use vt_core::pending::{DestTemplate, PendingItem, PendingStore};

const REMOTE: &str = "github.com/acme/app";

fn git_ref(old_path: &str, remote: Option<&str>) -> ProjectRef {
    ProjectRef {
        id: ProjectId(
            remote
                .map(str::to_owned)
                .unwrap_or_else(|| format!("path:{old_path}")),
        ),
        old_path: old_path.into(),
        remote: remote.map(str::to_owned),
        is_git: true,
    }
}

fn plain_ref(old_path: &str) -> ProjectRef {
    ProjectRef {
        id: ProjectId(format!("path:{old_path}")),
        old_path: old_path.into(),
        remote: None,
        is_git: false,
    }
}

fn index_with(paths: &[&str]) -> TargetIndex {
    let mut by_remote = BTreeMap::new();
    by_remote.insert(
        REMOTE.to_owned(),
        paths.iter().map(PathBuf::from).collect::<Vec<_>>(),
    );
    TargetIndex {
        by_remote,
        skipped_dirs: 0,
    }
}

#[test]
fn map_remote_single_auto() {
    let idx = index_with(&["/Users/dst/code/app"]);
    let r = git_ref("/Users/src/work/app", Some(REMOTE));
    assert_eq!(
        map_project(&r, &idx, "/Users/src", Path::new("/Users/dst")),
        Mapping::Auto(PathBuf::from("/Users/dst/code/app"))
    );
}

#[test]
fn map_remote_multiple_choose() {
    let idx = index_with(&["/Users/dst/a/app", "/Users/dst/b/app"]);
    let r = git_ref("/Users/src/work/app", Some(REMOTE));
    assert_eq!(
        map_project(&r, &idx, "/Users/src", Path::new("/Users/dst")),
        Mapping::Choose(vec![
            PathBuf::from("/Users/dst/a/app"),
            PathBuf::from("/Users/dst/b/app")
        ])
    );
}

#[test]
fn map_remote_missing_pending() {
    let idx = TargetIndex::default();
    let r = git_ref("/Users/src/work/app", Some(REMOTE));
    assert_eq!(
        map_project(&r, &idx, "/Users/src", Path::new("/Users/dst")),
        Mapping::Pending
    );
}

#[test]
fn map_git_without_remote_pending() {
    let idx = TargetIndex::default();
    // Under src_home, but a git repo without a remote never maps by path.
    let r = git_ref("/Users/src/work/app", None);
    assert_eq!(
        map_project(&r, &idx, "/Users/src", Path::new("/Users/dst")),
        Mapping::Pending
    );
}

#[test]
fn map_home_non_git_auto_rewrite() {
    let idx = TargetIndex::default();
    let r = plain_ref("/Users/src/Desktop/notes");
    assert_eq!(
        map_project(&r, &idx, "/Users/src", Path::new("/Users/dst")),
        Mapping::Auto(PathBuf::from("/Users/dst/Desktop/notes"))
    );
    // A trailing slash on src_home makes no difference; home itself maps too.
    assert_eq!(
        map_project(&r, &idx, "/Users/src/", Path::new("/Users/dst")),
        Mapping::Auto(PathBuf::from("/Users/dst/Desktop/notes"))
    );
    assert_eq!(
        map_project(
            &plain_ref("/Users/src"),
            &idx,
            "/Users/src",
            Path::new("/Users/dst")
        ),
        Mapping::Auto(PathBuf::from("/Users/dst"))
    );
    // A sibling sharing the prefix is not under home.
    assert_eq!(
        map_project(
            &plain_ref("/Users/src2/notes"),
            &idx,
            "/Users/src",
            Path::new("/Users/dst")
        ),
        Mapping::Pending
    );
}

#[test]
fn map_identical_abs_path_auto() {
    let tmp = tempfile::tempdir().unwrap();
    let existing = tmp.path().join("shared").join("proj");
    fs::create_dir_all(&existing).unwrap();
    let old = existing.to_str().unwrap();
    let r = plain_ref(old);
    assert_eq!(
        map_project(
            &r,
            &TargetIndex::default(),
            "/Users/src",
            Path::new("/Users/dst")
        ),
        Mapping::Auto(existing.clone())
    );
}

#[test]
fn map_outside_home_pending() {
    let r = plain_ref("/private/tmp/x/vt-nonexistent-acme");
    assert_eq!(
        map_project(
            &r,
            &TargetIndex::default(),
            "/Users/src",
            Path::new("/Users/dst")
        ),
        Mapping::Pending
    );
}

#[test]
fn rewrite_home_boundary() {
    let src = "/Users/alice";
    let dst = "/Users/bob";
    assert_eq!(rewrite_home("/Users/alice2/x", src, dst), "/Users/alice2/x");
    assert_eq!(
        rewrite_home("bash /Users/alice/.claude/h.sh", src, dst),
        "bash /Users/bob/.claude/h.sh"
    );
    assert_eq!(rewrite_home("/Users/alice", src, dst), "/Users/bob");
    assert_eq!(rewrite_home("\"/Users/alice\"", src, dst), "\"/Users/bob\"");
    assert_eq!(rewrite_home("'/Users/alice' x", src, dst), "'/Users/bob' x");
    assert_eq!(
        rewrite_home("/Users/alice /Users/alicex", src, dst),
        "/Users/bob /Users/alicex"
    );
    // Trailing slash on src_home is normalized.
    assert_eq!(
        rewrite_home("/Users/alice/x", "/Users/alice/", dst),
        "/Users/bob/x"
    );
    assert_eq!(
        rewrite_home("/Users/alice2/x", "/Users/alice/", dst),
        "/Users/alice2/x"
    );
}

fn make_repo(dir: &Path, url: &str) {
    fs::create_dir_all(dir.join(".git")).unwrap();
    fs::write(
        dir.join(".git/config"),
        format!("[remote \"origin\"]\n\turl = {url}\n"),
    )
    .unwrap();
}

#[test]
fn target_index_prunes_and_stops_at_repo() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    make_repo(&home.join("code/app"), "git@github.com:acme/app.git");
    make_repo(&home.join("Desktop/app2"), "https://github.com/acme/app");
    // Nested repo inside an indexed repo: not indexed.
    make_repo(
        &home.join("code/app/node_modules/dep"),
        "https://github.com/acme/dep",
    );
    make_repo(&home.join("code/app/sub"), "https://github.com/acme/sub");
    // Pruned names.
    make_repo(&home.join("Library/x"), "https://github.com/acme/lib");
    make_repo(&home.join(".hidden/x"), "https://github.com/acme/hidden");
    make_repo(
        &home.join("other/node_modules/x"),
        "https://github.com/acme/nm",
    );
    // Too deep for max_depth 2.
    make_repo(&home.join("a/b/c"), "https://github.com/acme/deep");
    #[cfg(unix)]
    std::os::unix::fs::symlink(home.join("code"), home.join("link")).unwrap();

    let idx = build_target_index(&[home.to_path_buf()], 2);
    let keys: Vec<&str> = idx.by_remote.keys().map(String::as_str).collect();
    assert_eq!(keys, vec!["github.com/acme/app"]);
    assert_eq!(
        idx.by_remote["github.com/acme/app"],
        vec![home.join("Desktop/app2"), home.join("code/app")]
    );
    assert_eq!(idx.skipped_dirs, 0);

    let deep = build_target_index(&[home.to_path_buf()], 3);
    assert!(deep.by_remote.contains_key("github.com/acme/deep"));
}

#[cfg(unix)]
#[test]
fn target_index_skips_unreadable_dir() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let locked = tmp.path().join("locked");
    fs::create_dir(&locked).unwrap();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    let idx = build_target_index(&[tmp.path().to_path_buf()], 5);
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(idx.by_remote.is_empty());
    assert_eq!(idx.skipped_dirs, 1);
}

fn entry(index: u32, rel: &str) -> Entry {
    Entry {
        index,
        tool: Tool::Claude,
        category: Category::Sessions,
        root: RootKind::ClaudeDir,
        rel: rel.into(),
        kind: EntryKind::File,
        size: 5,
        mtime_ns: 0,
        mode: 0o644,
        hash: None,
        partial_tail: false,
    }
}

fn staged_blob(dir: &Path, name: &str, body: &[u8]) -> PathBuf {
    let p = dir.join(name);
    fs::write(&p, body).unwrap();
    let mtime = filetime::FileTime::from_unix_time(1_700_000_000, 0);
    filetime::set_file_mtime(&p, mtime).unwrap();
    p
}

#[test]
fn pending_put_list_take_roundtrip() {
    let tmp = tempfile::tempdir().unwrap();
    let staging = tmp.path().join("staging");
    fs::create_dir(&staging).unwrap();
    let store = PendingStore {
        dir: tmp.path().join("vt/pending"),
    };
    let project = git_ref("/Users/src/work/app", Some(REMOTE));
    let blob = staged_blob(&staging, "0", b"hello");
    let items = vec![PendingItem {
        entry: entry(0, "projects/-Users-src-work-app/s.jsonl"),
        blob,
        dest_template: DestTemplate::ClaudeProjectSlugDir {
            rel_in_slug: "s.jsonl".into(),
        },
    }];
    store.put(&project, &items).unwrap();

    let listed = store.list().unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].project, project);
    assert_eq!(listed[0].items.len(), 1);

    let taken = store.take(&project.id).unwrap();
    assert_eq!(taken.project, project);
    let item = &taken.items[0];
    assert_eq!(item.entry, items[0].entry);
    assert_eq!(item.dest_template, items[0].dest_template);
    assert_eq!(fs::read(&item.blob).unwrap(), b"hello");
    let md = fs::metadata(&item.blob).unwrap();
    assert_eq!(
        filetime::FileTime::from_last_modification_time(&md).unix_seconds(),
        1_700_000_000
    );

    // take does not delete; remove does.
    assert_eq!(store.list().unwrap().len(), 1);
    store.remove(&project.id).unwrap();
    assert!(store.list().unwrap().is_empty());
    assert!(store.take(&project.id).is_err());
    // remove is idempotent.
    store.remove(&project.id).unwrap();
}

#[test]
fn pending_put_twice_merges() {
    let tmp = tempfile::tempdir().unwrap();
    let staging = tmp.path().join("staging");
    fs::create_dir(&staging).unwrap();
    let store = PendingStore {
        dir: tmp.path().join("pending"),
    };
    let project = plain_ref("/private/tmp/acme");
    // Same entry index in both sessions: blob names must not collide.
    let first = vec![PendingItem {
        entry: entry(3, "a.jsonl"),
        blob: staged_blob(&staging, "first", b"one"),
        dest_template: DestTemplate::ProjectRel("a.jsonl".into()),
    }];
    let second = vec![PendingItem {
        entry: entry(3, "b.jsonl"),
        blob: staged_blob(&staging, "second", b"two"),
        dest_template: DestTemplate::ClaudeJsonProjectsKey,
    }];
    store.put(&project, &first).unwrap();
    store.put(&project, &second).unwrap();

    let taken = store.take(&project.id).unwrap();
    assert_eq!(taken.items.len(), 2);
    assert_eq!(fs::read(&taken.items[0].blob).unwrap(), b"one");
    assert_eq!(fs::read(&taken.items[1].blob).unwrap(), b"two");
    assert_ne!(taken.items[0].blob, taken.items[1].blob);
    assert_eq!(
        taken.items[1].dest_template,
        DestTemplate::ClaudeJsonProjectsKey
    );
}

#[cfg(unix)]
#[test]
fn pending_store_is_private() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let staging = tmp.path().join("staging");
    fs::create_dir(&staging).unwrap();
    let store = PendingStore {
        dir: tmp.path().join("vt/pending"),
    };
    let project = plain_ref("/private/tmp/acme");
    let items = vec![PendingItem {
        entry: entry(0, "a"),
        blob: staged_blob(&staging, "a", b"x"),
        dest_template: DestTemplate::ProjectRel("a".into()),
    }];
    store.put(&project, &items).unwrap();
    let project_dir = fs::read_dir(&store.dir)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(project_dir.file_name().unwrap().len(), 16);
    assert_eq!(mode(&store.dir), 0o700);
    assert_eq!(mode(&project_dir), 0o700);
    assert_eq!(mode(&project_dir.join("blobs")), 0o700);
    assert_eq!(mode(&project_dir.join("pending.json")), 0o600);
}
