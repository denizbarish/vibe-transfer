//! `vibe-transfer import`: applies a bundle to this machine. Staging under
//! `<vt_home>/staging/<session>/` is removed on every exit path; the report
//! goes to `<vt_home>/reports/<session>.json`.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde_json::{Map, Value, json};
use vt_claude::{ClaudePlan, PlanInput, build_claude_apply_plan, read_claude_json_stable};
use vt_core::apply::{self, ApplyPlan, ApplyReport, Decision, Resolution};
use vt_core::bundle::{ReadBundle, SecretString, encrypt_to_file, read_bundle};
use vt_core::fsutil::create_private_dir_all;
use vt_core::manifest::ProjectId;
use vt_core::pathmap::{Mapping, build_target_index, map_project};
use vt_core::report::{
    SecretWrite, TransformRecord, VerifyReport, hash_bytes, json_bytes, verify_plan, write_report,
    write_secret_write,
};
use vt_core::secret_types::{SecretBytes, SecretItem, SecretKind};
use vt_core::session::new_session_id;
use vt_secrets::claude_service_name;

use super::{Ctx, Precomputed, Questions, Staging, needs_confirmation, pending_store};
use crate::ImportArgs;
use crate::ui::{self, Prompter};

/// Folder levels below each root searched for target repositories.
const INDEX_DEPTH: usize = 6;
/// Claude Code's default `cleanupPeriodDays`.
const CLAUDE_DEFAULT_CLEANUP_DAYS: u64 = 30;
/// Extra days on top of the oldest session's age in the suggestion.
const CLEANUP_MARGIN_DAYS: u64 = 30;
/// Login part of the Claude Keychain value; the rest (MCP OAuth and the
/// like) moves even when the target keeps its own account.
const LOGIN_KEY: &str = "claudeAiOauth";
const KEPT_SECRET_KEY: &str = "hedefteki farklı değer korundu (giriş dışı sır anahtarı)";
const KEPT_ACCOUNT: &str = "hedef hesabı korundu; giriş token'ı aktarılmadı";

#[derive(Serialize)]
struct Mapped {
    project: String,
    path: String,
}

#[derive(Serialize)]
struct Item {
    path: String,
    reason: String,
}

#[derive(Serialize)]
struct Summary {
    test_mode: bool,
    session_id: String,
    source_session: String,
    report_path: String,
    mapped: Vec<Mapped>,
    pending: Vec<String>,
    placed: u32,
    skipped_identical: u32,
    backed_up: usize,
    kept_both: usize,
    expected: u32,
    verified: u32,
    transformed: usize,
    skipped: Vec<Item>,
    problems: Vec<String>,
    conflicts: Vec<String>,
    source_notes: Vec<String>,
    cleanup_suggestion: Option<u64>,
    logout_command: Option<String>,
}

