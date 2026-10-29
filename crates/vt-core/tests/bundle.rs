use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Cursor;
use std::path::Path;

use age::secrecy::{ExposeSecret, SecretString};
use vt_core::bundle::{BundleError, BundleWriter, generate_passphrase, read_bundle};
use vt_core::manifest::{
    Category, Entry, EntryKind, FORMAT_VERSION, Manifest, RootKind, Selection, SourceInfo, Tool,
};
use vt_core::merkle::merkle_root;
use vt_core::secret_types::{SecretBytes, SecretItem, SecretKind, SecretMeta};

const MIB: usize = 1024 * 1024;

fn pass() -> SecretString {
    SecretString::from("test-pass-alice")
}

/// Deterministic incompressible bytes.
fn noise(len: usize, seed: &str) -> Vec<u8> {
    let mut out = vec![0u8; len];
    blake3::Hasher::new()
        .update(seed.as_bytes())
        .finalize_xof()
        .fill(&mut out);
    out
}

fn file_entry(index: u32, rel: &str, size: u64, hash: [u8; 32]) -> Entry {
    Entry {
        index,
        tool: Tool::Claude,
        category: Category::Sessions,
        root: RootKind::ClaudeDir,
        rel: rel.into(),
        kind: EntryKind::File,
        size,
        mtime_ns: 1_700_000_000_000_000_000,
        mode: 0o644,
        hash: Some(hash),
        partial_tail: false,
    }
}

fn manifest(entries: Vec<Entry>) -> Manifest {
    Manifest {
        format_version: FORMAT_VERSION,
        session_id: "session-1".into(),
        created_at: "2026-10-29T00:00:00Z".into(),
        source: SourceInfo {
            os: "macos".into(),
            user: "alice".into(),
            home: "/Users/alice".into(),
            hostname: "src".into(),
            tool_versions: BTreeMap::new(),
        },
        selection: Selection {
            tools: BTreeSet::from([Tool::Claude]),
            categories: BTreeMap::new(),
        },
        entries,
        projects: vec![],
        secrets: vec![],
        notes: vec![],
    }
}

fn secret() -> SecretItem {
    SecretItem {
        meta: SecretMeta {
            tool: Tool::Claude,
            kind: SecretKind::ClaudeKeychain,
            account_uuid: Some("00000000-0000-0000-0000-000000000001".into()),
            email: Some("alice@example.com".into()),
        },
        service: "Claude Code-credentials".into(),
        account_hint: "alice".into(),
        value: SecretBytes::new(b"{\"token\":\"synthetic-test-value\"}".to_vec()),
    }
}

/// Writes `files` (one entry per file, in order) and returns the manifest.
fn write_bundle(path: &Path, files: &[(&str, Vec<u8>)], with_secret: bool) -> Manifest {
    let mut w = BundleWriter::create(path, &pass()).unwrap();
    let mut entries = Vec::new();
    for (i, (rel, data)) in files.iter().enumerate() {
        let h = w.add_file(i as u32, &mut Cursor::new(data)).unwrap();
        assert_eq!(h, *blake3::hash(data).as_bytes());
        entries.push(file_entry(i as u32, rel, data.len() as u64, h));
    }
    if with_secret {
        w.add_secret(&secret()).unwrap();
    }
    let m = manifest(entries);
    w.finish(&m).unwrap();
    m
}

fn bundle_err(err: anyhow::Error) -> BundleError {
    err.downcast_ref::<BundleError>()
        .unwrap_or_else(|| panic!("not a BundleError: {err:#}"))
        .clone()
}

fn dir_is_empty(dir: &Path) -> bool {
    !dir.exists() || fs::read_dir(dir).unwrap().next().is_none()
}

