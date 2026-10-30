//! `vibe-transfer export`: writes the selected data to an encrypted bundle
//! and records the session in `<vt_home>/outbox/<session>/meta.json`.

use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use indicatif::{HumanBytes, ProgressBar};
use serde::Serialize;
use serde_json::Value;
use vt_claude::{ClaudeAdapter, item_key, read_claude_json_stable};
use vt_core::bundle::{BundleWriter, SecretString, generate_passphrase};
use vt_core::fsutil::{create_private_dir_all, create_private_file};
use vt_core::manifest::{
    Category, Entry, EntryKind, FORMAT_VERSION, Manifest, RootKind, SourceInfo, Tool,
};
use vt_core::scan::{UNREADABLE_DIR, VANISHED};
use vt_core::secret_types::{SecretItem, SecretKind, SecretMeta};
use vt_core::session::{new_session_id, utc_now_rfc3339};
use vt_secrets::claude_service_name;

use super::{Ctx, SelectionFlags, resolve_selection};
use crate::{ExportArgs, ui};

/// Backwards read size when looking for the last newline of a jsonl file.
const TAIL_CHUNK: u64 = 64 * 1024;

/// `<vt_home>/outbox/<session>/meta.json`.
#[derive(Serialize)]
struct OutboxMeta<'a> {
    session_id: &'a str,
    created_at: &'a str,
    secrets: &'a [SecretMeta],
    /// Secret store service of the exported Claude login, so
    /// `logout-source` removes exactly that item.
    secret_service: Option<String>,
    out_path: String,
}

#[derive(Serialize)]
struct Item {
    path: String,
    reason: String,
}

#[derive(Serialize)]
struct Summary<'a> {
    test_mode: bool,
    session_id: &'a str,
    out_path: String,
    entries: usize,
    files: usize,
    bytes: u64,
    bundle_bytes: u64,
    secrets: usize,
    skipped: Vec<Item>,
    notes: Vec<Item>,
}

/// Removes a half-written bundle unless the export got to the end.
struct PartialBundle<'a> {
    path: &'a Path,
    done: bool,
}

impl Drop for PartialBundle<'_> {
    fn drop(&mut self) {
        if !self.done {
            let _ = fs::remove_file(self.path);
        }
    }
}