pub fn run(ctx: &Ctx, args: &ImportArgs) -> Result<()> {
    let pass = if args.passphrase_stdin {
        ui::read_passphrase_stdin(false)?
    } else {
        ui::prompt_passphrase()?
    };
    if let Some(notice) = ctx.test_mode_notice() {
        eprintln!("{notice}");
    }
    let env = &ctx.env;
    let session = new_session_id();
    let staging_root = env.vt_home.join("staging");
    create_private_dir_all(&staging_root)
        .with_context(|| format!("oluşturulamadı: {}", staging_root.display()))?;
    let staging = staging_root.join(&session);
    create_private_dir_all(&staging)
        .with_context(|| format!("oluşturulamadı: {}", staging.display()))?;
    let _staging = Staging(staging.clone());

    let rb = read_bundle(&args.file, &pass, &staging)?;
    let mut prompter = Prompter::new(ctx.answers.as_ref());

    // Every question is asked before anything on the target changes.
    let mappings = map_projects(ctx, args, &rb, &mut prompter)?;
    let target_json = read_target_json(&env.claude_json)?;
    let decision = if target_has_claude_data(ctx) {
        prompter.claude_decision()?
    } else {
        Decision::Merge
    };
    let secret = rb
        .secrets
        .iter()
        .find(|s| s.meta.kind == SecretKind::ClaudeKeychain);
    let take_oauth = match (secret, account_uuid(&target_json)) {
        (None, _) => false,
        (Some(_), None) => true,
        (Some(s), Some(target)) if s.meta.account_uuid.as_deref() == Some(target.as_str()) => true,
        (Some(s), Some(target)) => {
            prompter.take_source_account(&target, s.meta.account_uuid.as_deref())?
        }
    };
    ui::wait_until_closed("Claude", "Aktarımdan önce", &|| ctx.claude_pids())?;

    let work_dir = staging.join("xform");
    create_private_dir_all(&work_dir)?;
    let input = PlanInput {
        mappings: &mappings,
        decision,
        take_oauth,
        session_id: &session,
        work_dir: &work_dir,
    };
    let plan = build_claude_apply_plan(&rb, env, &input, &mut Questions(&mut prompter))?;

    let sessions_before = count_sessions(&env.claude_dir);
    let pending_ids = put_pending(&env.vt_home, &plan)?;
    let mut resolver = Precomputed(&plan.resolutions);
    let applied = match apply::apply(&plan.plan, &env.vt_home, &mut resolver) {
        Ok(applied) => applied,
        Err(e) => {
            let store = pending_store(&env.vt_home);
            for id in &pending_ids.created {
                let _ = store.remove(id);
            }
            return Err(e);
        }
    };

    let mut report = VerifyReport {
        session_id: session.clone(),
        transformed: plan.transformed.clone(),
        pending: pending_ids.all.clone(),
        skipped: plan.skipped.clone(),
        problems: plan.problems.clone(),
        ..VerifyReport::default()
    };
    report.skipped.extend(source_skips(&rb));
    let token_moved = match secret {
        Some(item) => import_secret(ctx, item, take_oauth, &pass, &session, &mut report)
            .unwrap_or_else(|e| {
                report.problems.push(format!("sır yazılamadı: {e:#}"));
                false
            }),
        None => false,
    };

    let kept_target: BTreeSet<PathBuf> = plan
        .resolutions
        .iter()
        .filter(|(_, r)| **r == Resolution::KeepTarget)
        .map(|(d, _)| d.clone())
        .collect();
    report.conflicts.extend(applied.conflicts.iter().cloned());
    verify_plan(&plan.plan, &applied, &kept_target, &mut report);
    check_sessions(
        &plan,
        decision,
        sessions_before,
        &env.claude_dir,
        &mut report,
    );

    let cleanup_suggestion = cleanup_period(ctx, &plan, &session, &mut prompter, &mut report)?;
    let report_path = write_report(&env.vt_home, &report)?;

    let summary = Summary {
        test_mode: ctx.test_mode,
        session_id: session.clone(),
        source_session: rb.manifest.session_id.clone(),
        report_path: report_path.display().to_string(),
        mapped: mappings
            .iter()
            .filter_map(|(id, m)| match m {
                Mapping::Auto(p) => Some(Mapped {
                    project: id.0.clone(),
                    path: p.display().to_string(),
                }),
                _ => None,
            })
            .collect(),
        pending: report.pending.iter().map(|p| p.0.clone()).collect(),
        placed: applied.placed,
        skipped_identical: applied.skipped_identical,
        backed_up: applied.backed_up.len(),
        kept_both: applied.kept_both.len(),
        expected: report.expected,
        verified: report.verified,
        transformed: report.transformed.len(),
        skipped: report
            .skipped
            .iter()
            .map(|(path, reason)| Item {
                path: path.clone(),
                reason: reason.clone(),
            })
            .collect(),
        problems: report.problems.clone(),
        conflicts: report.conflicts.clone(),
        source_notes: rb.manifest.notes.clone(),
        cleanup_suggestion,
        logout_command: token_moved.then(|| {
            format!(
                "vibe-transfer logout-source --session {}",
                rb.manifest.session_id
            )
        }),
    };
    if args.json {
        println!("{}", serde_json::to_string_pretty(&summary)?);
    } else {
        print_human(&summary, &report.skipped);
    }
    if !report.problems.is_empty() {
        bail!(
            "aktarım tamamlandı ama doğrulamada {} sorun var; rapor: {}",
            report.problems.len(),
            report_path.display()
        );
    }
    Ok(())
}

