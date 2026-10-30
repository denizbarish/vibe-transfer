//! `vibe-transfer rollback`: undoes a session (default: the latest) after
//! the user confirms; no flag skips the question. The session's settings
//! companion (`<session>-ayar`) is undone first, projects a `link` session
//! took out of the pending store go back into it, and the secret store
//! item an import wrote is restored (needs the bundle passphrase) or
//! removed.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;
use vt_core::apply::{self, RollbackReport, validate_session};
use vt_core::bundle::{SecretString, decrypt_file};
use vt_core::fsutil::{create_private_dir_all, create_private_file};
use vt_core::report::{SecretWrite, VerifyReport, hash_bytes, read_secret_write};
use vt_core::secret_types::SecretBytes;

use super::link::ARCHIVE_DIR;
use super::{Ctx, pending_store, sessions_in};
use crate::RollbackArgs;
use crate::ui::{self, Prompter};

/// Suffix of the session that writes `cleanupPeriodDays` after an import.
const SETTINGS_SUFFIX: &str = "-ayar";
/// Marker in the session backup: the secret step is done.
const SECRET_DONE: &str = "secret-undone";

/// What happened to the import's secret store write.
enum SecretStep {
    /// The session wrote no secret, or it was already undone.
    Nothing,
    Done {
        restored: bool,
        /// The value had changed since the import (e.g. a refreshed token).
        changed: bool,
    },
    /// Not undone; why.
    Pending(String),
}

pub fn run(ctx: &Ctx, args: &RollbackArgs) -> Result<()> {
    if let Some(notice) = ctx.test_mode_notice() {
        eprintln!("{notice}");
    }
    let vt_home = &ctx.env.vt_home;
    let session = match &args.session {
        Some(s) => s.clone(),
        None => sessions_in(vt_home, "journal", ".jsonl")?
            .pop()
            .context("geri alınacak oturum yok")?,
    };
    validate_session(&session)?;
    let journal = |id: &str| vt_home.join("journal").join(format!("{id}.jsonl"));
    if !journal(&session).is_file() {
        bail!("oturum günlüğü bulunamadı: {session}");
    }
    let mut prompter = Prompter::new(ctx.answers.as_ref());
    if !prompter.confirm_rollback(&session)? {
        bail!("iptal edildi; hiçbir şey değişmedi");
    }
    ui::wait_until_closed("Claude", "Geri almadan önce", &|| ctx.claude_pids())?;

    let mut total = RollbackReport::default();
    let companion = format!("{session}{SETTINGS_SUFFIX}");
    if journal(&companion).is_file() {
        add(&mut total, apply::rollback(vt_home, &companion)?);
    }
    add(&mut total, apply::rollback(vt_home, &session)?);
    let backups = vt_home.join("backups").join(&session);
    let restored = pending_store(vt_home)
        .restore(&backups.join(ARCHIVE_DIR))
        .context("geri alındı ama bekleyen kayıt geri konamadı")?;

    let secret = undo_secret(ctx, args, &session, &backups)?;

    let c = ui::clean;
    println!("Geri alındı: {session}");
    println!(
        "Geri konan yedek: {}, kaldırılan: {}, kaldırılan boş klasör: {}",
        total.restored, total.removed, total.dirs_removed
    );
    if !total.set_aside.is_empty() {
        println!(
            "Sonradan değiştiği için silinmeyip kenara alınan: {}",
            total.set_aside.len()
        );
        for p in &total.set_aside {
            println!("    {}", c(&p.display().to_string()));
        }
    }
    for id in &restored {
        println!("Bekleme alanına geri kondu: {}", c(&id.0));
    }
    match &secret {
        SecretStep::Nothing => {}
        SecretStep::Done { restored, changed } => {
            if *restored {
                println!("Sır deposundaki önceki değer geri kondu.");
            } else {
                println!("İçe aktarılan sır, sır deposundan kaldırıldı.");
            }
            if *changed {
                println!(
                    "Not: sır içe aktarmadan sonra değişmişti (ör. token yenilendi); yine de geri alındı."
                );
            }
        }
        SecretStep::Pending(reason) => {
            eprintln!("UYARI: sır adımı geri alınmadı, bekliyor: {}", c(reason));
            eprintln!(
                "Dosyalar geri alındı. Sır adımını tamamlamak için bir terminalde yeniden çalıştırın; onay ve paket parolası sorulur:"
            );
            eprintln!("    vibe-transfer rollback {session}");
            bail!("sır adımı bekliyor: {reason}");
        }
    }
    Ok(())
}