pub fn run(ctx: &Ctx, args: &ExportArgs) -> Result<()> {
    let sel = resolve_selection(&SelectionFlags {
        tool: args.tool,
        only: &args.only,
        exclude: &args.exclude,
        include_optional: args.include_optional,
    })?;
    // Everything that can fail fast does so before any data is read.
    let given = if args.passphrase_stdin {
        Some(ui::read_passphrase_stdin(true, ui::BUNDLE_PASS_LABEL)?)
    } else {
        ui::ensure_passphrase_display()?;
        None
    };
    if fs::symlink_metadata(&args.out).is_ok() {
        bail!("dosya zaten var: {}", args.out.display());
    }
    if let Some(notice) = ctx.test_mode_notice() {
        eprintln!("{notice}");
    }
    ui::confirm_while_running("Claude", &vt_core::procs::running(Tool::Claude))?;

    let adapter = ClaudeAdapter {
        env: ctx.env.clone(),
    };
    let inv = adapter.inventory(&sel)?;
    let mut skipped: Vec<(String, String)> = inv.skipped;
    let notes: Vec<(String, String)> = inv.notes;
    let mut entries = inv.entries;

    // `~/.claude.json` is rewritten while Claude runs: take a parsed
    // snapshot instead of streaming the file.
    let wants_json = entries.iter().any(|(e, _)| e.root == RootKind::ClaudeJson);
    let wants_secret = sel.includes(Tool::Claude, Category::Secrets);
    let claude_json = if wants_json || wants_secret {
        match read_claude_json_stable(&ctx.env.claude_json) {
            Ok(v) => Some(v),
            Err(e) => {
                if wants_json {
                    entries.retain(|(e, _)| e.root != RootKind::ClaudeJson);
                    skipped.push((
                        item_key(&RootKind::ClaudeJson, ""),
                        format!("{UNREADABLE_DIR}: {e:#}"),
                    ));
                }
                None
            }
        }
    } else {
        None
    };
    let json_bytes = match (&claude_json, wants_json) {
        (Some(v), true) => Some(serde_json::to_vec_pretty(v).context("JSON yazılamadı")?),
        _ => None,
    };

    let secrets = if wants_secret {
        read_claude_secret(ctx, claude_json.as_ref(), &mut skipped)?
    } else {
        Vec::new()
    };

    let pass: SecretString = given.clone().unwrap_or_else(generate_passphrase);
    let session_id = new_session_id();
    let created_at = utc_now_rfc3339();

    let total: u64 = entries.iter().map(|(e, _)| e.size).sum();
    let bar = ui::progress(total, args.json);
    let mut writer = BundleWriter::create(&args.out, &pass)?;
    // Armed only once the file is ours: `create` never opens an existing one.
    let mut guard = PartialBundle {
        path: &args.out,
        done: false,
    };
    let mut kept = Vec::with_capacity(entries.len());
    for (mut entry, path) in entries {
        if entry.kind != EntryKind::File {
            kept.push(entry);
            continue;
        }
        let outcome = match (&entry.root, &json_bytes) {
            (RootKind::ClaudeJson, Some(bytes)) => {
                add_bytes(&mut writer, &mut entry, bytes, &bar).map(|()| Added::Whole)
            }
            _ => add_from_disk(&mut writer, &mut entry, &path, &bar),
        };
        match outcome? {
            Added::Whole => kept.push(entry),
            Added::TailCut(bytes) => {
                skipped.push((item_key(&entry.root, &entry.rel), tail_cut_reason(bytes)));
                kept.push(entry);
            }
            Added::Skipped(reason) => skipped.push((item_key(&entry.root, &entry.rel), reason)),
        }
    }
    for item in &secrets {
        writer.add_secret(item)?;
    }
    bar.finish_and_clear();

    let secret_metas: Vec<SecretMeta> = secrets.iter().map(|s| s.meta.clone()).collect();
    let secret_service = secrets
        .iter()
        .find(|s| s.meta.kind == SecretKind::ClaudeKeychain)
        .map(|s| s.service.clone());
    drop(secrets);
    let mut manifest_notes: Vec<String> = skipped
        .iter()
        .map(|(k, r)| format!("atlandı: {k}: {r}"))
        .collect();
    manifest_notes.extend(notes.iter().map(|(k, r)| format!("not: {k}: {r}")));
    let manifest = Manifest {
        format_version: FORMAT_VERSION,
        session_id: session_id.clone(),
        created_at: created_at.clone(),
        source: SourceInfo {
            os: std::env::consts::OS.to_owned(),
            user: ctx.env.user.clone(),
            home: ctx.env.home.to_string_lossy().into_owned(),
            hostname: vt_core::procs::host_name().unwrap_or_default(),
            tool_versions: Default::default(),
        },
        selection: sel,
        entries: kept,
        projects: inv.projects,
        secrets: secret_metas,
        notes: manifest_notes,
    };
    let written = writer.finish(&manifest)?;
    guard.done = true;

    // The bundle is complete: show the passphrase before anything else can
    // fail, or the bundle would be locked with a passphrase nobody saw.
    if given.is_none() {
        ui::show_passphrase(&pass);
    }
    ui::warn_separate_channel();

    let out_path = fs::canonicalize(&args.out)
        .with_context(|| format!("paket yolu çözülemedi: {}", args.out.display()))
        .and_then(|out_path| {
            write_outbox(
                &ctx.env.vt_home,
                &OutboxMeta {
                    session_id: &session_id,
                    created_at: &created_at,
                    secrets: &manifest.secrets,
                    secret_service,
                    out_path: out_path.to_string_lossy().into_owned(),
                },
            )?;
            Ok(out_path)
        })
        .with_context(|| {
            let pass_note = if given.is_none() {
                "parola yukarıda gösterildi"
            } else {
                "verilen parolayla açılır"
            };
            format!(
                "paket geçerli ve kullanılabilir ({}; {pass_note}), ancak oturum kaydı \
                 (outbox) yazılamadı; logout-source bu oturum için çalışmaz",
                args.out.display()
            )
        })?;

    let files: Vec<&Entry> = manifest
        .entries
        .iter()
        .filter(|e| e.kind == EntryKind::File)
        .collect();
    let to_items = |v: &[(String, String)]| -> Vec<Item> {
        v.iter()
            .map(|(path, reason)| Item {
                path: path.clone(),
                reason: reason.clone(),
            })
            .collect()
    };
    let summary = Summary {
        test_mode: ctx.test_mode,
        session_id: &session_id,
        out_path: out_path.to_string_lossy().into_owned(),
        entries: manifest.entries.len(),
        files: files.len(),
        bytes: files.iter().map(|e| e.size).sum(),
        bundle_bytes: written.bytes,
        secrets: manifest.secrets.len(),
        skipped: to_items(&skipped),
        notes: to_items(&notes),
    };
    if args.json {
        println!("{}", serde_json::to_string_pretty(&summary)?);
    } else {
        print_human(&summary, &skipped, &notes);
    }
    Ok(())
}

