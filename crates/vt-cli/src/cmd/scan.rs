//! `vibe-transfer scan`: what is here, what would move, and which files
//! hold plaintext secrets. Read-only; prints paths and counts, never values.

use std::collections::BTreeMap;

use anyhow::Result;
use indicatif::HumanBytes;
use serde::Serialize;
use vt_claude::{ClaudeAdapter, item_key};
use vt_core::manifest::{Category, EntryKind, Selection, Tool};

use super::{Ctx, ensure_supported};
use crate::secret_patterns::file_has_secret_pattern;
use crate::{ScanArgs, ui};

#[derive(Serialize)]
struct CategorySummary {
    category: Category,
    files: u64,
    entries: u64,
    bytes: u64,
}

#[derive(Serialize)]
struct Report {
    test_mode: bool,
    tool: Tool,
    categories: Vec<CategorySummary>,
    projects: usize,
    skipped: Vec<Item>,
    notes: Vec<Item>,
    secret_pattern_files: Vec<String>,
    secret_pattern_count: usize,
}

#[derive(Serialize)]
struct Item {
    path: String,
    reason: String,
}

pub fn run(ctx: &Ctx, args: &ScanArgs) -> Result<()> {
    let tool = args.tool.unwrap_or(Tool::Claude);
    ensure_supported(tool)?;
    // Everything, optional items included: scan shows what exists.
    let sel = Selection::from_flags(&[tool], &Category::ALL, &[]);
    let adapter = ClaudeAdapter {
        env: ctx.env.clone(),
    };
    let inv = adapter.inventory(&sel)?;

    let mut by_category: BTreeMap<Category, CategorySummary> = BTreeMap::new();
    let mut secret_files = Vec::new();
    let mut unreadable = Vec::new();
    for (entry, path) in &inv.entries {
        let sum = by_category
            .entry(entry.category)
            .or_insert(CategorySummary {
                category: entry.category,
                files: 0,
                entries: 0,
                bytes: 0,
            });
        sum.entries += 1;
        if entry.kind != EntryKind::File {
            continue;
        }
        sum.files += 1;
        sum.bytes += entry.size;
        let key = item_key(&entry.root, &entry.rel);
        match file_has_secret_pattern(path) {
            Ok(true) => secret_files.push(key),
            Ok(false) => {}
            // Gone or unreadable since the inventory: report, keep going.
            Err(e) => unreadable.push(Item {
                path: key,
                reason: format!("sır taraması için okunamadı: {e}"),
            }),
        }
    }
    secret_files.sort();

    let mut skipped: Vec<Item> = inv
        .skipped
        .into_iter()
        .map(|(path, reason)| Item { path, reason })
        .collect();
    skipped.extend(unreadable);
    let report = Report {
        test_mode: ctx.test_mode,
        tool,
        categories: by_category.into_values().collect(),
        projects: inv.projects.len(),
        skipped,
        notes: inv
            .notes
            .into_iter()
            .map(|(path, reason)| Item { path, reason })
            .collect(),
        secret_pattern_count: secret_files.len(),
        secret_pattern_files: secret_files,
    };

    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_human(ctx, &report);
    }
    Ok(())
}

fn print_human(ctx: &Ctx, r: &Report) {
    if let Some(notice) = ctx.test_mode_notice() {
        println!("{notice}");
        println!();
    }
    println!("Claude Code: {}", ctx.env.claude_dir.display());
    for c in &r.categories {
        println!(
            "  {:<26} {:>7} dosya  {:>10}",
            c.category.label(),
            c.files,
            HumanBytes(c.bytes).to_string()
        );
    }
    println!("Projeler: {}", r.projects);
    println!();

    let pairs = |items: &[Item]| -> Vec<(String, String)> {
        items
            .iter()
            .map(|i| (i.path.clone(), i.reason.clone()))
            .collect()
    };
    ui::print_grouped("Taşınmayanlar", &pairs(&r.skipped));
    ui::print_grouped("Notlar", &pairs(&r.notes));

    println!(
        "Düz metin sır deseni bulunan dosyalar: {}",
        r.secret_pattern_count
    );
    for path in &r.secret_pattern_files {
        println!("  {path}");
    }
    if r.secret_pattern_count > 0 {
        println!("Bu dosyalar paketin içinde şifreli taşınır.");
    }
}
