//! After the import: `pending`, `link`, `verify`, `rollback` and, on the
//! source, `logout-source`. Synthetic homes `src` and `dst` are selected
//! with `VT_TEST_HOME`; secrets live in a `MemStore` (`VT_TEST_SECRETS`) and
//! are observed only as digests (`VT_TEST_SECRETS_DUMP`). Answers come from
//! `VT_TEST_ANSWERS`.

// Fixtures use POSIX modes.
#![cfg(unix)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

use serde_json::{Value, json};
use vt_core::bundle::{SecretString, decrypt_file};
use vt_core::pathcodec::claude_slug;

const PASS: &str = "test-pass-lifecycle";
const BACKUP_PASS: &str = "test-pass-logout-backup";
const SERVICE: &str = "Claude Code-credentials";
const LOST_REMOTE: &str = "github.com/acme/lost";

/// Built at run time so no token-shaped literal sits in the source.
fn fake_token() -> String {
    format!(
        "{{\"claudeAiOauth\":{{\"accessToken\":\"{}{}lifecycle\"}}}}",
        "sk-ant-", "oat01-synthetic0"
    )
}

struct Fx {
    tmp: tempfile::TempDir,
    src: PathBuf,
    dst: PathBuf,
}

impl Fx {
    fn src_app(&self) -> PathBuf {
        self.src.join("work/app")
    }
    fn src_lost(&self) -> PathBuf {
        self.src.join("work/lost")
    }
    fn dst_app(&self) -> PathBuf {
        self.dst.join("code/app")
    }
    fn dst_lost(&self) -> PathBuf {
        self.dst.join("code/lost")
    }
    fn slug_dir(&self, home: &Path, project: &Path) -> PathBuf {
        home.join(".claude/projects")
            .join(claude_slug(project.to_str().unwrap()))
    }
    fn dst_vt(&self) -> PathBuf {
        self.dst.join(".vibe-transfer")
    }
    fn secrets_file(&self) -> PathBuf {
        self.tmp.path().join("src-secrets.json")
    }
}

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn git_repo(root: &Path, url: &str) {
    write(
        &root.join(".git/config"),
        &format!("[remote \"origin\"]\n\turl = {url}\n"),
    );
}

fn session(path: &Path, cwd: &Path, id: &str) {
    write(
        path,
        &format!("{}\n", json!({"type": "user", "cwd": cwd, "sessionId": id})),
    );
}

/// Source: `app` (also on the target) and `lost` (not yet), with a session
/// in a subfolder of `lost` and `lost` in `githubRepoPaths`.
fn setup() -> Fx {
    let tmp = tempfile::tempdir().unwrap();
    let fx = Fx {
        src: tmp.path().join("src"),
        dst: tmp.path().join("dst"),
        tmp,
    };
    let claude = fx.src.join(".claude");
    git_repo(&fx.src_app(), "https://github.com/acme/app.git");
    write(&fx.src_app().join("CLAUDE.md"), "# app\n");
    git_repo(&fx.src_lost(), "https://github.com/acme/lost.git");
    write(&fx.src_lost().join("CLAUDE.md"), "# lost\n");
    write(&claude.join("CLAUDE.md"), "# global\n");

    session(
        &fx.slug_dir(&fx.src, &fx.src_app()).join("sess-app.jsonl"),
        &fx.src_app(),
        "sess-app",
    );
    session(
        &fx.slug_dir(&fx.src, &fx.src_lost()).join("sess-lost.jsonl"),
        &fx.src_lost(),
        "sess-lost",
    );
    let sub = fx.src_lost().join("pkg/web");
    session(
        &fx.slug_dir(&fx.src, &sub).join("sess-sub.jsonl"),
        &sub,
        "sess-sub",
    );
    let lost = fx.src_lost().to_str().unwrap().to_owned();
    write(
        &fx.src.join(".claude.json"),
        &json!({
            "projects": {
                fx.src_app().to_str().unwrap(): {"allowedTools": ["Bash"]},
                lost.clone(): {"allowedTools": ["Read"]},
            },
            "githubRepoPaths": {"acme/lost": [lost]},
            "oauthAccount": {"accountUuid": "uuid-src"},
        })
        .to_string(),
    );
    write(
        &fx.secrets_file(),
        &json!([{"service": SERVICE, "account": "src", "value": fake_token()}]).to_string(),
    );
    git_repo(&fx.dst_app(), "git@github.com:acme/app.git");
    write(&fx.dst_app().join("notes.txt"), "target only\n");
    fs::create_dir_all(&fx.dst).unwrap();
    fx
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// Runs the binary inside one synthetic home only.
fn run(
    home: &Path,
    user: &str,
    args: &[&str],
    envs: &[(&str, &Path)],
    answers: Option<Value>,
    stdin: &str,
) -> Output {
    let mut cmd = assert_cmd::cargo::cargo_bin_cmd!("vibe-transfer");
    cmd.args(args)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home)
        .env("USER", user)
        .env("VT_TEST_HOME", home)
        .write_stdin(stdin.to_owned());
    for (k, v) in envs {
        cmd.env(k, v);
    }
    if let Some(a) = answers {
        cmd.env("VT_TEST_ANSWERS", a.to_string());
    }
    cmd.output().unwrap()
}