#[test]
fn passphrase_has_expected_shape() {
    let a = generate_passphrase();
    let b = generate_passphrase();
    let s = a.expose_secret();
    let groups: Vec<&str> = s.split('-').collect();
    assert_eq!(groups.len(), 7, "{}", groups.len());
    assert!(groups[..6].iter().all(|g| g.len() == 4));
    assert_eq!(groups[6].len(), 2);
    let chars: String = groups.concat();
    assert_eq!(chars.len(), 26);
    assert!(
        chars
            .chars()
            .all(|c| c.is_ascii_uppercase() || ('2'..='7').contains(&c))
    );
    assert_ne!(a.expose_secret(), b.expose_secret());
}

#[test]
fn roundtrip_files_secrets_manifest() {
    let tmp = tempfile::tempdir().unwrap();
    let bundle = tmp.path().join("out.vtb");
    let staging = tmp.path().join("staging");
    let files = [
        ("a.jsonl", b"{\"type\":\"user\"}\n".to_vec()),
        ("empty.txt", Vec::new()),
        ("big.bin", noise(40 * MIB, "big")),
    ];

    let mut w = BundleWriter::create(&bundle, &pass()).unwrap();
    let mut entries = Vec::new();
    for (i, (rel, data)) in files.iter().enumerate() {
        let h = w.add_file(i as u32, &mut Cursor::new(data)).unwrap();
        assert_eq!(h, *blake3::hash(data).as_bytes());
        entries.push(file_entry(i as u32, rel, data.len() as u64, h));
    }
    w.add_secret(&secret()).unwrap();
    let m = manifest(entries);
    let summary = w.finish(&m).unwrap();

    assert_eq!(summary.segments, 2);
    assert_eq!(summary.merkle_root, merkle_root(&m.entries));
    assert_eq!(summary.bytes, fs::metadata(&bundle).unwrap().len());

    let read = read_bundle(&bundle, &pass(), &staging).unwrap();
    assert_eq!(read.manifest, m);
    assert_eq!(read.blobs, staging.join("blobs"));
    for (i, (_, data)) in files.iter().enumerate() {
        let blob = fs::read(read.blobs.join(i.to_string())).unwrap();
        assert!(blob == *data, "blob {i} differs");
    }
    assert_eq!(fs::read_dir(&read.blobs).unwrap().count(), 3);
    assert_eq!(read.secrets.len(), 1);
    let s = &read.secrets[0];
    assert_eq!(s.meta, secret().meta);
    assert_eq!(s.service, "Claude Code-credentials");
    assert_eq!(s.account_hint, "alice");
    assert_eq!(s.value.expose(), secret().value.expose());

    // The secret never appears on disk in plain text.
    let raw = fs::read(&bundle).unwrap();
    let needle = b"synthetic-test-value";
    assert!(!raw.windows(needle.len()).any(|w| w == needle));
}

#[test]
fn wrong_passphrase_fails_cleanly() {
    let tmp = tempfile::tempdir().unwrap();
    let bundle = tmp.path().join("out.vtb");
    let staging = tmp.path().join("staging");
    fs::create_dir(&staging).unwrap();
    write_bundle(&bundle, &[("a.txt", b"hello".to_vec())], false);

    let err = read_bundle(&bundle, &SecretString::from("wrong-pass"), &staging).unwrap_err();
    assert_eq!(bundle_err(err), BundleError::WrongPassphrase);
    assert!(dir_is_empty(&staging));
}

#[test]
fn tampered_segment_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let bundle = tmp.path().join("out.vtb");
    let staging = tmp.path().join("staging");
    fs::create_dir(&staging).unwrap();
    write_bundle(&bundle, &[("a.bin", noise(256 * 1024, "a"))], false);

    // Outer layer: flip one ciphertext byte without opening age.
    let mut raw = fs::read(&bundle).unwrap();
    let mid = raw.len() / 2;
    raw[mid] ^= 0x01;
    let outer = tmp.path().join("outer.vtb");
    fs::write(&outer, &raw).unwrap();
    let err = read_bundle(&outer, &pass(), &staging).unwrap_err();
    assert!(
        matches!(bundle_err(err), BundleError::Corrupt { .. }),
        "outer tamper"
    );
    assert!(dir_is_empty(&staging));

    // Inner stream: the stored hash of segment 0 starts after
    // magic(4) + kind(1) + len(4) + seq(4) = byte 13.
    vt_core::bundle::tamper_inner_stream(&bundle, &pass(), |inner| inner[13] ^= 0x01).unwrap();
    let err = read_bundle(&bundle, &pass(), &staging).unwrap_err();
    assert_eq!(
        bundle_err(err),
        BundleError::Corrupt {
            what: "segment 0".into()
        }
    );
    assert!(dir_is_empty(&staging));
}

