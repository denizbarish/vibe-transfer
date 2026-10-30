//! `scan` and `export` end to end, against a fixture home selected with
//! `VT_TEST_HOME`; secrets come from `VT_TEST_SECRETS` into a `MemStore`.

// Fixtures use POSIX modes.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Output;

use vt_core::bundle::{ReadBundle, SecretString, read_bundle};
use vt_core::manifest::{Category, EntryKind, Manifest, RootKind};

const HOME_PLACEHOLDER: &str = "@HOME@";
const PASS: &str = "test-pass-alice";
const SESSION_REL: &str = "projects/-Users-src-app/s-app.jsonl";

struct Fixture {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    secrets: PathBuf,
    out_dir: PathBuf,
}

/// A synthetic token, built at run time so no token-shaped literal sits in
/// the source.
fn fake_token() -> String {
    format!("{}{}", "sk-ant-", "oat01-synthetic0value0for0tests")
}

fn fake_github_token() -> String {
    format!("{}{}", "gh", "p_0synthetic0github0token0value0x")
}

/// Copies the Claude adapter's fixture home and writes the secret file.
fn setup() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("../vt-claude/tests/fixtures/home");
    copy_tree(&src, &home, home.to_str().unwrap());

    let secrets = tmp.path().join("secrets.json");
    let items = serde_json::json!([{
        "service": "Claude Code-credentials",
        "account": "alice",
        "value": format!("{{\"claudeAiOauth\":{{\"accessToken\":\"{}\"}}}}", fake_token()),
    }]);
    fs::write(&secrets, items.to_string()).unwrap();

    let out_dir = tmp.path().join("out");
    fs::create_dir(&out_dir).unwrap();
    Fixture {
        _tmp: tmp,
        home,
        secrets,
        out_dir,
    }
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

/// Runs the binary inside the fixture only: `HOME` points there too, and
/// nothing is inherited that could reach real Claude data.
fn run(fx: &Fixture, args: &[&str], stdin: &str) -> Output {
    let mut cmd = assert_cmd::cargo::cargo_bin_cmd!("vibe-transfer");
    cmd.args(args)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", &fx.home)
        .env("USER", "alice")
        .env("VT_TEST_HOME", &fx.home)
        .env("VT_TEST_SECRETS", &fx.secrets)
        .write_stdin(stdin);
    cmd.output().unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn export(fx: &Fixture, name: &str, extra: &[&str]) -> (PathBuf, Output) {
    let out = fx.out_dir.join(name);
    let mut args = vec![
        "export",
        "--out",
        out.to_str().unwrap(),
        "--passphrase-stdin",
    ];
    args.extend_from_slice(extra);
    let output = run(fx, &args, &format!("{PASS}\n"));
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        text(&output.stdout),
        text(&output.stderr)
    );
    (out, output)
}

fn open(fx: &Fixture, bundle: &Path) -> ReadBundle {
    let staging = fx.out_dir.join(format!(
        "staging-{}",
        bundle.file_name().unwrap().to_str().unwrap()
    ));
    read_bundle(bundle, &SecretString::from(PASS.to_owned()), &staging).unwrap()
}