fn ok(out: &Output) {
    assert!(
        out.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        text(&out.stdout),
        text(&out.stderr)
    );
}

fn json_out(out: &Output) -> Value {
    serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|_| panic!("not JSON:\n{}\n{}", text(&out.stdout), text(&out.stderr)))
}

/// Exports `src`; returns the bundle and the export session id.
fn export(fx: &Fx) -> (PathBuf, String) {
    let out = fx.tmp.path().join("bundle.vtb");
    let output = run(
        &fx.src,
        "src",
        &[
            "export",
            "--out",
            out.to_str().unwrap(),
            "--passphrase-stdin",
            "--json",
        ],
        &[("VT_TEST_SECRETS", &fx.secrets_file())],
        None,
        &format!("{PASS}\n"),
    );
    ok(&output);
    let session = json_out(&output)["session_id"].as_str().unwrap().to_owned();
    (out, session)
}

/// Imports into `dst`; returns the import session id.
fn import(fx: &Fx, bundle: &Path, answers: Option<Value>) -> String {
    import_with(fx, bundle, answers, None)
}

/// Imports with `target_secrets` in the target store first.
fn import_with(
    fx: &Fx,
    bundle: &Path,
    answers: Option<Value>,
    target_secrets: Option<Value>,
) -> String {
    let file = fx.tmp.path().join("dst-secrets.json");
    let mut envs: Vec<(&str, &Path)> = Vec::new();
    if let Some(items) = target_secrets {
        fs::write(&file, items.to_string()).unwrap();
        envs.push(("VT_TEST_SECRETS", &file));
    }
    let output = run(
        &fx.dst,
        "dst",
        &[
            "import",
            bundle.to_str().unwrap(),
            "--passphrase-stdin",
            "--json",
        ],
        &envs,
        answers,
        &format!("{PASS}\n"),
    );
    ok(&output);
    json_out(&output)["session_id"].as_str().unwrap().to_owned()
}

fn dst(fx: &Fx, args: &[&str], answers: Option<Value>) -> Output {
    run(&fx.dst, "dst", args, &[], answers, "")
}

fn digest(value: &str) -> String {
    blake3::hash(value.as_bytes()).to_hex().to_string()
}

