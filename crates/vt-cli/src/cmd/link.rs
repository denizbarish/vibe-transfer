//! `vibe-transfer link`: places the pending data of a project below its
//! folder on this machine, under the import's rules, in a journaled session
//! of its own. Once applied and verified the project leaves the pending
//! store: its record moves to `<vt_home>/backups/<session>/pending/`, where
//! `rollback` finds it again.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Serialize;
use vt_claude::{LinkInput, build_link_plan};
use vt_core::apply::{self, Resolution};
use vt_core::fsutil::create_private_dir_all;
use vt_core::pathmap::{Mapping, build_target_index, map_project};
use vt_core::pending::{PendingProject, PendingStore};
use vt_core::remote::normalize_remote;
use vt_core::report::{VerifyReport, verify_plan, write_report};
use vt_core::session::new_session_id;

use super::{Ctx, Precomputed, Questions, Staging, needs_confirmation, pending_store};
use crate::LinkArgs;
use crate::ui::{self, Prompter};

/// Folder levels below each root searched for target repositories.
const INDEX_DEPTH: usize = 6;
/// Where a linked project's pending record goes, below the session backup.
pub const ARCHIVE_DIR: &str = "pending";

#[derive(Serialize)]
struct Linked {
    project: String,
    path: String,
    session_id: String,
    report_path: String,
    placed: u32,
    verified: u32,
    expected: u32,
    problems: Vec<String>,
}

#[derive(Serialize)]
struct Summary {
    test_mode: bool,
    linked: Vec<Linked>,
    /// Pending projects `--auto` found no folder for.
    unmatched: Vec<String>,
    corrupt: Vec<String>,
}

pub fn run(ctx: &Ctx, args: &LinkArgs) -> Result<()> {
    if let Some(notice) = ctx.test_mode_notice() {
        eprintln!("{notice}");
    }
    let store = pending_store(&ctx.env.vt_home);
    let listing = store.list()?;
    let corrupt: Vec<String> = listing
        .corrupt
        .iter()
        .map(|r| r.path.display().to_string())
        .collect();
    for path in &corrupt {
        eprintln!(
            "uyarı: okunamayan bekleme kaydı atlandı: {}",
            ui::clean(path)
        );
    }
    let mut prompter = Prompter::new(ctx.answers.as_ref());
    let mut unmatched = Vec::new();
    let targets = if args.auto {
        auto_targets(ctx, args, &listing.items, &mut prompter, &mut unmatched)?
    } else {
        let (Some(selector), Some(path)) = (&args.project, &args.path) else {
            bail!("proje ve yol birlikte verilmeli ya da --auto kullanılmalı");
        };
        let project = find(&listing.items, selector)?;
        let root = target_root(path)?;
        let root = if needs_confirmation(&root, &ctx.env) {
            match prompter.choose_path(&project.project, std::slice::from_ref(&root))? {
                Some(root) => root,
                None => bail!("bağlama iptal edildi; veri bekleme alanında kaldı"),
            }
        } else {
            root
        };
        vec![(project.project.id.clone(), root)]
    };

    let mut linked = Vec::new();
    if !targets.is_empty() {
        ui::wait_until_closed("Claude", "Bağlamadan önce", &|| ctx.claude_pids())?;
    }
    for (id, root) in &targets {
        let project = store.take(id)?;
        linked.push(link_one(ctx, &store, &project, root, &mut prompter)?);
    }

    let summary = Summary {
        test_mode: ctx.test_mode,
        linked,
        unmatched,
        corrupt,
    };
    if args.json {
        println!("{}", serde_json::to_string_pretty(&summary)?);
    } else {
        print_human(&summary);
    }
    let failed: Vec<&Linked> = summary
        .linked
        .iter()
        .filter(|l| !l.problems.is_empty())
        .collect();
    if let Some(first) = failed.first() {
        bail!(
            "bağlandı ama doğrulamada sorun var; veri bekleme alanında kaldı. Rapor: {}",
            first.report_path
        );
    }
    Ok(())
}

fn print_human(s: &Summary) {
    let c = ui::clean;
    if s.linked.is_empty() {
        println!("Bağlanan proje yok.");
    }
    for l in &s.linked {
        println!("Bağlandı: {} -> {}", c(&l.project), c(&l.path));
        println!(
            "    Oturum: {}; yerleştirilen: {}, doğrulanan: {}/{}",
            l.session_id, l.placed, l.verified, l.expected
        );
        for p in &l.problems {
            println!("    SORUN: {}", c(p));
        }
        println!("    Rapor: {}", c(&l.report_path));
    }
    if !s.unmatched.is_empty() {
        println!("Hedefte klasörü bulunamayan: {}", s.unmatched.len());
        for id in &s.unmatched {
            println!("    {}", c(id));
        }
    }
}