fn print_human(s: &Summary, skipped: &[(String, String)]) {
    // Bundle-supplied text (ids, paths, notes) is shown through `ui::clean`.
    let c = ui::clean;
    println!("İçe aktarıldı. Oturum: {}", s.session_id);
    println!(
        "Yerleştirilen: {}, zaten aynı: {}, yedeklenen: {}, yan yana tutulan: {}",
        s.placed, s.skipped_identical, s.backed_up, s.kept_both
    );
    println!(
        "Doğrulanan: {}/{}; dönüştürülen dosya: {}",
        s.verified, s.expected, s.transformed
    );
    for m in &s.mapped {
        println!("Eşlendi: {} -> {}", c(&m.project), c(&m.path));
    }
    if !s.pending.is_empty() {
        println!("Bekleme alanında: {}", s.pending.len());
        for id in &s.pending {
            println!("    {}", c(id));
        }
    }
    ui::print_grouped("Atlananlar", skipped);
    if !s.conflicts.is_empty() {
        println!("Çakışmalar: {}", s.conflicts.len());
        for conflict in &s.conflicts {
            println!("    {}", c(conflict));
        }
    }
    if !s.source_notes.is_empty() {
        println!("Kaynaktaki dışa aktarma notları: {}", s.source_notes.len());
        for note in &s.source_notes {
            println!("    {}", c(note));
        }
    }
    for p in &s.problems {
        println!("SORUN: {}", c(p));
    }
    println!("Rapor: {}", c(&s.report_path));
    if let Some(cmd) = &s.logout_command {
        println!(
            "Giriş token'ı taşındı. Hedefte çalıştığını doğruladıktan sonra kaynakta çalıştırın:"
        );
        println!("    {}", c(cmd));
    }
}

/// Target path per manifest project; several candidates are asked. A
/// project that would map onto the target home or `/` (from a path the
/// bundle supplied) is asked too, never taken automatically.
fn map_projects(
    ctx: &Ctx,
    args: &ImportArgs,
    rb: &ReadBundle,
    prompter: &mut Prompter,
) -> Result<BTreeMap<ProjectId, Mapping>> {
    let roots = if args.roots.is_empty() {
        vec![ctx.env.home.clone()]
    } else {
        args.roots.clone()
    };
    let idx = build_target_index(&roots, INDEX_DEPTH);
    let src_home = &rb.manifest.source.home;
    let mut out = BTreeMap::new();
    for p in &rb.manifest.projects {
        let mapping = match map_project(p, &idx, src_home, &ctx.env.home) {
            Mapping::Auto(path) if needs_confirmation(&path, &ctx.env) => {
                match prompter.choose_path(p, std::slice::from_ref(&path))? {
                    Some(path) => Mapping::Auto(path),
                    None => Mapping::Pending,
                }
            }
            Mapping::Choose(candidates) => match prompter.choose_path(p, &candidates)? {
                Some(path) => Mapping::Auto(path),
                None => Mapping::Pending,
            },
            other => other,
        };
        out.insert(p.id.clone(), mapping);
    }
    Ok(out)
}

fn read_target_json(path: &Path) -> Result<Value> {
    if fs::symlink_metadata(path).is_err() {
        return Ok(Value::Null);
    }
    read_claude_json_stable(path)
}

fn account_uuid(claude_json: &Value) -> Option<String> {
    claude_json
        .get("oauthAccount")?
        .get("accountUuid")?
        .as_str()
        .map(str::to_owned)
}

fn target_has_claude_data(ctx: &Ctx) -> bool {
    let dir_used = fs::read_dir(&ctx.env.claude_dir).is_ok_and(|mut d| d.next().is_some());
    dir_used || fs::symlink_metadata(&ctx.env.claude_json).is_ok()
}

struct PendingIds {
    all: Vec<ProjectId>,
    /// Projects that had no pending data before this import.
    created: Vec<ProjectId>,
}

