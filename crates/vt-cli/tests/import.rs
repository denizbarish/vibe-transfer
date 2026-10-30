//! `import` end to end: a bundle exported from one synthetic home (user
//! `src`) is imported into another (user `dst`) whose projects live at
//! different paths. Both homes are selected with `VT_TEST_HOME`; secrets
//! live in a `MemStore` filled from `VT_TEST_SECRETS` and are observed only
//! as BLAKE3 digests through `VT_TEST_SECRETS_DUMP`. Interactive answers
//! come from `VT_TEST_ANSWERS`.

// Fixtures use POSIX modes.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::{Duration, SystemTime};

use serde_json::{Value, json};
use vt_core::bundle::{SecretString, decrypt_file};
use vt_core::pathcodec::claude_slug;

const PASS: &str = "test-pass-import";
const SERVICE: &str = "Claude Code-credentials";
const APP_REMOTE: &str = "github.com/acme/app";
const LOST_REMOTE: &str = "github.com/acme/lost";

/// Synthetic token values, built at run time so no token-shaped literal
/// sits in the source.
fn fake_token(tag: &str) -> String {
    format!(
        "{{\"claudeAiOauth\":{{\"accessToken\":\"{}{}{tag}\"}}}}",
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
    fn dst_app(&self) -> PathBuf {
        self.dst.join("code/app")
    }
    fn src_lost(&self) -> PathBuf {
        self.src.join("work/lost")
    }
    fn slug_dir(&self, home: &Path, project: &Path) -> PathBuf {
        home.join(".claude/projects")
            .join(claude_slug(project.to_str().unwrap()))
    }
    fn dst_vt(&self) -> PathBuf {
        self.dst.join(".vibe-transfer")
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

fn set_age(path: &Path, days: u64) {
    let when = SystemTime::now() - Duration::from_secs(days * 86_400);
    fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(when)
        .unwrap();
}

/// Source home: one project the target has (at another path), one it does
/// not, Claude settings with home paths, a hook script, a session and
/// memory, and the Keychain item.
fn setup() -> Fx {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    let dst = tmp.path().join("dst");
    let fx = Fx { src, dst, tmp };
    let s = fx.src.to_str().unwrap().to_owned();
    let claude = fx.src.join(".claude");

    git_repo(&fx.src_app(), "https://github.com/acme/app.git");
    write(&fx.src_app().join("CLAUDE.md"), "# app\n");
    write(
        &fx.src_app().join(".mcp.json"),
        &json!({"mcpServers": {"tool": {"command": format!("{s}/bin/tool")}}}).to_string(),
    );
    git_repo(&fx.src_lost(), "https://github.com/acme/lost.git");
    write(&fx.src_lost().join("CLAUDE.md"), "# lost\n");

    write(&claude.join("CLAUDE.md"), "# global\n");
    // Machine-specific: the export skips it and says so in the manifest.
    write(&claude.join("sessions/1.json"), "{}\n");
    write(&claude.join("agents/reviewer.md"), "reviewer\n");
    write(
        &claude.join("settings.json"),
        &json!({"hooks": {"Stop": [{"hooks": [
            {"type": "command", "command": format!("{s}/.claude/hooks/done.sh")}
        ]}]}})
        .to_string(),
    );
    let hook = claude.join("hooks/done.sh");
    write(
        &hook,
        &format!("#!/bin/sh\necho done >> {s}/logs/done.txt\n"),
    );
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();

    let app_slug = fx.slug_dir(&fx.src, &fx.src_app());
    let session = app_slug.join("sess-app.jsonl");
    write(
        &session,
        &format!(
            "{}\n",
            json!({"type": "user", "cwd": fx.src_app(), "sessionId": "sess-app"})
        ),
    );
    set_age(&session, 90);
    write(&app_slug.join("memory/MEMORY.md"), "- source line\n");
    let lost_slug = fx.slug_dir(&fx.src, &fx.src_lost());
    write(
        &lost_slug.join("sess-lost.jsonl"),
        &format!(
            "{}\n",
            json!({"type": "user", "cwd": fx.src_lost(), "sessionId": "sess-lost"})
        ),
    );

    write(
        &fx.src.join(".claude.json"),
        &json!({
            "userID": "src-user-id",
            "projects": {
                fx.src_app().to_str().unwrap(): {"allowedTools": ["Bash"]},
                fx.src_lost().to_str().unwrap(): {"allowedTools": []},
            },
            "oauthAccount": {"accountUuid": "uuid-src", "emailAddress": "src@example.test"},
        })
        .to_string(),
    );
    write(
        &fx.tmp.path().join("src-secrets.json"),
        &json!([{"service": SERVICE, "account": "src", "value": fake_token("src")}]).to_string(),
    );

    git_repo(&fx.dst_app(), "git@github.com:acme/app.git");
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
) -> Output {
    let mut cmd = assert_cmd::cargo::cargo_bin_cmd!("vibe-transfer");
    cmd.args(args)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home)
        .env("USER", user)
        .env("VT_TEST_HOME", home)
        .write_stdin(format!("{PASS}\n"));
    for (k, v) in envs {
        cmd.env(k, v);
    }
    if let Some(a) = answers {
        cmd.env("VT_TEST_ANSWERS", a.to_string());
    }
    cmd.output().unwrap()
}

fn export(fx: &Fx) -> PathBuf {
    let out = fx.tmp.path().join("bundle.vtb");
    let secrets = fx.tmp.path().join("src-secrets.json");
    let output = run(
        &fx.src,
        "src",
        &[
            "export",
            "--out",
            out.to_str().unwrap(),
            "--passphrase-stdin",
        ],
        &[("VT_TEST_SECRETS", &secrets)],
        None,
    );
    assert!(output.status.success(), "{}", text(&output.stderr));
    out
}

struct Imported {
    output: Output,
    summary: Value,
    secrets: Vec<Value>,
}

/// Imports into `dst`; `target_secrets` fills the target store first.
fn import(
    fx: &Fx,
    bundle: &Path,
    answers: Option<Value>,
    target_secrets: Option<Value>,
) -> Imported {
    let dump = fx.tmp.path().join("dump.json");
    let _ = fs::remove_file(&dump);
    let secrets_file = fx.tmp.path().join("dst-secrets.json");
    let mut envs: Vec<(&str, &Path)> = vec![("VT_TEST_SECRETS_DUMP", &dump)];
    if let Some(items) = target_secrets {
        fs::write(&secrets_file, items.to_string()).unwrap();
        envs.push(("VT_TEST_SECRETS", &secrets_file));
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
    );
    let summary = serde_json::from_slice(&output.stdout).unwrap_or(Value::Null);
    let secrets = fs::read(&dump)
        .ok()
        .map(|b| serde_json::from_slice::<Vec<Value>>(&b).unwrap())
        .unwrap_or_default();
    Imported {
        output,
        summary,
        secrets,
    }
}

fn ok(i: &Imported) {
    assert!(
        i.output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        text(&i.output.stdout),
        text(&i.output.stderr)
    );
}

fn digest(value: &str) -> String {
    blake3::hash(value.as_bytes()).to_hex().to_string()
}

fn secret_digest(i: &Imported, account: &str) -> Option<String> {
    i.secrets
        .iter()
        .find(|s| s["service"] == SERVICE && s["account"] == account)
        .map(|s| s["blake3"].as_str().unwrap().to_owned())
}

fn report(fx: &Fx, i: &Imported) -> Value {
    let session = i.summary["session_id"].as_str().unwrap();
    let path = fx.dst_vt().join("reports").join(format!("{session}.json"));
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

fn read_json(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

fn no_leaks(fx: &Fx, i: &Imported, values: &[String]) {
    let rep = fs::read_to_string(fx.dst_vt().join("reports").join(format!(
        "{}.json",
        i.summary["session_id"].as_str().unwrap()
    )))
    .unwrap_or_default();
    for shown in [text(&i.output.stdout), text(&i.output.stderr), rep] {
        for v in values {
            assert!(!shown.contains(v.as_str()), "secret leaked:\n{shown}");
        }
        assert!(!shown.contains(PASS), "passphrase leaked:\n{shown}");
    }
}

fn staging_empty(fx: &Fx) {
    let staging = fx.dst_vt().join("staging");
    let left: Vec<_> = fs::read_dir(&staging)
        .map(|r| r.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    assert!(left.is_empty(), "staging left behind: {left:?}");
}

#[test]
fn import_into_empty_target_full_roundtrip() {
    let fx = setup();
    let bundle = export(&fx);
    let i = import(&fx, &bundle, None, None);
    ok(&i);
    let d = fx.dst.to_str().unwrap();
    let s = fx.src.to_str().unwrap();

    // Sessions and memory land in the slug of the target project path.
    let src_slug = fx.slug_dir(&fx.src, &fx.src_app());
    let dst_slug = fx.slug_dir(&fx.dst, &fx.dst_app());
    assert_eq!(
        fs::read(dst_slug.join("sess-app.jsonl")).unwrap(),
        fs::read(src_slug.join("sess-app.jsonl")).unwrap()
    );
    assert_eq!(
        fs::read_to_string(dst_slug.join("memory/MEMORY.md")).unwrap(),
        "- source line\n"
    );
    // mtime survives (Claude's cleanup looks at it).
    let age = SystemTime::now()
        .duration_since(
            fs::metadata(dst_slug.join("sess-app.jsonl"))
                .unwrap()
                .modified()
                .unwrap(),
        )
        .unwrap();
    assert!(age > Duration::from_secs(80 * 86_400), "{age:?}");

    // ~/.claude.json: project keys use target paths, identity stays out.
    let cj = read_json(&fx.dst.join(".claude.json"));
    let keys: Vec<&String> = cj["projects"].as_object().unwrap().keys().collect();
    assert_eq!(
        keys,
        vec![&fx.dst_app().to_str().unwrap().to_owned()],
        "{cj}"
    );
    assert_eq!(cj["oauthAccount"]["accountUuid"], "uuid-src");
    assert!(cj.get("userID").is_none(), "{cj}");

    // Project files under the target project, paths rewritten.
    assert_eq!(
        fs::read_to_string(fx.dst_app().join("CLAUDE.md")).unwrap(),
        "# app\n"
    );
    let mcp = read_json(&fx.dst_app().join(".mcp.json"));
    assert_eq!(
        mcp["mcpServers"]["tool"]["command"],
        format!("{d}/bin/tool")
    );

    // Settings and hook script point at the target home.
    let settings = read_json(&fx.dst.join(".claude/settings.json"));
    assert_eq!(
        settings["hooks"]["Stop"][0]["hooks"][0]["command"],
        format!("{d}/.claude/hooks/done.sh")
    );
    let hook = fx.dst.join(".claude/hooks/done.sh");
    let hook_text = fs::read_to_string(&hook).unwrap();
    assert!(
        hook_text.contains(&format!("{d}/logs/done.txt")),
        "{hook_text}"
    );
    assert!(!hook_text.contains(s), "{hook_text}");
    assert_eq!(
        fs::metadata(&hook).unwrap().permissions().mode() & 0o777,
        0o755
    );

    // The secret is in the target store under the target user.
    assert_eq!(secret_digest(&i, "dst"), Some(digest(&fake_token("src"))));
    assert_eq!(secret_digest(&i, "src"), None);

    // Report: everything verified, transforms recorded, nothing wrong.
    let rep = report(&fx, &i);
    assert_eq!(rep["problems"], json!([]), "{rep}");
    assert!(rep["expected"].as_u64().unwrap() > 0);
    assert_eq!(rep["expected"], rep["verified"], "{rep}");
    let kinds: Vec<&str> = rep["transformed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["kind"].as_str().unwrap())
        .collect();
    assert!(kinds.contains(&"home_paths"), "{kinds:?}");
    assert!(kinds.contains(&"json_paths"), "{kinds:?}");
    assert!(kinds.contains(&"claude_json_merge"), "{kinds:?}");
    assert_eq!(rep["pending"], json!([LOST_REMOTE]), "{rep}");

    // Export notes reach the summary; the logout hint names the export session.
    let stdout = text(&i.output.stdout);
    assert!(stdout.contains("logout-source --session"), "{stdout}");
    assert!(!i.summary["source_notes"].as_array().unwrap().is_empty());
    assert!(
        rep["skipped"].to_string().contains("kaynak:sessions"),
        "{rep}"
    );
    staging_empty(&fx);
    no_leaks(&fx, &i, &[fake_token("src")]);
}

#[test]
fn import_merge_keeps_existing_target_sessions() {
    let fx = setup();
    let bundle = export(&fx);
    let dst_slug = fx.slug_dir(&fx.dst, &fx.dst_app());
    write(&dst_slug.join("old.jsonl"), "{\"type\":\"user\"}\n");
    write(&dst_slug.join("memory/MEMORY.md"), "- target line\n");
    // Same account on the target, with an older token.
    write(
        &fx.dst.join(".claude.json"),
        &json!({"userID": "dst-user-id", "oauthAccount": {"accountUuid": "uuid-src"}}).to_string(),
    );
    let old = fake_token("old");
    let i = import(
        &fx,
        &bundle,
        Some(json!({"decision": "merge"})),
        Some(json!([{"service": SERVICE, "account": "dst", "value": old}])),
    );
    ok(&i);

    assert_eq!(
        fs::read_to_string(dst_slug.join("old.jsonl")).unwrap(),
        "{\"type\":\"user\"}\n"
    );
    assert!(dst_slug.join("sess-app.jsonl").exists());
    let memory = fs::read_to_string(dst_slug.join("memory/MEMORY.md")).unwrap();
    assert_eq!(memory, "- target line\n- source line\n");
    let cj = read_json(&fx.dst.join(".claude.json"));
    assert_eq!(cj["userID"], "dst-user-id");
    assert!(
        cj["projects"].get(fx.dst_app().to_str().unwrap()).is_some(),
        "{cj}"
    );

    // The token was replaced; the old one is in the encrypted backup.
    assert_eq!(secret_digest(&i, "dst"), Some(digest(&fake_token("src"))));
    let session = i.summary["session_id"].as_str().unwrap();
    let backup = fx
        .dst_vt()
        .join("backups")
        .join(session)
        .join("secrets.age");
    assert_eq!(
        fs::metadata(&backup).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let raw = fs::read(&backup).unwrap();
    assert!(!String::from_utf8_lossy(&raw).contains(&old));
    let plain = decrypt_file(&backup, &SecretString::from(PASS.to_owned())).unwrap();
    assert!(String::from_utf8_lossy(&plain).contains("oat01-synthetic0old"));

    let rep = report(&fx, &i);
    assert_eq!(rep["problems"], json!([]), "{rep}");
    assert_eq!(rep["expected"], rep["verified"], "{rep}");
    staging_empty(&fx);
    no_leaks(&fx, &i, &[fake_token("src"), old]);
}

#[test]
fn import_replace_backs_up_target_only_files() {
    let fx = setup();
    let bundle = export(&fx);
    let dst_slug = fx.slug_dir(&fx.dst, &fx.dst_app());
    let old_session = dst_slug.join("old.jsonl");
    write(&old_session, "{\"type\":\"user\"}\n");
    let only_target = fx.dst.join(".claude/agents/target-only.md");
    write(&only_target, "mine\n");
    // Machine-specific data outside the transferred categories stays.
    let machine = fx.dst.join(".claude/sessions/1.json");
    write(&machine, "{}\n");

    let i = import(&fx, &bundle, Some(json!({"decision": "replace"})), None);
    ok(&i);
    let session = i.summary["session_id"].as_str().unwrap();
    let abs = fx.dst_vt().join("backups").join(session).join("abs");
    let mirror = |p: &Path| abs.join(p.strip_prefix("/").unwrap());

    assert!(!old_session.exists());
    assert_eq!(
        fs::read_to_string(mirror(&old_session)).unwrap(),
        "{\"type\":\"user\"}\n"
    );
    assert!(!only_target.exists());
    assert_eq!(fs::read_to_string(mirror(&only_target)).unwrap(), "mine\n");
    assert!(machine.exists());
    assert!(dst_slug.join("sess-app.jsonl").exists());
    assert!(fx.dst.join(".claude/agents/reviewer.md").exists());
    let rep = report(&fx, &i);
    assert_eq!(rep["problems"], json!([]), "{rep}");
    staging_empty(&fx);
}

#[test]
fn import_prompts_on_different_target_account() {
    let fx = setup();
    let bundle = export(&fx);
    let account = json!({"accountUuid": "uuid-dst", "emailAddress": "dst@example.test"});
    write(
        &fx.dst.join(".claude.json"),
        &json!({"oauthAccount": account}).to_string(),
    );
    let mine = fake_token("dst");
    let target_secret = json!([{"service": SERVICE, "account": "dst", "value": mine}]);

    // No terminal and no answers: the question is not guessed.
    let refused = import(&fx, &bundle, None, Some(target_secret.clone()));
    assert!(!refused.output.status.success());
    let stderr = text(&refused.output.stderr);
    assert!(stderr.contains("terminal"), "{stderr}");
    assert!(!fx.dst.join(".claude/CLAUDE.md").exists());
    assert_eq!(
        read_json(&fx.dst.join(".claude.json"))["oauthAccount"],
        account
    );
    staging_empty(&fx);

    let i = import(
        &fx,
        &bundle,
        Some(json!({"decision": "merge", "account": "keep_target"})),
        Some(target_secret),
    );
    ok(&i);
    assert_eq!(secret_digest(&i, "dst"), Some(digest(&mine)));
    let cj = read_json(&fx.dst.join(".claude.json"));
    assert_eq!(cj["oauthAccount"], account);
    assert!(
        cj["projects"].get(fx.dst_app().to_str().unwrap()).is_some(),
        "{cj}"
    );
    assert!(fx.dst.join(".claude/CLAUDE.md").exists());
    let stdout = text(&i.output.stdout);
    assert!(!stdout.contains("logout-source"), "{stdout}");
    let rep = report(&fx, &i);
    assert_eq!(rep["problems"], json!([]), "{rep}");
    let skipped = rep["skipped"].to_string();
    assert!(skipped.contains("hedef hesabı korundu"), "{skipped}");
    no_leaks(&fx, &i, &[fake_token("src"), mine]);
}

#[test]
fn import_unmatched_project_goes_pending() {
    let fx = setup();
    let bundle = export(&fx);
    let i = import(&fx, &bundle, None, None);
    ok(&i);

    // Nothing is created for the project the target does not have.
    let lost_slug_src = fx.slug_dir(&fx.dst, &fx.src_lost());
    assert!(!lost_slug_src.exists());
    assert!(!fx.dst.join("work").exists());
    let cj = read_json(&fx.dst.join(".claude.json"));
    assert!(
        cj["projects"]
            .get(fx.src_lost().to_str().unwrap())
            .is_none(),
        "{cj}"
    );

    // Its data waits in the pending store.
    let pending = fx.dst_vt().join("pending");
    let records: Vec<Value> = fs::read_dir(&pending)
        .unwrap()
        .flatten()
        .map(|e| read_json(&e.path().join("pending.json")))
        .collect();
    assert_eq!(records.len(), 1, "{records:?}");
    let rec = &records[0];
    assert_eq!(rec["project"]["id"], LOST_REMOTE);
    let templates = rec["items"].to_string();
    assert!(templates.contains("sess-lost.jsonl"), "{templates}");
    assert!(
        templates.contains("claude_json_projects_key"),
        "{templates}"
    );
    assert!(templates.contains("CLAUDE.md"), "{templates}");

    let rep = report(&fx, &i);
    assert_eq!(rep["pending"], json!([LOST_REMOTE]));
    let stdout = text(&i.output.stdout);
    assert!(stdout.contains(LOST_REMOTE), "{stdout}");
    assert!(i.summary["mapped"].to_string().contains(APP_REMOTE));
}

#[test]
fn import_warns_cleanup_period() {
    let fx = setup();
    let bundle = export(&fx);
    let i = import(&fx, &bundle, Some(json!({"cleanup": false})), None);
    ok(&i);
    let stderr = text(&i.output.stderr);
    assert!(stderr.contains("cleanupPeriodDays"), "{stderr}");
    let suggested = i.summary["cleanup_suggestion"].as_u64().unwrap();
    assert!(suggested >= 90, "{suggested}");
    let settings = read_json(&fx.dst.join(".claude/settings.json"));
    assert!(settings.get("cleanupPeriodDays").is_none(), "{settings}");

    // With consent the value is written.
    let fx = setup();
    let bundle = export(&fx);
    let i = import(&fx, &bundle, Some(json!({"cleanup": true})), None);
    ok(&i);
    let settings = read_json(&fx.dst.join(".claude/settings.json"));
    assert!(
        settings["cleanupPeriodDays"].as_u64().unwrap() >= 90,
        "{settings}"
    );
    let rep = report(&fx, &i);
    assert_eq!(rep["problems"], json!([]), "{rep}");
}

#[test]
fn import_asks_before_mapping_a_project_onto_the_target_home() {
    let fx = setup();
    // A session opened in the source home: its project is the home itself.
    let home_session = fx.slug_dir(&fx.src, &fx.src).join("sess-home.jsonl");
    write(
        &home_session,
        &format!(
            "{}\n",
            json!({"type": "user", "cwd": fx.src, "sessionId": "sess-home"})
        ),
    );
    let bundle = export(&fx);
    let id = format!("path:{}", fx.src.display());

    let refused = import(&fx, &bundle, None, None);
    assert!(!refused.output.status.success());
    let stderr = text(&refused.output.stderr);
    assert!(stderr.contains("terminal"), "{stderr}");
    assert!(!fx.dst.join(".claude").exists());

    let i = import(
        &fx,
        &bundle,
        Some(json!({"choose": {id: fx.dst.to_str().unwrap()}})),
        None,
    );
    ok(&i);
    assert!(
        fx.slug_dir(&fx.dst, &fx.dst)
            .join("sess-home.jsonl")
            .exists()
    );
}

#[test]
fn import_reports_apply_conflicts() {
    let fx = setup();
    let bundle = export(&fx);
    // The target's agents folder is a symlink (user setup): a replace
    // leaves it alone and must say so.
    let real = fx.dst.join("dotfiles/agents");
    write(&real.join("mine.md"), "mine\n");
    fs::create_dir_all(fx.dst.join(".claude")).unwrap();
    std::os::unix::fs::symlink(&real, fx.dst.join(".claude/agents")).unwrap();

    let i = import(&fx, &bundle, Some(json!({"decision": "replace"})), None);
    ok(&i);
    let rep = report(&fx, &i);
    assert!(rep["conflicts"].to_string().contains("symlink"), "{rep}");
    assert!(
        i.summary["conflicts"].to_string().contains("symlink"),
        "{}",
        i.summary
    );
    assert!(real.join("mine.md").exists());
}

#[test]
fn import_never_prints_bundle_control_sequences_raw() {
    let fx = setup();
    // An unclassified file whose name carries ESC/OSC/BEL: the export skips
    // it and quotes the name in the manifest notes.
    write(
        &fx.src.join(".claude/evil\u{1b}]0;pwned\u{7}\u{1b}[31m.txt"),
        "x\n",
    );
    let bundle = export(&fx);

    let output = run(
        &fx.dst,
        "dst",
        &["import", bundle.to_str().unwrap(), "--passphrase-stdin"],
        &[],
        None,
    );
    let (stdout, stderr) = (text(&output.stdout), text(&output.stderr));
    assert!(output.status.success(), "{stdout}\n{stderr}");
    for shown in [&stdout, &stderr] {
        assert!(
            !shown.contains('\u{1b}') && !shown.contains('\u{7}'),
            "{shown:?}"
        );
    }
    assert!(stdout.contains("evil\\u{1b}]0;pwned\\u{7}"), "{stdout}");

    // `--json` stays a faithful JSON encoding (escaped by serde).
    let fx2 = setup();
    write(&fx2.src.join(".claude/evil\u{1b}[31m.txt"), "x\n");
    let bundle = export(&fx2);
    let i = import(&fx2, &bundle, None, None);
    ok(&i);
    assert!(!text(&i.output.stdout).contains('\u{1b}'));
    assert!(
        i.summary["source_notes"]
            .to_string()
            .contains("evil\\u001b[31m.txt"),
        "{}",
        i.summary
    );
}

#[test]
fn import_kept_account_without_target_item_moves_mcp_oauth() {
    let fx = setup();
    let mcp = json!({"srv": {"accessToken": format!("{}{}", "synthetic-", "mcp-value")}});
    let mut value: Value = serde_json::from_str(&fake_token("src")).unwrap();
    value["mcpOAuth"] = mcp.clone();
    write(
        &fx.tmp.path().join("src-secrets.json"),
        &json!([{"service": SERVICE, "account": "src", "value": value.to_string()}]).to_string(),
    );
    let bundle = export(&fx);
    write(
        &fx.dst.join(".claude.json"),
        &json!({"oauthAccount": {"accountUuid": "uuid-dst"}}).to_string(),
    );

    let i = import(
        &fx,
        &bundle,
        Some(json!({"decision": "merge", "account": "keep_target"})),
        None,
    );
    ok(&i);
    let expected = json!({"mcpOAuth": mcp}).to_string();
    assert_eq!(secret_digest(&i, "dst"), Some(digest(&expected)));
    assert_eq!(
        read_json(&fx.dst.join(".claude.json"))["oauthAccount"]["accountUuid"],
        "uuid-dst"
    );
    assert!(!text(&i.output.stdout).contains("logout-source"));
    no_leaks(&fx, &i, &[fake_token("src"), "synthetic-mcp-value".into()]);
}