#[test]
fn manifest_hash_mismatch_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let bundle = tmp.path().join("out.vtb");
    let staging = tmp.path().join("staging");
    fs::create_dir(&staging).unwrap();

    let mut w = BundleWriter::create(&bundle, &pass()).unwrap();
    let h = w.add_file(0, &mut Cursor::new(b"real content")).unwrap();
    let mut wrong = h;
    wrong[0] ^= 0xff;
    w.finish(&manifest(vec![file_entry(0, "a.txt", 12, wrong)]))
        .unwrap();

    let err = read_bundle(&bundle, &pass(), &staging).unwrap_err();
    assert!(matches!(bundle_err(err), BundleError::Corrupt { .. }));
    assert!(dir_is_empty(&staging));
}

#[test]
fn file_entry_without_hash_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let bundle = tmp.path().join("out.vtb");
    let staging = tmp.path().join("staging");

    let mut w = BundleWriter::create(&bundle, &pass()).unwrap();
    w.add_file(0, &mut Cursor::new(b"x")).unwrap();
    let mut e = file_entry(0, "a.txt", 1, [0; 32]);
    e.hash = None;
    w.finish(&manifest(vec![e])).unwrap();

    let err = read_bundle(&bundle, &pass(), &staging).unwrap_err();
    assert!(matches!(bundle_err(err), BundleError::Corrupt { .. }));
    assert!(dir_is_empty(&staging));
}

#[test]
fn blob_without_manifest_entry_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let bundle = tmp.path().join("out.vtb");
    let staging = tmp.path().join("staging");

    let mut w = BundleWriter::create(&bundle, &pass()).unwrap();
    let h = w.add_file(0, &mut Cursor::new(b"listed")).unwrap();
    w.add_file(7, &mut Cursor::new(b"stray")).unwrap();
    w.finish(&manifest(vec![file_entry(0, "a.txt", 6, h)]))
        .unwrap();

    let err = read_bundle(&bundle, &pass(), &staging).unwrap_err();
    assert!(matches!(bundle_err(err), BundleError::Corrupt { .. }));
    assert!(dir_is_empty(&staging));
}

#[test]
fn unsupported_version_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let bundle = tmp.path().join("out.vtb");
    let staging = tmp.path().join("staging");

    let mut w = BundleWriter::create(&bundle, &pass()).unwrap();
    let h = w.add_file(0, &mut Cursor::new(b"x")).unwrap();
    let mut m = manifest(vec![file_entry(0, "a.txt", 1, h)]);
    m.format_version = FORMAT_VERSION + 1;
    w.finish(&m).unwrap();

    let err = read_bundle(&bundle, &pass(), &staging).unwrap_err();
    assert_eq!(
        bundle_err(err),
        BundleError::UnsupportedVersion(FORMAT_VERSION + 1)
    );
    assert!(dir_is_empty(&staging));
}

#[test]
fn duplicate_file_index_is_refused_by_writer() {
    let tmp = tempfile::tempdir().unwrap();
    let mut w = BundleWriter::create(&tmp.path().join("out.vtb"), &pass()).unwrap();
    w.add_file(0, &mut Cursor::new(b"a")).unwrap();
    assert!(w.add_file(0, &mut Cursor::new(b"b")).is_err());
}

#[test]
fn create_refuses_existing_file() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("out.vtb");
    fs::write(&path, b"keep me").unwrap();
    assert!(BundleWriter::create(&path, &pass()).is_err());
    assert_eq!(fs::read(&path).unwrap(), b"keep me");
}

