//! `vibe-transfer rollback`: undoes a session (default: the latest) after
//! the user confirms; no flag skips the question. The session's settings
//! companion (`<session>-ayar`) is undone first, and projects a `link`
//! session took out of the pending store go back into it.

use anyhow::{Context, Result, bail};
use vt_core::apply::{self, RollbackReport, validate_session};

use super::link::ARCHIVE_DIR;
use super::{Ctx, pending_store, sessions_in};
use crate::RollbackArgs;
use crate::ui::{self, Prompter};

/// Suffix of the session that writes `cleanupPeriodDays` after an import.
const SETTINGS_SUFFIX: &str = "-ayar";

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
    let secret_backup = backups.join("secrets.age");
    if secret_backup.is_file() {
        println!(
            "Not: sır deposundaki değer geri alınmadı. Önceki değer paket parolasıyla şifreli olarak burada: {}",
            c(&secret_backup.display().to_string())
        );
    }
    Ok(())
}

fn add(total: &mut RollbackReport, r: RollbackReport) {
    total.restored += r.restored;
    total.removed += r.removed;
    total.dirs_removed += r.dirs_removed;
    total.set_aside.extend(r.set_aside);
}