/// Stores the pending data before the apply, so a failed store write
/// leaves the target untouched. Every pending project is listed, with or
/// without data to store.
fn put_pending(vt_home: &Path, plan: &ClaudePlan) -> Result<PendingIds> {
    let store = pending_store(vt_home);
    let mut ids = PendingIds {
        all: Vec::new(),
        created: Vec::new(),
    };
    if plan.pending.is_empty() {
        return Ok(ids);
    }
    let existing: BTreeSet<ProjectId> = store
        .list()?
        .items
        .into_iter()
        .map(|p| p.project.id)
        .collect();
    for (project, items) in &plan.pending {
        if let Err(e) = store.put(project, items) {
            for id in &ids.created {
                let _ = store.remove(id);
            }
            return Err(e.context(format!("bekleme alanına yazılamadı: {}", project.id.0)));
        }
        if !existing.contains(&project.id) {
            ids.created.push(project.id.clone());
        }
        ids.all.push(project.id.clone());
    }
    Ok(ids)
}

/// Export-side skips carried in the manifest notes.
fn source_skips(rb: &ReadBundle) -> Vec<(String, String)> {
    rb.manifest
        .notes
        .iter()
        .filter_map(|n| n.strip_prefix("atlandı: "))
        .map(|rest| match rest.split_once(": ") {
            Some((key, reason)) => (format!("kaynak:{key}"), reason.to_owned()),
            None => (format!("kaynak:{rest}"), "kaynakta atlandı".to_owned()),
        })
        .collect()
}

/// Writes the Claude Keychain item to the target store. A previous target
/// value is first saved, age-encrypted with the bundle passphrase, to
/// `<vt_home>/backups/<session>/secrets.age`. Returns whether the login
/// token moved.
fn import_secret(
    ctx: &Ctx,
    item: &SecretItem,
    take_oauth: bool,
    pass: &SecretString,
    session: &str,
    report: &mut VerifyReport,
) -> Result<bool> {
    let service = claude_service_name(ctx.env.claude_config_dir_override.as_deref());
    let account = ctx.env.user.clone();
    let key = format!("keychain:{service}");
    let previous = ctx.store.get(&service, &account)?;
    let new_value = if take_oauth {
        item.value.clone()
    } else {
        report.skipped.push((key.clone(), KEPT_ACCOUNT.to_owned()));
        let extras = non_login_extras(previous.as_ref(), &item.value);
        for k in &extras.kept {
            report
                .skipped
                .push((format!("{key}#{k}"), KEPT_SECRET_KEY.to_owned()));
        }
        if let Some(reason) = extras.unusable {
            report.skipped.push((key.clone(), reason.to_owned()));
        }
        match extras.value {
            Some(merged) => merged,
            None => return Ok(false),
        }
    };
    if previous.as_ref().map(SecretBytes::expose) == Some(new_value.expose()) {
        return Ok(take_oauth);
    }
    if let Some(prev) = &previous {
        backup_secret(&ctx.env.vt_home, session, pass, &service, &account, prev)?;
    }
    // Recorded on disk before the store changes: rollback finds it even
    // when the import fails later (or the set itself fails halfway).
    let write = SecretWrite {
        service: service.clone(),
        account: account.clone(),
        had_previous: previous.is_some(),
        written: hash_bytes(new_value.expose()),
    };
    write_secret_write(&ctx.env.vt_home, session, &write)?;
    ctx.store.set(&service, &account, &new_value)?;
    report.secret_write = Some(write);
    match ctx.store.get(&service, &account)? {
        Some(v) if v.expose() == new_value.expose() => report.verified += 1,
        _ => report
            .problems
            .push(format!("sır deposunda beklenen değer yok: {service}")),
    }
    report.expected += 1;
    Ok(take_oauth)
}

/// What the target keeps when it keeps its own login.
struct Extras {
    /// The new target value; `None` when nothing changes.
    value: Option<SecretBytes>,
    /// Keys (never values) where the target's differing value was kept.
    kept: Vec<String>,
    /// Why the non-login parts could not be merged, if so.
    unusable: Option<&'static str>,
}