fn add(total: &mut RollbackReport, r: RollbackReport) {
    total.restored += r.restored;
    total.removed += r.removed;
    total.dirs_removed += r.dirs_removed;
    total.set_aside.extend(r.set_aside);
}

/// Undoes the secret store write recorded in the session's report, once.
fn undo_secret(
    ctx: &Ctx,
    args: &RollbackArgs,
    session: &str,
    backups: &Path,
) -> Result<SecretStep> {
    // The dedicated record is written before the store changes; older
    // imports only have it in the report.
    let recorded = match read_secret_write(&ctx.env.vt_home, session)? {
        Some(w) => Some(w),
        None => report_secret_write(&ctx.env.vt_home, session)?,
    };
    let Some(w) = recorded else {
        return Ok(SecretStep::Nothing);
    };
    let marker = backups.join(SECRET_DONE);
    if marker.exists() {
        return Ok(SecretStep::Nothing);
    }
    let current = ctx.store.get(&w.service, &w.account)?;
    // Digests only: values are never compared in a way that could be logged.
    let changed = current.as_ref().map(|v| hash_bytes(v.expose())) != Some(w.written);
    drop(current);
    let restored = if w.had_previous {
        let pass = match passphrase(args) {
            Ok(Some(pass)) => pass,
            Ok(None) => {
                return Ok(SecretStep::Pending(
                    "önceki değeri geri koymak için paket parolası gerekiyor; terminal yok".into(),
                ));
            }
            Err(e) => return Ok(SecretStep::Pending(format!("parola alınamadı: {e:#}"))),
        };
        let previous = match previous_value(&backups.join("secrets.age"), &pass, &w) {
            Ok(v) => v,
            Err(e) => return Ok(SecretStep::Pending(format!("{e:#}"))),
        };
        ctx.store.set(&w.service, &w.account, &previous)?;
        match ctx.store.get(&w.service, &w.account)? {
            Some(v) if v.expose() == previous.expose() => {}
            _ => bail!("önceki sır değeri geri konamadı: {}", w.service),
        }
        true
    } else {
        ctx.store.delete(&w.service, &w.account)?;
        if ctx.store.get(&w.service, &w.account)?.is_some() {
            bail!("içe aktarılan sır kaldırılamadı: {}", w.service);
        }
        false
    };
    create_private_dir_all(backups)
        .with_context(|| format!("oluşturulamadı: {}", backups.display()))?;
    create_private_file(&marker)
        .with_context(|| format!("oluşturulamadı: {}", marker.display()))?;
    Ok(SecretStep::Done { restored, changed })
}

/// `secret_write` of the session's report, for imports made before the
/// dedicated record existed.
fn report_secret_write(vt_home: &Path, session: &str) -> Result<Option<SecretWrite>> {
    let path = vt_home.join("reports").join(format!("{session}.json"));
    let Ok(bytes) = fs::read(&path) else {
        return Ok(None);
    };
    let report: VerifyReport = serde_json::from_slice(&bytes)
        .with_context(|| format!("rapor bozuk: {}", path.display()))?;
    Ok(report.secret_write)
}

/// The bundle passphrase: from stdin with `--passphrase-stdin`, typed on a
/// terminal, or `None` when neither is possible.
fn passphrase(args: &RollbackArgs) -> Result<Option<SecretString>> {
    if args.passphrase_stdin || ui::interactive() {
        return ui::read_passphrase_stdin(false).map(Some);
    }
    Ok(None)
}

/// The value the import saved before overwriting `w`.
fn previous_value(path: &Path, pass: &SecretString, w: &SecretWrite) -> Result<SecretBytes> {
    let plain = decrypt_file(path, pass)?;
    let items: Value = serde_json::from_slice(&plain).context("sır yedeği okunamadı")?;
    let item = items
        .as_array()
        .and_then(|a| {
            a.iter()
                .find(|i| i["service"] == w.service.as_str() && i["account"] == w.account.as_str())
        })
        .ok_or_else(|| anyhow!("sır yedeğinde bu kayıt yok: {}", w.service))?;
    if let Some(text) = item["value"].as_str() {
        return Ok(SecretBytes::new(text.as_bytes().to_vec()));
    }
    serde_json::from_value::<Vec<u8>>(item["value_bytes"].clone())
        .map(SecretBytes::new)
        .context("sır yedeği okunamadı")
}
