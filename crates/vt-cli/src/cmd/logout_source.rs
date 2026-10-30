//! `vibe-transfer logout-source`: runs on the source machine once the login
//! works on the target. The Claude secret an export carried is backed up
//! age-encrypted with a passphrase the user enters, the backup is read back
//! and compared, and only then is the secret deleted from the store. This
//! is the only command that changes the source.

use std::fs;

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::Value;
use vt_core::apply::validate_session;
use vt_core::bundle::{SecretString, decrypt_file};
use vt_core::secret_types::{SecretKind, SecretMeta};
use vt_secrets::claude_service_name;

use super::Ctx;
use super::import::backup_secret;
use crate::LogoutSourceArgs;
use crate::ui;

/// The part of `<vt_home>/outbox/<session>/meta.json` this command reads.
#[derive(Deserialize)]
struct OutboxMeta {
    session_id: String,
    secrets: Vec<SecretMeta>,
}

pub fn run(ctx: &Ctx, args: &LogoutSourceArgs) -> Result<()> {
    if let Some(notice) = ctx.test_mode_notice() {
        eprintln!("{notice}");
    }
    let env = &ctx.env;
    validate_session(&args.session)?;
    let meta_path = env
        .vt_home
        .join("outbox")
        .join(&args.session)
        .join("meta.json");
    let bytes = fs::read(&meta_path)
        .with_context(|| format!("dışa aktarma kaydı okunamadı: {}", meta_path.display()))?;
    let meta: OutboxMeta = serde_json::from_slice(&bytes)
        .with_context(|| format!("dışa aktarma kaydı bozuk: {}", meta_path.display()))?;
    if meta.session_id != args.session {
        bail!(
            "dışa aktarma kaydı başka bir oturuma ait: {}",
            meta_path.display()
        );
    }
    if !meta
        .secrets
        .iter()
        .any(|s| s.kind == SecretKind::ClaudeKeychain)
    {
        bail!("bu dışa aktarmada Claude giriş kaydı taşınmadı; kaldırılacak bir şey yok");
    }
    let backup_session = format!("logout-{}", args.session);
    let backup = env
        .vt_home
        .join("backups")
        .join(&backup_session)
        .join("secrets.age");
    if fs::symlink_metadata(&backup).is_ok() {
        bail!(
            "bu oturum için yedek zaten var, üzerine yazılmaz: {}",
            backup.display()
        );
    }

    // Claude rewrites its credentials while it runs.
    ui::wait_until_closed("Claude", "Girişi kaldırmadan önce", &|| {
        ctx.claude_pids()
    })?;
    let service = claude_service_name(env.claude_config_dir_override.as_deref());
    let account = env.user.clone();
    let Some(value) = ctx.store.get(&service, &account)? else {
        bail!("Claude giriş kaydı bulunamadı (zaten kaldırılmış olabilir): {service}");
    };

    let pass = read_passphrase(args)?;
    backup_secret(
        &env.vt_home,
        &backup_session,
        &pass,
        &service,
        &account,
        &value,
    )?;
    let plain = decrypt_file(&backup, &pass).context("yedek doğrulanamadı")?;
    let items: Value = serde_json::from_slice(&plain).context("yedek doğrulanamadı")?;
    let item = &items[0];
    let same = item["service"] == service.as_str()
        && item["account"] == account.as_str()
        && match std::str::from_utf8(value.expose()) {
            Ok(text) => item["value"] == text,
            Err(_) => serde_json::from_value::<Vec<u8>>(item["value_bytes"].clone())
                .is_ok_and(|b| b == value.expose()),
        };
    if !same {
        bail!(
            "yedek açıldı ama değer eşleşmedi; sır silinmedi. Yedek: {}",
            backup.display()
        );
    }

    ctx.store.delete(&service, &account)?;
    if ctx.store.get(&service, &account)?.is_some() {
        bail!("sır deposundan silinemedi: {service}");
    }
    println!("Kaynak makinede Claude girişi kaldırıldı: {service}");
    println!(
        "Yedek (girdiğiniz parolayla şifreli): {}",
        ui::clean(&backup.display().to_string())
    );
    Ok(())
}

/// The backup passphrase: typed twice on a terminal, or one line of stdin
/// with `--passphrase-stdin`.
fn read_passphrase(args: &LogoutSourceArgs) -> Result<SecretString> {
    if !args.passphrase_stdin && !ui::interactive() {
        bail!("yedek parolası için terminal yok; parolayı --passphrase-stdin ile verin");
    }
    ui::read_passphrase_stdin(true)
}
