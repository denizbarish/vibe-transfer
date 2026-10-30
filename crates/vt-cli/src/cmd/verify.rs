//! `vibe-transfer verify`: checks again every object a session's report
//! recorded as verified. Exits with an error (code 1) on any deviation and
//! lists the deviating paths only, never content.

use std::fs;

use anyhow::{Context, Result, bail};
use serde::Serialize;
use vt_core::apply::validate_session;
use vt_core::report::{VerifyReport, deviations};

use super::{Ctx, sessions_in};
use crate::VerifyArgs;
use crate::ui;

#[derive(Serialize)]
struct Summary {
    test_mode: bool,
    session_id: String,
    checked: usize,
    deviations: Vec<String>,
}

pub fn run(ctx: &Ctx, args: &VerifyArgs) -> Result<()> {
    if let Some(notice) = ctx.test_mode_notice() {
        eprintln!("{notice}");
    }
    let vt_home = &ctx.env.vt_home;
    let session = match &args.session {
        Some(s) => s.clone(),
        None => sessions_in(vt_home, "reports", ".json")?
            .pop()
            .context("doğrulanacak rapor yok")?,
    };
    validate_session(&session)?;
    let path = vt_home.join("reports").join(format!("{session}.json"));
    let bytes = fs::read(&path).with_context(|| format!("rapor okunamadı: {}", path.display()))?;
    let report: VerifyReport = serde_json::from_slice(&bytes)
        .with_context(|| format!("rapor bozuk: {}", path.display()))?;
    if report.files.is_empty() && report.verified > 0 {
        bail!(
            "bu rapor dosya listesi içermiyor (eski sürüm); yeniden denetlenemez: {}",
            path.display()
        );
    }
    let found = deviations(&report.files);
    let summary = Summary {
        test_mode: ctx.test_mode,
        session_id: session.clone(),
        checked: report.files.len(),
        deviations: found.iter().map(|p| p.display().to_string()).collect(),
    };
    if args.json {
        println!("{}", serde_json::to_string_pretty(&summary)?);
    } else {
        println!(
            "Oturum {}: denetlenen {}, sapma {}",
            summary.session_id,
            summary.checked,
            summary.deviations.len()
        );
        for p in &summary.deviations {
            println!("    {}", ui::clean(p));
        }
    }
    if !summary.deviations.is_empty() {
        bail!(
            "{} dosya rapordaki hâlinden farklı ya da yok",
            summary.deviations.len()
        );
    }
    Ok(())
}
