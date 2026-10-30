//! Terminal interaction: prompts, progress, passphrase display and report
//! lists. Prompts and progress go to stderr; stdout carries only results.

use std::collections::BTreeMap;
use std::io::{self, BufRead, IsTerminal};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use dialoguer::{Confirm, MultiSelect, Password, Select};
use indicatif::{ProgressBar, ProgressDrawTarget, ProgressStyle};
use serde::Deserialize;
use vt_core::apply::{Decision, Resolution};
use vt_core::bundle::{ExposeSecret, SecretString};
use vt_core::manifest::{Category, JsonConflict, ProjectRef, Selection, Tool};

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

/// Reads the passphrase from stdin. On a terminal it is typed without echo
/// (twice when `confirm`); otherwise the first line is used.
pub fn read_passphrase_stdin(confirm: bool) -> Result<SecretString> {
    let pass = if io::stdin().is_terminal() {
        let mut prompt = Password::new().with_prompt("Paket parolası");
        if confirm {
            prompt = prompt.with_confirmation("Paket parolası (tekrar)", "Parolalar eşleşmedi");
        }
        prompt.interact().context("parola okunamadı")?
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

/// Asks for the passphrase on the terminal; without one it must come from
/// `--passphrase-stdin`.
pub fn prompt_passphrase() -> Result<SecretString> {
    if !interactive() {
        bail!("parola için terminal yok; parolayı --passphrase-stdin ile verin");
    }
    read_passphrase_stdin(false)
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

/// Answers to the interactive questions, for tests only (`VT_TEST_ANSWERS`,
/// valid only with `VT_TEST_HOME`). This is the single place answers come
/// from besides the terminal; no command-line flag sets any of them.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Answers {
    /// Claude merge or replace: `merge` | `replace`.
    pub decision: Option<String>,
    /// Per project id: `merge` | `replace`.
    #[serde(default)]
    pub projects: BTreeMap<String, String>,
    /// Different target account: `keep_target` | `take_source`.
    pub account: Option<String>,
    /// Per project id with several candidates: a path, or `pending`.
    #[serde(default)]
    pub choose: BTreeMap<String, String>,
    /// Every differing file: `source` | `target` | `both`.
    pub file: Option<String>,
    /// Every differing JSON key: `source` | `target`.
    pub json: Option<String>,
    /// Whether to write the suggested `cleanupPeriodDays`.
    pub cleanup: Option<bool>,
    /// PIDs to treat as a running Claude (the real process list belongs to
    /// the real machine, not the synthetic home).
    #[serde(default)]
    pub claude_pids: Vec<u32>,
}

impl Answers {
    pub fn parse(text: &str) -> Result<Self> {
        serde_json::from_str(text).context("VT_TEST_ANSWERS geçerli JSON değil")
    }
}

/// Asks the user, or reads the test answers. Without a terminal and without
/// test answers every question fails instead of guessing.
pub struct Prompter<'a> {
    answers: Option<&'a Answers>,
    all_files: Option<Resolution>,
    all_json: Option<Resolution>,
}

/// Error for a question that cannot be asked.
fn no_terminal(question: &str) -> anyhow::Error {
    anyhow!(
        "\"{question}\" sorusu kullanıcı kararı gerektirir ve hiçbir bayrakla atlanamaz; \
         terminal yok. Komutu bir terminalde çalıştırın."
    )
}

fn missing_answer(key: &str) -> anyhow::Error {
    anyhow!("VT_TEST_ANSWERS içinde \"{key}\" yanıtı yok")
}

fn select(prompt: &str, items: &[&str]) -> Result<usize> {
    Select::new()
        .with_prompt(prompt)
        .items(items)
        .default(0)
        .interact()
        .context("seçim yapılamadı")
}

fn parse_decision(s: &str) -> Result<Decision> {
    match s {
        "merge" => Ok(Decision::Merge),
        "replace" => Ok(Decision::Replace),
        other => bail!("geçersiz karar yanıtı: {other}"),
    }
}

impl<'a> Prompter<'a> {
    pub fn new(answers: Option<&'a Answers>) -> Self {
        Self {
            answers,
            all_files: None,
            all_json: None,
        }
    }

    fn decision_prompt(&self, question: &str) -> Result<Decision> {
        if !interactive() {
            return Err(no_terminal(question));
        }
        let picked = select(
            question,
            &[
                "Birleştir (iki taraf da kalır)",
                "Değiştir (kaynak kazanır, hedef yedeklenir)",
            ],
        )?;
        Ok(if picked == 0 {
            Decision::Merge
        } else {
            Decision::Replace
        })
    }

    /// Merge or replace for the Claude data on the target.
    pub fn claude_decision(&mut self) -> Result<Decision> {
        let question = "Hedefte Claude verisi var. Claude için birleştir mi, değiştir mi?";
        match self.answers {
            Some(a) => parse_decision(
                a.decision
                    .as_deref()
                    .ok_or_else(|| missing_answer("decision"))?,
            ),
            None => self.decision_prompt(question),
        }
    }

    pub fn project_decision(&mut self, project: &ProjectRef, root: &Path) -> Result<Decision> {
        let question = format!(
            "{} projesinde ({}) hedefte farklı içerik var. Birleştir mi, değiştir mi?",
            project.id.0,
            root.display()
        );
        match self.answers {
            Some(a) => match a.projects.get(&project.id.0).or(a.decision.as_ref()) {
                Some(s) => parse_decision(s),
                None => Err(missing_answer("projects")),
            },
            None => self.decision_prompt(&question),
        }
    }

    /// `true`: replace the target account with the source's.
    pub fn take_source_account(&mut self, target: &str, source: Option<&str>) -> Result<bool> {
        let question = format!(
            "Hedef başka bir Claude hesabıyla girişli ({target}; kaynak: {}). Hangisi kalsın?",
            source.unwrap_or("bilinmiyor")
        );
        match self.answers {
            Some(a) => match a.account.as_deref() {
                Some("keep_target") => Ok(false),
                Some("take_source") => Ok(true),
                Some(other) => bail!("geçersiz hesap yanıtı: {other}"),
                None => Err(missing_answer("account")),
            },
            None => {
                if !interactive() {
                    return Err(no_terminal(&question));
                }
                let picked = select(
                    &question,
                    &["Hedef hesabı koru", "Kaynak hesabıyla değiştir"],
                )?;
                Ok(picked == 1)
            }
        }
    }

    /// One of `candidates` for the project, or `None` for the pending store.
    pub fn choose_path(
        &mut self,
        project: &ProjectRef,
        candidates: &[PathBuf],
    ) -> Result<Option<PathBuf>> {
        let question = format!("{} hedefte birden çok yerde var. Hangisi?", project.id.0);
        match self.answers {
            Some(a) => match a.choose.get(&project.id.0).map(String::as_str) {
                Some("pending") => Ok(None),
                Some(path) => {
                    let path = PathBuf::from(path);
                    if !candidates.contains(&path) {
                        bail!("seçilen yol adaylar arasında değil: {}", path.display());
                    }
                    Ok(Some(path))
                }
                None => Err(missing_answer("choose")),
            },
            None => {
                if !interactive() {
                    return Err(no_terminal(&question));
                }
                let mut items: Vec<String> =
                    candidates.iter().map(|p| p.display().to_string()).collect();
                items.push("Bekleme alanına koy".to_owned());
                let labels: Vec<&str> = items.iter().map(String::as_str).collect();
                let picked = select(&question, &labels)?;
                Ok(candidates.get(picked).cloned())
            }
        }
    }

    pub fn file(&mut self, dest: &Path) -> Result<Resolution> {
        if let Some(r) = self.all_files {
            return Ok(r);
        }
        let question = format!("Hedefte farklı içerikli dosya var: {}", dest.display());
        match self.answers {
            Some(a) => match a.file.as_deref() {
                Some("source") => Ok(Resolution::KeepSource),
                Some("target") => Ok(Resolution::KeepTarget),
                Some("both") => Ok(Resolution::KeepBoth),
                Some(other) => bail!("geçersiz dosya yanıtı: {other}"),
                None => Err(missing_answer("file")),
            },
            None => {
                if !interactive() {
                    return Err(no_terminal(&question));
                }
                let choices = [
                    (Resolution::KeepSource, false),
                    (Resolution::KeepTarget, false),
                    (Resolution::KeepBoth, false),
                    (Resolution::KeepSource, true),
                    (Resolution::KeepTarget, true),
                    (Resolution::KeepBoth, true),
                ];
                let picked = select(
                    &question,
                    &[
                        "Kaynak",
                        "Hedef",
                        "İkisi de (kaynak .from-<makine> adıyla)",
                        "Hepsine kaynak",
                        "Hepsine hedef",
                        "Hepsine ikisi de",
                    ],
                )?;
                let (r, all) = choices[picked];
                if all {
                    self.all_files = Some(r);
                }
                Ok(r)
            }
        }
    }

    pub fn json(&mut self, c: &JsonConflict) -> Result<Resolution> {
        if let Some(r) = self.all_json {
            return Ok(r);
        }
        let question = format!("JSON anahtarı iki tarafta farklı: {}", c.path);
        match self.answers {
            Some(a) => match a.json.as_deref() {
                Some("source") => Ok(Resolution::KeepSource),
                Some("target") => Ok(Resolution::KeepTarget),
                Some(other) => bail!("geçersiz JSON yanıtı: {other}"),
                None => Err(missing_answer("json")),
            },
            None => {
                if !interactive() {
                    return Err(no_terminal(&question));
                }
                let picked = select(
                    &question,
                    &["Kaynak", "Hedef", "Hepsine kaynak", "Hepsine hedef"],
                )?;
                let r = if picked % 2 == 0 {
                    Resolution::KeepSource
                } else {
                    Resolution::KeepTarget
                };
                if picked >= 2 {
                    self.all_json = Some(r);
                }
                Ok(r)
            }
        }
    }

    /// Whether to write the suggested `cleanupPeriodDays`. Without a
    /// terminal (and no test answer) nothing is written.
    pub fn confirm_cleanup(&mut self, suggested: u64) -> Result<bool> {
        match self.answers {
            Some(a) => Ok(a.cleanup.unwrap_or(false)),
            None if interactive() => Confirm::new()
                .with_prompt(format!("cleanupPeriodDays {suggested} olarak yazılsın mı?"))
                .default(false)
                .interact()
                .context("onay alınamadı"),
            None => {
                eprintln!("Değer yazılmadı: onay için komutu bir terminalde çalıştırın.");
                Ok(false)
            }
        }
    }
}

/// Waits until `pids()` is empty: on a terminal the user closes the tool
/// and confirms; without one the import stops.
pub fn wait_until_closed(tool_name: &str, pids: &dyn Fn() -> Vec<u32>) -> Result<()> {
    loop {
        let running = pids();
        if running.is_empty() {
            return Ok(());
        }
        let list: Vec<String> = running.iter().map(u32::to_string).collect();
        let msg = format!(
            "{tool_name} çalışıyor (PID {}). Aktarımdan önce kapatılmalı.",
            list.join(", ")
        );
        if !interactive() {
            bail!("{msg} Kapatıp yeniden deneyin.");
        }
        eprintln!("uyarı: {msg}");
        let closed = Confirm::new()
            .with_prompt(format!("{tool_name} kapatıldı mı?"))
            .default(true)
            .interact()
            .context("onay alınamadı")?;
        if !closed {
            bail!("iptal edildi");
        }
    }
}