/// The pending project `selector` names: its id, its remote (any URL
/// form), or its old path.
fn find<'a>(items: &'a [PendingProject], selector: &str) -> Result<&'a PendingProject> {
    let remote = normalize_remote(selector);
    let old_path = selector.trim_end_matches('/');
    let found: Vec<&PendingProject> = items
        .iter()
        .filter(|p| {
            p.project.id.0 == selector
                || (remote.is_some() && p.project.remote == remote)
                || (!old_path.is_empty() && p.project.old_path == old_path)
        })
        .collect();
    match found.as_slice() {
        [one] => Ok(one),
        [] => bail!("bekleyen proje bulunamadı: {selector}"),
        many => {
            let ids: Vec<&str> = many.iter().map(|p| p.project.id.0.as_str()).collect();
            bail!(
                "birden çok bekleyen proje eşleşti: {}; proje kimliğini verin",
                ids.join(", ")
            )
        }
    }
}

/// `path` made absolute; it must be an existing folder.
fn target_root(path: &Path) -> Result<PathBuf> {
    let root =
        std::path::absolute(path).with_context(|| format!("yol çözülemedi: {}", path.display()))?;
    if !fs::metadata(&root).is_ok_and(|m| m.is_dir()) {
        bail!("klasör yok: {}", root.display());
    }
    Ok(root)
}

/// Every pending project the target index maps to an existing folder, with
/// the import's questions for several candidates or a sensitive folder.
fn auto_targets(
    ctx: &Ctx,
    args: &LinkArgs,
    items: &[PendingProject],
    prompter: &mut Prompter,
    unmatched: &mut Vec<String>,
) -> Result<Vec<(vt_core::manifest::ProjectId, PathBuf)>> {
    let roots = if args.roots.is_empty() {
        vec![ctx.env.home.clone()]
    } else {
        args.roots.clone()
    };
    let idx = build_target_index(&roots, INDEX_DEPTH);
    let mut out = Vec::new();
    for p in items {
        let src_home = p
            .items
            .iter()
            .find_map(|i| i.source_home.clone())
            .unwrap_or_default();
        let chosen = match map_project(&p.project, &idx, &src_home, &ctx.env.home) {
            Mapping::Auto(path) if !path.is_dir() => None,
            Mapping::Auto(path) if needs_confirmation(&path, &ctx.env) => {
                prompter.choose_path(&p.project, std::slice::from_ref(&path))?
            }
            Mapping::Auto(path) => Some(path),
            Mapping::Choose(candidates) => prompter.choose_path(&p.project, &candidates)?,
            Mapping::Pending => None,
        };
        match chosen {
            Some(path) => out.push((p.project.id.clone(), path)),
            None => unmatched.push(p.project.id.0.clone()),
        }
    }
    Ok(out)
}

/// Plans, applies and verifies one project in a new session; on success
/// its pending record moves to the session backup.
fn link_one(
    ctx: &Ctx,
    store: &PendingStore,
    project: &PendingProject,
    root: &Path,
    prompter: &mut Prompter,
) -> Result<Linked> {
    let env = &ctx.env;
    let session = new_session_id();
    let staging = env.vt_home.join("staging").join(&session);
    create_private_dir_all(&staging)
        .with_context(|| format!("oluşturulamadı: {}", staging.display()))?;
    let _staging = Staging(staging.clone());
    let work_dir = staging.join("xform");
    create_private_dir_all(&work_dir)?;

    let input = LinkInput {
        project,
        root,
        session_id: &session,
        work_dir: &work_dir,
    };
    let plan = build_link_plan(env, &input, &mut Questions(prompter))?;
    // The plan may have asked questions: Claude can have started since.
    ui::wait_until_closed("Claude", "Bağlamadan önce", &|| ctx.claude_pids())?;
    let applied = apply::apply(
        &plan.plan,
        &env.vt_home,
        &mut Precomputed(&plan.resolutions),
    )?;

    let mut report = VerifyReport {
        session_id: session.clone(),
        transformed: plan.transformed.clone(),
        skipped: plan.skipped.clone(),
        problems: plan.problems.clone(),
        ..VerifyReport::default()
    };
    let kept_target: BTreeSet<PathBuf> = plan
        .resolutions
        .iter()
        .filter(|(_, r)| **r == Resolution::KeepTarget)
        .map(|(d, _)| d.clone())
        .collect();
    report.conflicts.extend(applied.conflicts.iter().cloned());
    verify_plan(&plan.plan, &applied, &kept_target, &mut report);
    let report_path = write_report(&env.vt_home, &report)?;
    if report.problems.is_empty() {
        let archive = env.vt_home.join("backups").join(&session).join(ARCHIVE_DIR);
        store
            .archive(&project.project.id, &archive)
            .context("bağlandı ama bekleyen kayıt kaldırılamadı")?;
    }
    Ok(Linked {
        project: project.project.id.0.clone(),
        path: root.display().to_string(),
        session_id: session,
        report_path: report_path.display().to_string(),
        placed: applied.placed,
        verified: report.verified,
        expected: report.expected,
        problems: report.problems,
    })
}
