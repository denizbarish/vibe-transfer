//! Terminal interaction: prompts, progress, passphrase display and report
//! lists. Prompts and progress go to stderr; stdout carries only results.

use std::collections::BTreeMap;
use std::io::{self, BufRead, IsTerminal};

use anyhow::{Context, Result, bail};
use dialoguer::{Confirm, MultiSelect, Password};
use indicatif::{ProgressBar, ProgressDrawTarget, ProgressStyle};
use vt_core::bundle::{ExposeSecret, SecretString};
use vt_core::manifest::{Category, Selection, Tool};

/// Report lines shown per reason before the rest is only counted.
const LIST_LIMIT: usize = 20;

/// Whether the user can answer prompts: stdin and stderr are terminals.
pub fn interactive() -> bool {
    io::stdin().is_terminal() && io::stderr().is_terminal()
}

/// Asks for the categories to transfer. Claude is the only tool so far, so
/// there is no tool question yet.
pub fn prompt_selection() -> Result<Selection> {
    let tool = Tool::Claude;
    let labels: Vec<&str> = Category::ALL.iter().map(|c| c.label()).collect();
    let defaults: Vec<bool> = Category::ALL
        .iter()
        .map(|c| *c != Category::Cache)
        .collect();
    let picked = MultiSelect::new()
        .with_prompt("Claude Code: taşınacak kategoriler")
        .items(&labels)
        .defaults(&defaults)
        .interact()
        .context("seçim yapılamadı")?;
    if picked.is_empty() {
        bail!("hiç kategori seçilmedi");
    }
    let only: Vec<Category> = picked.into_iter().map(|i| Category::ALL[i]).collect();
    Ok(Selection::from_flags(&[tool], &only, &[]))
}

/// Warns that the tool is running; on a terminal asks whether to go on.
pub fn confirm_while_running(tool_name: &str, pids: &[u32]) -> Result<()> {
    if pids.is_empty() {
        return Ok(());
    }
    let list: Vec<String> = pids.iter().map(u32::to_string).collect();
    eprintln!(
        "uyarı: {tool_name} çalışıyor (PID {}). Açık oturumların yazılmakta olan son satırları pakete girmez.",
        list.join(", ")
    );
    if interactive() {
        let go_on = Confirm::new()
            .with_prompt("Yine de devam edilsin mi?")
            .default(false)
            .interact()
            .context("onay alınamadı")?;
        if !go_on {
            bail!("iptal edildi");
        }
    }
    Ok(())
}

/// Reads the passphrase from stdin. On a terminal it is typed twice without
/// echo; otherwise the first line is used.
pub fn read_passphrase_stdin() -> Result<SecretString> {
    let pass = if io::stdin().is_terminal() {
        Password::new()
            .with_prompt("Paket parolası")
            .with_confirmation("Paket parolası (tekrar)", "Parolalar eşleşmedi")
            .interact()
            .context("parola okunamadı")?
    } else {
        let mut line = String::new();
        io::stdin()
            .lock()
            .read_line(&mut line)
            .context("parola okunamadı")?;
        let len = line.trim_end_matches(['\r', '\n']).len();
        line.truncate(len);
        line
    };
    if pass.is_empty() {
        bail!("parola boş olamaz");
    }
    Ok(SecretString::from(pass))
}

/// Fails unless a generated passphrase can be shown on a terminal: written
/// anywhere else it could end up in a log.
pub fn ensure_passphrase_display() -> Result<()> {
    if !io::stderr().is_terminal() {
        bail!(
            "üretilen parola yalnız terminalde gösterilir; terminal yoksa parolayı --passphrase-stdin ile verin"
        );
    }
    Ok(())
}

/// Shows a generated passphrase once, on stderr (a terminal).
pub fn show_passphrase(pass: &SecretString) {
    eprintln!();
    eprintln!("Paket parolası (yalnız şimdi gösterilir):");
    eprintln!();
    eprintln!("    {}", pass.expose_secret());
    eprintln!();
}

pub fn warn_separate_channel() {
    eprintln!("uyarı: Parolayı paket dosyasıyla aynı kanaldan gönderme.");
}

/// Byte progress on stderr; hidden when stderr is not a terminal or
/// `hidden` is set.
pub fn progress(total: u64, hidden: bool) -> ProgressBar {
    if hidden || !io::stderr().is_terminal() {
        return ProgressBar::hidden();
    }
    let bar = ProgressBar::with_draw_target(Some(total), ProgressDrawTarget::stderr());
    let style = ProgressStyle::with_template("{bar:30} {bytes}/{total_bytes} {msg}")
        .unwrap_or_else(|_| ProgressStyle::default_bar());
    bar.set_style(style);
    bar
}

/// Prints `(path, reason)` items grouped by reason, at most [`LIST_LIMIT`]
/// paths per reason.
pub fn print_grouped(title: &str, items: &[(String, String)]) {
    if items.is_empty() {
        return;
    }
    let mut groups: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (path, reason) in items {
        groups.entry(reason).or_default().push(path);
    }
    println!("{title}: {}", items.len());
    for (reason, paths) in groups {
        println!("  {reason} ({})", paths.len());
        for path in paths.iter().take(LIST_LIMIT) {
            println!("    {path}");
        }
        if paths.len() > LIST_LIMIT {
            println!(
                "    ... ve {} tane daha (tam liste: --json)",
                paths.len() - LIST_LIMIT
            );
        }
    }
}