/// Runs in `dst` with the target store holding `store` (each run starts a
/// fresh in-memory store, so the state after the import is given here);
/// returns the output and the store digests afterwards.
fn dst_store(
    fx: &Fx,
    args: &[&str],
    answers: Option<Value>,
    store: Value,
    stdin: &str,
) -> (Output, Vec<Value>) {
    let file = fx.tmp.path().join("dst-store.json");
    let dump = fx.tmp.path().join("dst-dump.json");
    let _ = fs::remove_file(&dump);
    fs::write(&file, store.to_string()).unwrap();
    let out = run(
        &fx.dst,
        "dst",
        args,
        &[("VT_TEST_SECRETS", &file), ("VT_TEST_SECRETS_DUMP", &dump)],
        answers,
        stdin,
    );
    let secrets = fs::read(&dump)
        .ok()
        .map(|b| serde_json::from_slice(&b).unwrap())
        .unwrap_or_default();
    (out, secrets)
}

fn dst_digest(secrets: &[Value]) -> Option<String> {
    secrets
        .iter()
        .find(|s| s["service"] == SERVICE && s["account"] == "dst")
        .map(|s| s["blake3"].as_str().unwrap().to_owned())
}

fn read_json(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

/// Every file and symlink below `root` (outside `skip`) with its content.
fn snapshot(root: &Path, skip: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for e in fs::read_dir(&dir).unwrap() {
            let path = e.unwrap().path();
            if path.starts_with(skip) {
                continue;
            }
            let md = fs::symlink_metadata(&path).unwrap();
            if md.is_dir() {
                stack.push(path);
            } else if md.file_type().is_symlink() {
                let t = fs::read_link(&path).unwrap();
                out.insert(path, t.to_string_lossy().as_bytes().to_vec());
            } else {
                out.insert(path.clone(), fs::read(&path).unwrap());
            }
        }
    }
    out
}