fn mode(path: &Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

fn has_note(m: &Manifest, needle: &str) -> bool {
    m.notes.iter().any(|n| n.contains(needle))
}

#[test]
fn export_creates_bundle_and_outbox() {
    let fx = setup();
    let (bundle, output) = export(&fx, "a.vtb", &[]);
    let stdout = text(&output.stdout);
    let stderr = text(&output.stderr);

    assert_eq!(mode(&bundle), 0o600);
    let read = open(&fx, &bundle);
    let m = &read.manifest;
    let rels: Vec<&str> = m
        .entries
        .iter()
        .filter(|e| e.root == RootKind::ClaudeDir)
        .map(|e| e.rel.as_str())
        .collect();
    assert!(rels.contains(&"CLAUDE.md"), "{rels:?}");
    assert!(rels.contains(&SESSION_REL), "{rels:?}");
    assert!(m.entries.iter().any(|e| e.root == RootKind::ClaudeJson));
    assert!(!m.entries.iter().any(|e| e.category == Category::Cache));
    assert_eq!(m.source.user, "alice");

    // The Keychain item travels as an encrypted secret, never as a file.
    assert_eq!(read.secrets.len(), 1);
    assert_eq!(m.secrets.len(), 1);
    assert!(
        String::from_utf8_lossy(read.secrets[0].value.expose()).contains(&fake_token()),
        "secret value lost"
    );

    // Skipped items and inventory notes reach the manifest and the summary.
    assert!(has_note(m, "sessions"), "{:?}", m.notes);
    assert!(has_note(m, "daemon"), "{:?}", m.notes);
    assert!(has_note(m, "mystery.txt"), "{:?}", m.notes);
    assert!(stdout.contains("sessions"), "{stdout}");
    assert!(stdout.contains("mystery.txt"), "{stdout}");

    // Outbox: owner-only dirs and file, value-free metadata.
    let vt_home = fx.home.join(".vibe-transfer");
    let session_dir = vt_home.join("outbox").join(&m.session_id);
    let meta_path = session_dir.join("meta.json");
    assert_eq!(mode(&vt_home), 0o700);
    assert_eq!(mode(&vt_home.join("outbox")), 0o700);
    assert_eq!(mode(&session_dir), 0o700);
    assert_eq!(mode(&meta_path), 0o600);
    let meta_text = fs::read_to_string(&meta_path).unwrap();
    let meta: serde_json::Value = serde_json::from_str(&meta_text).unwrap();
    assert_eq!(meta["session_id"], m.session_id.as_str());
    assert_eq!(
        Path::new(meta["out_path"].as_str().unwrap()),
        fs::canonicalize(&bundle).unwrap()
    );
    assert!(meta["created_at"].as_str().unwrap().ends_with('Z'));
    assert_eq!(meta["secrets"].as_array().unwrap().len(), 1);
    assert_eq!(meta["secrets"][0]["kind"], "claude_keychain");

    // Neither the secret nor the passphrase is printed or stored in clear.
    for shown in [&stdout, &stderr, &meta_text] {
        assert!(!shown.contains(&fake_token()), "secret leaked:\n{shown}");
        assert!(!shown.contains(PASS), "passphrase leaked:\n{shown}");
    }
    assert!(stdout.contains(&m.session_id), "{stdout}");
    assert!(
        stderr.contains("Parolayı paket dosyasıyla aynı kanaldan gönderme"),
        "{stderr}"
    );
}

#[test]
fn export_outbox_failure_keeps_usable_bundle() {
    let fx = setup();
    // An outbox nobody can write into: the session dir cannot be created.
    let outbox = fx.home.join(".vibe-transfer/outbox");
    fs::create_dir_all(&outbox).unwrap();
    fs::set_permissions(&outbox, fs::Permissions::from_mode(0o500)).unwrap();

    let bundle = fx.out_dir.join("e.vtb");
    let output = run(
        &fx,
        &[
            "export",
            "--out",
            bundle.to_str().unwrap(),
            "--passphrase-stdin",
        ],
        &format!("{PASS}\n"),
    );
    fs::set_permissions(&outbox, fs::Permissions::from_mode(0o700)).unwrap();
    let stderr = text(&output.stderr);

    assert!(!output.status.success(), "{stderr}");
    assert!(
        stderr.contains("paket geçerli ve kullanılabilir"),
        "{stderr}"
    );
    assert!(stderr.contains("oturum kaydı"), "{stderr}");
    assert!(
        stderr.contains("Parolayı paket dosyasıyla aynı kanaldan gönderme"),
        "{stderr}"
    );
    assert!(!stderr.contains(PASS), "{stderr}");
    // The bundle survived and opens with the passphrase.
    assert!(bundle.exists());
    let read = open(&fx, &bundle);
    assert!(!read.manifest.entries.is_empty());
    assert_eq!(read.secrets.len(), 1);
}

#[test]
fn export_respects_exclude_sessions() {
    let fx = setup();
    let (bundle, _) = export(&fx, "b.vtb", &["--exclude", "sessions"]);
    let m = open(&fx, &bundle).manifest;
    assert!(
        !m.entries.iter().any(|e| e.category == Category::Sessions),
        "{:?}",
        m.entries
    );
    assert!(m.entries.iter().any(|e| e.category == Category::Memory));
    assert!(has_note(&m, "kullanıcı seçimiyle atlandı"), "{:?}", m.notes);
    let sessions = m.selection.categories.values().next().unwrap();
    assert!(!sessions.contains(&Category::Sessions));
}

#[test]
fn export_truncates_partial_jsonl_tail() {
    let fx = setup();
    let path = fx.home.join(".claude").join(SESSION_REL);
    let complete = fs::read(&path).unwrap();
    assert_eq!(complete.last(), Some(&b'\n'));
    // Claude was writing a line when the export started.
    let mut torn = complete.clone();
    torn.extend_from_slice(b"{\"type\":\"user\",\"cwd\":\"/half");
    fs::write(&path, &torn).unwrap();

    let (bundle, _) = export(&fx, "c.vtb", &[]);
    let read = open(&fx, &bundle);
    let entry = read
        .manifest
        .entries
        .iter()
        .find(|e| e.root == RootKind::ClaudeDir && e.rel == SESSION_REL)
        .unwrap();
    assert!(entry.partial_tail);
    let blob = fs::read(read.blobs.join(entry.index.to_string())).unwrap();
    assert_eq!(blob, complete);
    assert_eq!(entry.size, complete.len() as u64);

    // A complete jsonl keeps all its bytes and no flag.
    let plain = read
        .manifest
        .entries
        .iter()
        .find(|e| e.rel == "projects/-Users-src-plain/s-plain.jsonl")
        .unwrap();
    assert!(!plain.partial_tail);
    assert_eq!(plain.kind, EntryKind::File);
}

#[test]
fn export_without_terminal_requires_passphrase_stdin() {
    let fx = setup();
    let out = fx.out_dir.join("d.vtb");
    let output = run(&fx, &["export", "--out", out.to_str().unwrap()], "");
    assert!(!output.status.success());
    assert!(
        text(&output.stderr).contains("--passphrase-stdin"),
        "{}",
        text(&output.stderr)
    );
    assert!(!out.exists());
}

#[test]
fn scan_lists_secret_patterns_without_values() {
    let fx = setup();
    let claude = fx.home.join(".claude");
    let history = claude.join("history.jsonl");
    let mut lines = fs::read_to_string(&history).unwrap();
    lines.push_str(&format!(
        "{{\"display\":\"export GH={}\"}}\n",
        fake_github_token()
    ));
    fs::write(&history, lines).unwrap();
    let key_body = "MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQC";
    fs::write(
        claude.join("hooks/deploy.pem"),
        format!(
            "-----BEGIN {}-----\n{key_body}\n-----END {}-----\n",
            "PRIVATE KEY", "PRIVATE KEY"
        ),
    )
    .unwrap();

    let output = run(&fx, &["scan"], "");
    let stdout = text(&output.stdout);
    let stderr = text(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    assert!(stdout.contains("history.jsonl"), "{stdout}");
    assert!(stdout.contains("hooks/deploy.pem"), "{stdout}");
    assert!(
        !stdout.contains("CLAUDE.md\n"),
        "clean file listed:\n{stdout}"
    );
    assert!(stdout.contains("test modu"), "{stdout}");
    for shown in [&stdout, &stderr] {
        assert!(
            !shown.contains(&fake_github_token()),
            "value leaked:\n{shown}"
        );
        assert!(!shown.contains(key_body), "value leaked:\n{shown}");
        assert!(!shown.contains(&fake_token()), "value leaked:\n{shown}");
    }

    let output = run(&fx, &["scan", "--json"], "");
    assert!(output.status.success());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["test_mode"], true);
    let files: Vec<&str> = json["secret_pattern_files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(files, vec!["history.jsonl", "hooks/deploy.pem"]);
    assert_eq!(json["secret_pattern_count"], 2);
    assert!(!text(&output.stdout).contains(&fake_github_token()));
}