fn print_human(s: &Summary, skipped: &[(String, String)], notes: &[(String, String)]) {
    println!("Paket yazıldı: {}", s.out_path);
    println!("Oturum: {}", s.session_id);
    println!(
        "Aktarılan: {} öğe ({} dosya, {}), {} sır; paket {}",
        s.entries,
        s.files,
        HumanBytes(s.bytes),
        s.secrets,
        HumanBytes(s.bundle_bytes)
    );
    ui::print_grouped("Atlananlar", skipped);
    ui::print_grouped("Notlar", notes);
}

/// The Claude Keychain item, when there is one. A missing or unreadable
/// item is reported in `skipped` and the export goes on without it.
fn read_claude_secret(
    ctx: &Ctx,
    claude_json: Option<&Value>,
    skipped: &mut Vec<(String, String)>,
) -> Result<Vec<SecretItem>> {
    let service = claude_service_name(ctx.env.claude_config_dir_override.as_deref());
    let account = ctx.env.user.clone();
    let key = format!("keychain:{service}");
    let value = match ctx.store.get(&service, &account) {
        Ok(Some(v)) => v,
        Ok(None) => {
            skipped.push((key, "Claude giriş kaydı bulunamadı".to_owned()));
            return Ok(Vec::new());
        }
        Err(e) => {
            skipped.push((key, format!("{UNREADABLE_DIR}: {e:#}")));
            return Ok(Vec::new());
        }
    };
    let account_field = |name: &str| {
        claude_json
            .and_then(|v| v.get("oauthAccount"))
            .and_then(|a| a.get(name))
            .and_then(Value::as_str)
            .map(str::to_owned)
    };
    Ok(vec![SecretItem {
        meta: SecretMeta {
            tool: Tool::Claude,
            kind: SecretKind::ClaudeKeychain,
            account_uuid: account_field("accountUuid"),
            email: account_field("emailAddress"),
        },
        service,
        account_hint: account,
        value,
    }])
}

/// Adds an in-memory blob (the `~/.claude.json` snapshot).
fn add_bytes(
    writer: &mut BundleWriter,
    entry: &mut Entry,
    bytes: &[u8],
    bar: &ProgressBar,
) -> Result<()> {
    let mut reader = Counting {
        inner: bytes,
        count: 0,
        bar,
    };
    entry.hash = Some(writer.add_file(entry.index, &mut reader)?);
    entry.size = reader.count;
    Ok(())
}