#[cfg(unix)]
#[test]
fn staging_and_bundle_permissions_are_private() {
    use std::os::unix::fs::PermissionsExt;
    let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;

    let tmp = tempfile::tempdir().unwrap();
    let bundle = tmp.path().join("out.vtb");
    let staging = tmp.path().join("staging");
    write_bundle(&bundle, &[("a.txt", b"transcript".to_vec())], false);
    assert_eq!(mode(&bundle), 0o600);

    let read = read_bundle(&bundle, &pass(), &staging).unwrap();
    assert_eq!(mode(&staging), 0o700);
    assert_eq!(mode(&read.blobs), 0o700);
    assert_eq!(mode(&read.blobs.join("0")), 0o600);
}

#[test]
fn blob_for_directory_entry_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let bundle = tmp.path().join("out.vtb");
    let staging = tmp.path().join("staging");

    let mut w = BundleWriter::create(&bundle, &pass()).unwrap();
    w.add_file(0, &mut Cursor::new(b"not a dir")).unwrap();
    let mut e = file_entry(0, "dir", 0, [0; 32]);
    e.kind = EntryKind::Dir;
    e.hash = None;
    w.finish(&manifest(vec![e])).unwrap();

    let err = read_bundle(&bundle, &pass(), &staging).unwrap_err();
    assert!(matches!(bundle_err(err), BundleError::Corrupt { .. }));
    assert!(dir_is_empty(&staging));
}

/// Inner stream frames as `(start, kind, total length with header)`.
fn frames(inner: &[u8]) -> Vec<(usize, u8, usize)> {
    let mut out = Vec::new();
    let mut at = 4;
    while at < inner.len() {
        let len = u32::from_be_bytes(inner[at + 1..at + 5].try_into().unwrap()) as usize;
        out.push((at, inner[at], 5 + len));
        at += 5 + len;
    }
    out
}

/// Writes a one-file bundle, applies `edit` to its inner stream and reads it.
fn read_after_tamper(edit: impl FnOnce(&mut Vec<u8>)) -> BundleError {
    let tmp = tempfile::tempdir().unwrap();
    let bundle = tmp.path().join("out.vtb");
    let staging = tmp.path().join("staging");
    write_bundle(&bundle, &[("a.txt", b"hello".to_vec())], false);
    vt_core::bundle::tamper_inner_stream(&bundle, &pass(), edit).unwrap();
    let err = read_bundle(&bundle, &pass(), &staging).unwrap_err();
    assert!(dir_is_empty(&staging));
    bundle_err(err)
}

#[test]
fn stream_without_end_is_rejected() {
    let err = read_after_tamper(|inner| {
        let (start, kind, _) = *frames(inner).last().unwrap();
        assert_eq!(kind, 4);
        inner.truncate(start);
    });
    assert!(matches!(err, BundleError::Corrupt { .. }), "{err:?}");
}

#[test]
fn trailing_data_after_end_is_rejected() {
    let err = read_after_tamper(|inner| inner.push(0));
    assert!(matches!(err, BundleError::Corrupt { .. }), "{err:?}");
}

#[test]
fn duplicated_segment_is_rejected() {
    let err = read_after_tamper(|inner| {
        let (start, kind, len) = frames(inner)[0];
        assert_eq!(kind, 1);
        let copy = inner[start..start + len].to_vec();
        inner.splice(start + len..start + len, copy);
    });
    assert_eq!(
        err,
        BundleError::Corrupt {
            what: "segment 1".into()
        }
    );
}

#[test]
fn oversized_frame_is_rejected() {
    let err = read_after_tamper(|inner| {
        let (start, kind, _) = frames(inner)[0];
        assert_eq!(kind, 1);
        inner[start + 1..start + 5].copy_from_slice(&u32::MAX.to_be_bytes());
    });
    assert_eq!(
        err,
        BundleError::Corrupt {
            what: "segment 0 boyutu".into()
        }
    );
}