/// The target value (or, with none, an empty object) plus the source's
/// non-login parts it lacks, e.g. MCP OAuth entries. Same-name keys with a
/// different value keep the target's and are listed in `kept`.
fn non_login_extras(target: Option<&SecretBytes>, source: &SecretBytes) -> Extras {
    let mut out = Extras {
        value: None,
        kept: Vec::new(),
        unusable: None,
    };
    let Ok(Value::Object(sm)) = serde_json::from_slice::<Value>(source.expose()) else {
        out.unusable =
            Some("kaynak sır değeri JSON nesnesi değil; giriş dışı kısımlar aktarılmadı");
        return out;
    };
    let mut tm = match target.map(|t| serde_json::from_slice::<Value>(t.expose())) {
        None => Map::new(),
        Some(Ok(Value::Object(m))) => m,
        Some(_) => {
            out.unusable =
                Some("hedef sır değeri JSON nesnesi değil; giriş dışı kısımlar aktarılmadı");
            return out;
        }
    };
    let mut changed = false;
    for (k, v) in sm.iter().filter(|(k, _)| k.as_str() != LOGIN_KEY) {
        match (tm.get_mut(k), v) {
            (None, _) => {
                tm.insert(k.clone(), v.clone());
                changed = true;
            }
            (Some(Value::Object(te)), Value::Object(se)) => {
                for (name, entry) in se {
                    match te.get(name) {
                        None => {
                            te.insert(name.clone(), entry.clone());
                            changed = true;
                        }
                        Some(existing) if existing != entry => {
                            out.kept.push(format!("{k}.{name}"));
                        }
                        Some(_) => {}
                    }
                }
            }
            (Some(existing), _) if existing != v => out.kept.push(k.clone()),
            (Some(_), _) => {}
        }
    }
    if changed {
        out.value = serde_json::to_vec(&Value::Object(tm))
            .ok()
            .map(SecretBytes::new);
    }
    out
}

/// Saves `value` age-encrypted with `pass` to
/// `<vt_home>/backups/<session>/secrets.age` (a new file).
pub(super) fn backup_secret(
    vt_home: &Path,
    session: &str,
    pass: &SecretString,
    service: &str,
    account: &str,
    value: &SecretBytes,
) -> Result<()> {
    let dir = vt_home.join("backups").join(session);
    create_private_dir_all(&dir).with_context(|| format!("oluşturulamadı: {}", dir.display()))?;
    let v = match std::str::from_utf8(value.expose()) {
        Ok(text) => json!({"service": service, "account": account, "value": text}),
        Err(_) => json!({"service": service, "account": account, "value_bytes": value.expose()}),
    };
    // Wiped on drop.
    let plain = SecretBytes::new(serde_json::to_vec(&json!([v]))?);
    encrypt_to_file(&dir.join("secrets.age"), pass, plain.expose())
}

/// Top-level session files under `projects/*/`.
fn count_sessions(claude_dir: &Path) -> u32 {
    let Ok(slugs) = fs::read_dir(claude_dir.join("projects")) else {
        return 0;
    };
    let mut n = 0;
    for slug in slugs.flatten() {
        if !slug.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let Ok(files) = fs::read_dir(slug.path()) else {
            continue;
        };
        n += files
            .flatten()
            .filter(|f| {
                f.file_type().is_ok_and(|t| t.is_file())
                    && f.path().extension().is_some_and(|e| e == "jsonl")
            })
            .count() as u32;
    }
    n
}

/// Claude check: the target holds at least the sessions it should.
fn check_sessions(
    plan: &ClaudePlan,
    decision: Decision,
    before: u32,
    claude_dir: &Path,
    report: &mut VerifyReport,
) {
    let after = count_sessions(claude_dir);
    let min = match decision {
        Decision::Replace => plan.sessions.placed,
        Decision::Merge => before + plan.sessions.new_files,
    };
    if after < min {
        report.problems.push(format!(
            "Claude oturum sayısı beklenenden az: {after} (en az {min} olmalı)"
        ));
    }
}