/// What happened to one file streamed into the bundle.
enum Added {
    /// Every byte was written.
    Whole,
    /// Written up to its last newline; this many trailing bytes were not.
    TailCut(u64),
    /// Not written, with the reason.
    Skipped(String),
}

/// Skip reason for the unfinished last line of a cut `.jsonl` file.
fn tail_cut_reason(bytes: u64) -> String {
    format!("son satır tamamlanmamış, {bytes} bayt alınmadı")
}

/// Streams one file into the bundle. `.jsonl` files are cut after their
/// last newline (a line still being written is dropped, `partial_tail`
/// set, the dropped byte count returned). `size` and `hash` describe
/// exactly the bytes written. Returns the skip reason when the file
/// vanished or cannot be opened; a read error mid-file fails the export,
/// since the writer cannot recover from it.
fn add_from_disk(
    writer: &mut BundleWriter,
    entry: &mut Entry,
    path: &Path,
    bar: &ProgressBar,
) -> Result<Added> {
    let mut file = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Ok(Added::Skipped(VANISHED.to_owned()));
        }
        Err(e) => return Ok(Added::Skipped(format!("{UNREADABLE_DIR}: {e}"))),
    };
    let md = file
        .metadata()
        .with_context(|| format!("metadata {}", path.display()))?;
    if !md.is_file() {
        return Ok(Added::Skipped(format!(
            "{UNREADABLE_DIR}: tarama sırasında türü değişti"
        )));
    }
    let len = md.len();
    let limit = if entry.rel.ends_with(".jsonl") {
        let cut = last_line_end(&mut file, len)
            .with_context(|| format!("okunamadı: {}", path.display()))?;
        entry.partial_tail = cut < len;
        file.seek(SeekFrom::Start(0))?;
        cut
    } else {
        len
    };
    let mut reader = Counting {
        inner: file.take(limit),
        count: 0,
        bar,
    };
    entry.hash = Some(
        writer
            .add_file(entry.index, &mut reader)
            .with_context(|| format!("pakete eklenemedi: {}", path.display()))?,
    );
    entry.size = reader.count;
    Ok(if limit < len {
        Added::TailCut(len - limit)
    } else {
        Added::Whole
    })
}

/// Offset just past the last `\n` in the first `len` bytes; 0 if none.
fn last_line_end(file: &mut File, len: u64) -> io::Result<u64> {
    let mut end = len;
    let mut buf = Vec::new();
    while end > 0 {
        let start = end.saturating_sub(TAIL_CHUNK);
        file.seek(SeekFrom::Start(start))?;
        buf.clear();
        Read::by_ref(file).take(end - start).read_to_end(&mut buf)?;
        if let Some(i) = buf.iter().rposition(|b| *b == b'\n') {
            return Ok(start + i as u64 + 1);
        }
        end = start;
    }
    Ok(0)
}

/// Counts bytes read and advances the progress bar.
struct Counting<'a, R> {
    inner: R,
    count: u64,
    bar: &'a ProgressBar,
}

impl<R: Read> Read for Counting<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.count += n as u64;
        self.bar.inc(n as u64);
        Ok(n)
    }
}

fn write_outbox(vt_home: &Path, meta: &OutboxMeta) -> Result<PathBuf> {
    let dir = vt_home.join("outbox").join(meta.session_id);
    create_private_dir_all(&dir)
        .with_context(|| format!("klasör oluşturulamadı: {}", dir.display()))?;
    let path = dir.join("meta.json");
    let mut file = create_private_file(&path)
        .with_context(|| format!("dosya oluşturulamadı: {}", path.display()))?;
    let json = serde_json::to_vec_pretty(meta)?;
    file.write_all(&json)?;
    file.sync_all()?;
    Ok(path)
}
