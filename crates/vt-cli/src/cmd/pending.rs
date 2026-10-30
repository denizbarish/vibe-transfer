//! `vibe-transfer pending`: what waits in `<vt_home>/pending` for a project
//! root, and (`--orphans`) files no record owns, removed only on consent.

use anyhow::Result;
use serde::Serialize;
use vt_core::pending::CorruptRecord;

use super::{Ctx, pending_store};
use crate::PendingArgs;
use crate::ui::{self, Prompter};

#[derive(Serialize)]
struct Project {
    id: String,
    old_path: String,
    remote: Option<String>,
    items: usize,
    bytes: u64,
}

#[derive(Serialize)]
struct Corrupt {
    path: String,
    error: String,
}

#[derive(Serialize)]
struct Listed {
    test_mode: bool,
    projects: Vec<Project>,
    corrupt: Vec<Corrupt>,
}

#[derive(Serialize)]
struct Orphans {
    test_mode: bool,
    orphans: Vec<String>,
    removed: Vec<String>,
    corrupt: Vec<Corrupt>,
}

pub fn run(ctx: &Ctx, args: &PendingArgs) -> Result<()> {
    if let Some(notice) = ctx.test_mode_notice() {
        eprintln!("{notice}");
    }
    if args.orphans {
        return orphans(ctx, args);
    }
    let listing = pending_store(&ctx.env.vt_home).list()?;
    let out = Listed {
        test_mode: ctx.test_mode,
        projects: listing
            .items
            .iter()
            .map(|p| Project {
                id: p.project.id.0.clone(),
                old_path: p.project.old_path.clone(),
                remote: p.project.remote.clone(),
                items: p.items.len(),
                bytes: p.items.iter().map(|i| i.entry.size).sum(),
            })
            .collect(),
        corrupt: corrupt(&listing.corrupt),
    };
    if args.json {
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }
    let c = ui::clean;
    if out.projects.is_empty() {
        println!("Bekleyen proje yok.");
    }
    for p in &out.projects {
        println!("{}", c(&p.id));
        println!("    eski yol: {}", c(&p.old_path));
        println!("    öğe: {}, boyut: {} bayt", p.items, p.bytes);
    }
    print_corrupt(&out.corrupt);
    Ok(())
}

fn orphans(ctx: &Ctx, args: &PendingArgs) -> Result<()> {
    let store = pending_store(&ctx.env.vt_home);
    let listing = store.list_orphans()?;
    let mut removed = Vec::new();
    if !listing.items.is_empty() {
        if !args.json {
            println!("Sahipsiz dosyalar: {}", listing.items.len());
            for path in &listing.items {
                println!("    {}", ui::clean(&path.display().to_string()));
            }
        }
        let mut prompter = Prompter::new(ctx.answers.as_ref());
        match prompter.confirm_orphans(listing.items.len())? {
            Some(true) => removed = store.remove_orphans(&listing.items)?,
            Some(false) => {}
            None if !args.json => {
                println!("Silinmedi: onay için komutu bir terminalde çalıştırın.")
            }
            None => {}
        }
    }
    let out = Orphans {
        test_mode: ctx.test_mode,
        orphans: listing
            .items
            .iter()
            .map(|p| p.display().to_string())
            .collect(),
        removed: removed.iter().map(|p| p.display().to_string()).collect(),
        corrupt: corrupt(&listing.corrupt),
    };
    if args.json {
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }
    if out.orphans.is_empty() {
        println!("Sahipsiz dosya yok.");
    } else if !out.removed.is_empty() {
        println!("Silinen: {}", out.removed.len());
    }
    print_corrupt(&out.corrupt);
    Ok(())
}

fn corrupt(records: &[CorruptRecord]) -> Vec<Corrupt> {
    records
        .iter()
        .map(|r| Corrupt {
            path: r.path.display().to_string(),
            error: r.error.clone(),
        })
        .collect()
}

fn print_corrupt(records: &[Corrupt]) {
    if records.is_empty() {
        return;
    }
    println!(
        "Okunamayan bekleme kayıtları (dokunulmadı): {}",
        records.len()
    );
    for r in records {
        println!("    {}: {}", ui::clean(&r.path), ui::clean(&r.error));
    }
}