/// Warns when Claude's cleanup would delete transferred sessions and, with
/// the user's consent, writes the suggested `cleanupPeriodDays` (journaled
/// in its own apply session `<session>-ayar`). Returns the suggestion.
fn cleanup_period(
    ctx: &Ctx,
    plan: &ClaudePlan,
    session: &str,
    prompter: &mut Prompter,
    report: &mut VerifyReport,
) -> Result<Option<u64>> {
    let Some(oldest) = plan.sessions.oldest_mtime_ns else {
        return Ok(None);
    };
    let now_ns = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0);
    let age_days =
        u64::try_from(now_ns.saturating_sub(oldest) / 86_400_000_000_000).unwrap_or(0) + 1;
    let path = ctx.env.claude_dir.join("settings.json");
    let original = fs::read(&path).ok();
    let mut settings: Value = match &original {
        Some(bytes) => match serde_json::from_slice(bytes) {
            Ok(v) => v,
            Err(_) => {
                report.skipped.push((
                    path.display().to_string(),
                    "JSON okunamadı, cleanupPeriodDays denetlenemedi".into(),
                ));
                return Ok(None);
            }
        },
        None => json!({}),
    };
    let current = settings.get("cleanupPeriodDays").and_then(Value::as_u64);
    if current.is_some_and(|days| days >= age_days) {
        return Ok(None);
    }
    let suggested = age_days.max(CLAUDE_DEFAULT_CLEANUP_DAYS) + CLEANUP_MARGIN_DAYS;
    let now = match current {
        Some(days) => format!("{days} gün"),
        None => format!("tanımlı değil (Claude varsayılanı {CLAUDE_DEFAULT_CLEANUP_DAYS} gün)"),
    };
    eprintln!(
        "uyarı: Claude ayarlarında cleanupPeriodDays {now}; en eski aktarılan oturum {age_days} günlük. \
         Claude bu oturumları silebilir. Önerilen değer: {suggested}"
    );
    if !prompter.confirm_cleanup(suggested)? {
        return Ok(Some(suggested));
    }
    let Value::Object(map) = &mut settings else {
        report.skipped.push((
            path.display().to_string(),
            "nesne değil, cleanupPeriodDays yazılmadı".into(),
        ));
        return Ok(Some(suggested));
    };
    map.insert("cleanupPeriodDays".into(), json!(suggested));
    let mini = ApplyPlan {
        session_id: format!("{session}-ayar"),
        source_host: String::new(),
        placements: Vec::new(),
        replace_scopes: Vec::new(),
        json_writes: vec![(path.clone(), settings.clone())],
    };
    let empty = BTreeMap::new();
    let applied: ApplyReport = apply::apply(&mini, &ctx.env.vt_home, &mut Precomputed(&empty))
        .context("cleanupPeriodDays yazılamadı")?;
    let before = original.as_deref().map(hash_bytes).unwrap_or([0; 32]);
    report.transformed.push(TransformRecord {
        dest: path,
        before,
        after: hash_bytes(&json_bytes(&settings)),
        kind: "cleanup_period".into(),
    });
    report.conflicts.extend(applied.conflicts.iter().cloned());
    verify_plan(&mini, &applied, &BTreeSet::new(), report);
    Ok(Some(suggested))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes(v: Value) -> SecretBytes {
        SecretBytes::new(serde_json::to_vec(&v).unwrap())
    }

    #[test]
    fn kept_target_keys_are_listed_without_values() {
        let target = bytes(
            json!({"claudeAiOauth": {"t": "a"}, "mcpOAuth": {"x": "target", "y": "same"}, "other": 1}),
        );
        let source = bytes(
            json!({"claudeAiOauth": {"t": "b"}, "mcpOAuth": {"x": "source", "y": "same", "z": "new"}, "other": 2}),
        );
        let out = non_login_extras(Some(&target), &source);
        assert_eq!(out.kept, vec!["mcpOAuth.x".to_owned(), "other".to_owned()]);
        let merged: Value = serde_json::from_slice(out.value.unwrap().expose()).unwrap();
        assert_eq!(merged["claudeAiOauth"]["t"], "a");
        assert_eq!(merged["mcpOAuth"]["x"], "target");
        assert_eq!(merged["mcpOAuth"]["z"], "new");
    }
}