fn pending_ids(fx: &Fx) -> Vec<String> {
    let out = dst(fx, &["pending", "--json"], None);
    ok(&out);
    json_out(&out)["projects"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["id"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn link_moves_pending_into_cloned_project() {
    let fx = setup();
    let (bundle, _) = export(&fx);
    import(&fx, &bundle, None);
    assert_eq!(pending_ids(&fx), vec![LOST_REMOTE.to_owned()]);
    let listed = dst(&fx, &["pending"], None);
    ok(&listed);
    assert!(text(&listed.stdout).contains(LOST_REMOTE));

    // The user clones the project on the target, then links.
    git_repo(&fx.dst_lost(), "git@github.com:acme/lost.git");
    let out = dst(&fx, &["link", "--auto", "--json"], None);
    ok(&out);
    let summary = json_out(&out);
    assert_eq!(summary["linked"][0]["project"], LOST_REMOTE);

    assert_eq!(
        fs::read_to_string(fx.dst_lost().join("CLAUDE.md")).unwrap(),
        "# lost\n"
    );
    assert!(
        fx.slug_dir(&fx.dst, &fx.dst_lost())
            .join("sess-lost.jsonl")
            .is_file()
    );
    // A session started in a subfolder lands in that subfolder's slug.
    assert!(
        fx.slug_dir(&fx.dst, &fx.dst_lost().join("pkg/web"))
            .join("sess-sub.jsonl")
            .is_file()
    );
    let claude_json = read_json(&fx.dst.join(".claude.json"));
    let lost = fx.dst_lost().to_str().unwrap().to_owned();
    assert_eq!(
        claude_json["projects"][&lost]["allowedTools"],
        json!(["Read"]),
        "{claude_json:#}"
    );
    assert_eq!(claude_json["githubRepoPaths"]["acme/lost"], json!([lost]));
    assert!(pending_ids(&fx).is_empty());

    // The link has its own report, and verify accepts it.
    let session = summary["linked"][0]["session_id"].as_str().unwrap();
    let verified = dst(&fx, &["verify", session], None);
    ok(&verified);
}

#[test]
fn link_to_home_needs_confirmation_and_keeps_pending() {
    let fx = setup();
    let (bundle, _) = export(&fx);
    import(&fx, &bundle, None);
    let home = fx.dst.to_str().unwrap();
    let out = dst(&fx, &["link", LOST_REMOTE, home], None);
    assert!(!out.status.success());
    assert!(
        text(&out.stderr).contains("terminal yok"),
        "{}",
        text(&out.stderr)
    );
    assert_eq!(pending_ids(&fx), vec![LOST_REMOTE.to_owned()]);
    assert!(!fx.dst.join("CLAUDE.md").exists());

    // By old path, to a folder whose CLAUDE.md differs: the import's
    // merge or replace question, which no flag skips.
    write(&fx.dst_lost().join("CLAUDE.md"), "# target lost\n");
    let old = fx.src_lost().to_str().unwrap().to_owned();
    let target = fx.dst_lost().to_str().unwrap().to_owned();
    let out = dst(&fx, &["link", &old, &target], None);
    assert!(!out.status.success());
    assert!(
        text(&out.stderr).contains("terminal yok"),
        "{}",
        text(&out.stderr)
    );
    assert_eq!(pending_ids(&fx), vec![LOST_REMOTE.to_owned()]);

    let answers = json!({"projects": {LOST_REMOTE: "merge"}, "file": "both"});
    let out = dst(&fx, &["link", &old, &target], Some(answers));
    ok(&out);
    assert_eq!(
        fs::read_to_string(fx.dst_lost().join("CLAUDE.md")).unwrap(),
        "# target lost\n"
    );
    assert_eq!(
        fs::read_to_string(fx.dst_lost().join("CLAUDE.from-pending.md")).unwrap(),
        "# lost\n"
    );
    assert!(pending_ids(&fx).is_empty());
}

#[test]
fn verify_detects_modified_file() {
    let fx = setup();
    let (bundle, _) = export(&fx);
    let session = import(&fx, &bundle, None);
    let out = dst(&fx, &["verify", &session, "--json"], None);
    ok(&out);
    assert!(json_out(&out)["checked"].as_u64().unwrap() > 0);
    // Without a session: the latest report.
    ok(&dst(&fx, &["verify"], None));

    let md = fx.dst_app().join("CLAUDE.md");
    fs::write(&md, "# changed\n").unwrap();
    let out = dst(&fx, &["verify", &session], None);
    assert_eq!(out.status.code(), Some(1));
    let shown = text(&out.stdout);
    assert!(shown.contains(md.to_str().unwrap()), "{shown}");
    assert!(!shown.contains("# app"), "{shown}");
}

#[test]
fn rollback_restores_pre_import_state() {
    let fx = setup();
    let (bundle, _) = export(&fx);
    write(&fx.dst.join(".claude/CLAUDE.md"), "# target global\n");
    let before = snapshot(&fx.dst, &fx.dst_vt());
    let session = import(&fx, &bundle, Some(json!({"decision": "replace"})));
    assert_ne!(snapshot(&fx.dst, &fx.dst_vt()), before);

    // No terminal, no answer: refused, nothing changes.
    let out = dst(&fx, &["rollback"], None);
    assert!(!out.status.success());
    assert!(
        text(&out.stderr).contains("terminal yok"),
        "{}",
        text(&out.stderr)
    );
    let out = dst(
        &fx,
        &["rollback", &session],
        Some(json!({"rollback": false})),
    );
    assert!(!out.status.success());
    assert_ne!(snapshot(&fx.dst, &fx.dst_vt()), before);

    // The imported token has since been refreshed: it is removed anyway,
    // and the summary says so. No previous value, so no passphrase needed.
    let refreshed = format!("{}-refreshed", fake_token());
    let (out, secrets) = dst_store(
        &fx,
        &["rollback"],
        Some(json!({"rollback": true})),
        json!([{"service": SERVICE, "account": "dst", "value": refreshed}]),
        "",
    );
    ok(&out);
    assert_eq!(snapshot(&fx.dst, &fx.dst_vt()), before);
    assert_eq!(dst_digest(&secrets), None, "{secrets:?}");
    assert!(
        text(&out.stdout).contains("değişmişti"),
        "{}",
        text(&out.stdout)
    );
}

#[test]
fn rollback_restores_the_previous_secret_with_the_passphrase() {
    let fx = setup();
    let (bundle, _) = export(&fx);
    let previous = format!("{}-target", fake_token());
    let session = import_with(
        &fx,
        &bundle,
        None,
        Some(json!([{"service": SERVICE, "account": "dst", "value": previous}])),
    );
    let after_import = json!([{"service": SERVICE, "account": "dst", "value": fake_token()}]);
    let yes = Some(json!({"rollback": true}));

    // No passphrase: files are rolled back, the secret step waits.
    let (out, secrets) = dst_store(
        &fx,
        &["rollback", &session],
        yes.clone(),
        after_import.clone(),
        "",
    );
    assert!(!out.status.success());
    let err = text(&out.stderr);
    assert!(err.contains("sır adımı bekliyor"), "{err}");
    assert!(
        err.contains(&format!("vibe-transfer rollback {session}")),
        "{err}"
    );
    assert_eq!(dst_digest(&secrets), Some(digest(&fake_token())));
    assert!(!fx.dst_app().join("CLAUDE.md").exists());

    // A wrong passphrase changes nothing either.
    let (out, secrets) = dst_store(
        &fx,
        &["rollback", &session, "--passphrase-stdin"],
        yes.clone(),
        after_import.clone(),
        "wrong-pass\n",
    );
    assert!(!out.status.success());
    assert_eq!(dst_digest(&secrets), Some(digest(&fake_token())));

    let (out, secrets) = dst_store(
        &fx,
        &["rollback", &session, "--passphrase-stdin"],
        yes,
        after_import,
        &format!("{PASS}\n"),
    );
    ok(&out);
    assert_eq!(dst_digest(&secrets), Some(digest(&previous)));
    let shown = format!("{}{}", text(&out.stdout), text(&out.stderr));
    assert!(!shown.contains("sk-ant-"), "{shown}");
}

#[test]
fn rollback_of_a_link_puts_the_project_back_into_pending() {
    let fx = setup();
    let (bundle, _) = export(&fx);
    import(&fx, &bundle, None);
    git_repo(&fx.dst_lost(), "git@github.com:acme/lost.git");
    // A record written by an older version: absolute blob paths.
    let project_dir = fs::read_dir(fx.dst_vt().join("pending"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let record_path = project_dir.join("pending.json");
    let mut record = read_json(&record_path);
    for item in record["items"].as_array_mut().unwrap() {
        let abs = project_dir.join(item["blob"].as_str().unwrap());
        item["blob"] = abs.to_str().unwrap().into();
    }
    fs::write(&record_path, record.to_string()).unwrap();
    let before = snapshot(&fx.dst, &fx.dst_vt());
    let out = dst(&fx, &["link", "--auto", "--json"], None);
    ok(&out);
    let session = json_out(&out)["linked"][0]["session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(pending_ids(&fx).is_empty());

    let out = dst(
        &fx,
        &["rollback", &session],
        Some(json!({"rollback": true})),
    );
    ok(&out);
    assert_eq!(snapshot(&fx.dst, &fx.dst_vt()), before);
    assert_eq!(pending_ids(&fx), vec![LOST_REMOTE.to_owned()]);
    // The restored record is readable and links again.
    let out = dst(&fx, &["link", "--auto", "--json"], None);
    ok(&out);
    assert_eq!(
        fs::read_to_string(fx.dst_lost().join("CLAUDE.md")).unwrap(),
        "# lost\n"
    );
}

#[test]
fn pending_orphans_are_removed_only_with_confirmation() {
    let fx = setup();
    let (bundle, _) = export(&fx);
    import(&fx, &bundle, None);
    let pending = fx.dst_vt().join("pending");
    let project_dir = fs::read_dir(&pending)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let stray = project_dir.join("blobs/ffff-9");
    fs::write(&stray, b"lost blob").unwrap();
    let broken = pending.join("0123456789abcdef");
    write(&broken.join("pending.json"), "{ not json");

    // The corrupt record is reported; the good one is still listed.
    let out = dst(&fx, &["pending", "--json"], None);
    ok(&out);
    let listed = json_out(&out);
    assert_eq!(listed["projects"].as_array().unwrap().len(), 1);
    assert!(
        listed["corrupt"][0]["path"]
            .as_str()
            .unwrap()
            .contains("0123456789abcdef")
    );

    let out = dst(&fx, &["pending", "--orphans"], None);
    ok(&out);
    assert!(
        text(&out.stdout).contains("ffff-9"),
        "{}",
        text(&out.stdout)
    );
    assert!(stray.exists());
    let out = dst(
        &fx,
        &["pending", "--orphans"],
        Some(json!({"orphans": false})),
    );
    ok(&out);
    assert!(stray.exists());
    let out = dst(
        &fx,
        &["pending", "--orphans"],
        Some(json!({"orphans": true})),
    );
    ok(&out);
    assert!(!stray.exists());
    assert!(broken.join("pending.json").exists());
    assert_eq!(pending_ids(&fx), vec![LOST_REMOTE.to_owned()]);
}

fn logout(fx: &Fx, session: &str, answers: Option<Value>) -> (Output, Vec<Value>) {
    let dump = fx.tmp.path().join("dump.json");
    let _ = fs::remove_file(&dump);
    let out = run(
        &fx.src,
        "src",
        &["logout-source", "--session", session, "--passphrase-stdin"],
        &[
            ("VT_TEST_SECRETS", &fx.secrets_file()),
            ("VT_TEST_SECRETS_DUMP", &dump),
        ],
        answers,
        &format!("{BACKUP_PASS}\n"),
    );
    let secrets = fs::read(&dump)
        .ok()
        .map(|b| serde_json::from_slice(&b).unwrap())
        .unwrap_or_default();
    (out, secrets)
}

#[test]
fn logout_source_backs_up_then_deletes_secret() {
    let fx = setup();
    let (_, session) = export(&fx);
    let backup = fx
        .src
        .join(".vibe-transfer/backups")
        .join(format!("logout-{session}"))
        .join("secrets.age");

    // Claude still running and no terminal: nothing happens.
    let (out, secrets) = logout(&fx, &session, Some(json!({"claude_pids": [4242]})));
    assert!(!out.status.success());
    assert!(
        text(&out.stderr).contains("Claude"),
        "{}",
        text(&out.stderr)
    );
    assert_eq!(secrets.len(), 1);
    assert!(!backup.exists());

    let (out, secrets) = logout(&fx, &session, None);
    ok(&out);
    assert!(secrets.is_empty(), "{secrets:?}");
    let shown = format!("{}{}", text(&out.stdout), text(&out.stderr));
    assert!(!shown.contains("sk-ant-"), "{shown}");
    assert!(!shown.contains(BACKUP_PASS), "{shown}");
    let raw = fs::read(&backup).unwrap();
    assert!(!String::from_utf8_lossy(&raw).contains("sk-ant-"));
    let plain = decrypt_file(&backup, &SecretString::from(BACKUP_PASS.to_owned())).unwrap();
    let items: Value = serde_json::from_slice(&plain).unwrap();
    assert_eq!(items[0]["service"], SERVICE);
    assert_eq!(items[0]["account"], "src");
    assert_eq!(items[0]["value"], fake_token());

    // An existing backup is never overwritten.
    let (out, _) = logout(&fx, &session, None);
    assert!(!out.status.success());
    assert!(backup.exists());
}

#[test]
fn logout_source_refuses_another_service() {
    let fx = setup();
    let (_, session) = export(&fx);
    let meta_path = fx
        .src
        .join(".vibe-transfer/outbox")
        .join(&session)
        .join("meta.json");
    let mut meta = read_json(&meta_path);
    assert_eq!(meta["secret_service"], SERVICE);
    meta["secret_service"] = format!("{SERVICE}-0badc0de").into();
    fs::write(&meta_path, meta.to_string()).unwrap();

    let (out, secrets) = logout(&fx, &session, None);
    assert!(!out.status.success());
    assert!(
        text(&out.stderr).contains("başka bir servis"),
        "{}",
        text(&out.stderr)
    );
    assert_eq!(secrets.len(), 1);
    assert_eq!(secrets[0]["blake3"], digest(&fake_token()));
    assert!(
        !fx.src
            .join(".vibe-transfer/backups")
            .join(format!("logout-{session}"))
            .exists()
    );
}

#[test]
fn logout_source_refuses_another_account() {
    let fx = setup();
    let (_, session) = export(&fx);
    // The user logged in with another account on the source since.
    let path = fx.src.join(".claude.json");
    let mut doc = read_json(&path);
    doc["oauthAccount"]["accountUuid"] = "uuid-other".into();
    fs::write(&path, doc.to_string()).unwrap();

    let (out, secrets) = logout(&fx, &session, None);
    assert!(!out.status.success());
    assert!(
        text(&out.stderr).contains("dışa aktarılan hesap"),
        "{}",
        text(&out.stderr)
    );
    assert_eq!(secrets.len(), 1);
    assert_eq!(secrets[0]["blake3"], digest(&fake_token()));
    assert!(
        !fx.src
            .join(".vibe-transfer/backups")
            .join(format!("logout-{session}"))
            .exists()
    );
}

fn store_with(token: &str) -> Value {
    json!([{"service": SERVICE, "account": "dst", "value": token}])
}

#[test]
fn rollback_on_an_empty_target_removes_the_secret_once() {
    let fx = setup();
    let (bundle, _) = export(&fx);
    // Nothing on the target is overwritten and there is no previous secret.
    fs::remove_dir_all(fx.dst.join("code")).unwrap();
    let session = import(&fx, &bundle, None);
    let yes = Some(json!({"rollback": true}));
    let (out, secrets) = dst_store(
        &fx,
        &["rollback", &session],
        yes.clone(),
        store_with(&fake_token()),
        "",
    );
    ok(&out);
    assert_eq!(dst_digest(&secrets), None, "{secrets:?}");
    let shown = text(&out.stdout);
    assert!(shown.contains("Geri alındı"), "{shown}");
    assert!(shown.contains("sır deposundan kaldırıldı"), "{shown}");

    // Logged in again since: a second rollback leaves the store alone.
    let fresh = format!("{}-relogin", fake_token());
    let (out, secrets) = dst_store(&fx, &["rollback", &session], yes, store_with(&fresh), "");
    ok(&out);
    assert_eq!(dst_digest(&secrets), Some(digest(&fresh)));
}

#[test]
fn rollback_undoes_the_secret_of_an_import_that_failed_after_writing_it() {
    let fx = setup();
    let (bundle, _) = export(&fx);
    // The report cannot be written: the import fails after the store write.
    write(&fx.dst_vt().join("reports"), "not a folder\n");
    let output = run(
        &fx.dst,
        "dst",
        &[
            "import",
            bundle.to_str().unwrap(),
            "--passphrase-stdin",
            "--json",
        ],
        &[],
        None,
        &format!("{PASS}\n"),
    );
    assert!(!output.status.success());
    let session = fs::read_dir(fx.dst_vt().join("journal"))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .find_map(|n| n.strip_suffix(".jsonl").map(str::to_owned))
        .unwrap();
    let (out, secrets) = dst_store(
        &fx,
        &["rollback", &session],
        Some(json!({"rollback": true})),
        store_with(&fake_token()),
        "",
    );
    ok(&out);
    assert_eq!(dst_digest(&secrets), None, "{secrets:?}");
}
