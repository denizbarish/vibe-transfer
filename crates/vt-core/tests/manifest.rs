use std::collections::{BTreeMap, BTreeSet};
use std::fs;

use vt_core::manifest::{
    Category, Entry, EntryKind, Manifest, ProjectId, ProjectRef, RootKind, Selection, SourceInfo,
    Tool,
};
use vt_core::merkle::merkle_root;
use vt_core::scan::{hash_file, scan_tree};
use vt_core::secret_types::{SecretBytes, SecretItem, SecretKind, SecretMeta};

fn sample_meta() -> SecretMeta {
    SecretMeta {
        tool: Tool::Claude,
        kind: SecretKind::ClaudeKeychain,
        account_uuid: Some("00000000-0000-0000-0000-000000000001".into()),
        email: Some("alice@example.com".into()),
    }
}

#[test]
fn secret_item_debug_redacts_value() {
    let item = SecretItem {
        meta: sample_meta(),
        service: "Claude Code-credentials".into(),
        account_hint: "alice".into(),
        value: SecretBytes::new(b"super-secret-token-value".to_vec()),
    };
    let dbg = format!("{item:?}");
    assert!(!dbg.contains("super-secret-token-value"), "{dbg}");
    assert!(dbg.contains("Claude Code-credentials"));
    assert_eq!(item.value.expose(), b"super-secret-token-value");

    // The value survives the JSON frame used inside encrypted bundles.
    let json = serde_json::to_string(&item).unwrap();
    let back: SecretItem = serde_json::from_str(&json).unwrap();
    assert_eq!(back.value.expose(), b"super-secret-token-value");
}

#[cfg(unix)]
#[test]
fn scan_preserves_symlink_and_mtime() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(root.join("x"), b"hello").unwrap();
    fs::create_dir(root.join("sub")).unwrap();
    fs::write(root.join("sub/a.txt"), b"abc").unwrap();
    std::os::unix::fs::symlink("../x", root.join("sub/link")).unwrap();
    fs::write(root.join("skip.tmp"), b"no").unwrap();

    let entries = scan_tree(root, &|rel: &str| rel.ends_with(".tmp")).unwrap();
    let rels: Vec<&str> = entries.iter().map(|e| e.rel.as_str()).collect();
    assert_eq!(rels, vec!["sub", "sub/a.txt", "sub/link", "x"]);

    let link = entries.iter().find(|e| e.rel == "sub/link").unwrap();
    assert_eq!(
        link.kind,
        EntryKind::Symlink {
            target: "../x".into()
        }
    );

    let file = entries.iter().find(|e| e.rel == "sub/a.txt").unwrap();
    assert_eq!(file.kind, EntryKind::File);
    assert_eq!(file.size, 3);
    let md = fs::symlink_metadata(root.join("sub/a.txt")).unwrap();
    let expected_ns = md
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos() as i64;
    assert_eq!(file.mtime_ns, expected_ns);
    use std::os::unix::fs::MetadataExt;
    assert_eq!(file.mode, md.mode());

    let sub = entries.iter().find(|e| e.rel == "sub").unwrap();
    assert_eq!(sub.kind, EntryKind::Dir);

    let h = hash_file(&root.join("x")).unwrap();
    assert_eq!(h, *blake3::hash(b"hello").as_bytes());
}

fn entry(index: u32, root: RootKind, rel: &str, hash: [u8; 32]) -> Entry {
    Entry {
        index,
        tool: Tool::Claude,
        category: Category::Sessions,
        root,
        rel: rel.into(),
        kind: EntryKind::File,
        size: 10,
        mtime_ns: 1_700_000_000_000_000_000,
        mode: 0o100644,
        hash: Some(hash),
        partial_tail: false,
    }
}

#[test]
fn merkle_root_is_order_independent_and_content_sensitive() {
    let a = vec![
        entry(0, RootKind::ClaudeDir, "projects/a.jsonl", [1; 32]),
        entry(1, RootKind::ClaudeDir, "settings.json", [2; 32]),
        entry(2, RootKind::ClaudeJson, "", [3; 32]),
        entry(
            3,
            RootKind::Project(ProjectId("github.com/acme/app".into())),
            "CLAUDE.md",
            [4; 32],
        ),
        entry(4, RootKind::AgentsDir, "skills/x.md", [5; 32]),
    ];
    let mut shuffled = a.clone();
    shuffled.reverse();
    shuffled.swap(0, 2);
    let r1 = merkle_root(&a);
    assert_eq!(r1, merkle_root(&shuffled));

    let mut changed = a.clone();
    changed[3].hash.as_mut().unwrap()[31] ^= 1;
    assert_ne!(r1, merkle_root(&changed));

    assert_eq!(merkle_root(&[]), *blake3::hash(b"vt-empty").as_bytes());
    assert_ne!(merkle_root(&a[..1]), merkle_root(&a[..2]));
}

#[test]
fn manifest_json_roundtrip() {
    let mut cats = BTreeMap::new();
    cats.insert(
        Tool::Claude,
        BTreeSet::from([Category::Sessions, Category::Memory, Category::Secrets]),
    );
    let m = Manifest {
        format_version: 1,
        session_id: "s-0001".into(),
        created_at: "2026-10-29T10:00:00Z".into(),
        source: SourceInfo {
            os: "macos".into(),
            user: "alice".into(),
            home: "/Users/alice".into(),
            hostname: "src".into(),
            tool_versions: BTreeMap::from([("claude".into(), "2.0.0".into())]),
        },
        selection: Selection {
            tools: BTreeSet::from([Tool::Claude]),
            categories: cats,
        },
        entries: vec![
            entry(0, RootKind::ClaudeDir, "settings.json", [7; 32]),
            Entry {
                kind: EntryKind::Symlink {
                    target: "../x".into(),
                },
                hash: None,
                partial_tail: true,
                root: RootKind::Project(ProjectId("path:/Users/alice/app".into())),
                ..entry(1, RootKind::ClaudeDir, "link", [0; 32])
            },
        ],
        projects: vec![ProjectRef {
            id: ProjectId("github.com/acme/app".into()),
            old_path: "/Users/alice/app".into(),
            remote: Some("git@github.com:acme/app.git".into()),
            is_git: true,
        }],
        secrets: vec![sample_meta()],
        notes: vec!["not".into()],
    };
    let json = serde_json::to_string(&m).unwrap();
    let back: Manifest = serde_json::from_str(&json).unwrap();
    assert_eq!(back, m);
    assert_eq!(serde_json::to_string(&back).unwrap(), json);
}
