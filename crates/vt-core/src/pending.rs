//! Lossless waiting area for data of projects without a target path.
//!
//! Layout: `<dir>/<first 16 hex of blake3(project id)>/pending.json` plus
//! `blobs/<put nonce>-<entry index>`. Directories are owner-only (0700),
//! `pending.json` is 0600; blobs keep their original mode and mtime.

use std::fs;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use rand::RngCore;
use serde::{Deserialize, Serialize};

use crate::fsutil::{create_private_dir_all, create_private_file};
use crate::manifest::{Entry, ProjectId, ProjectRef};

const PENDING_JSON: &str = "pending.json";
const BLOBS_DIR: &str = "blobs";

/// Computes an item's target location once the project root is known.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DestTemplate {
    /// Path relative to the project root.
    ProjectRel(String),
    /// Path inside `<claude dir>/projects/<slug of the project root>/`.
    ClaudeProjectSlugDir { rel_in_slug: String },
    /// The project's value under `projects` in `~/.claude.json`.
    ClaudeJsonProjectsKey,
}

/// One waiting entry. `blob` is the content file: the staged source on
/// `put`, the stored copy in results of `list` and `take`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingItem {
    pub entry: Entry,
    pub blob: PathBuf,
    pub dest_template: DestTemplate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingProject {
    pub project: ProjectRef,
    pub items: Vec<PendingItem>,
}

/// Pending store rooted at `dir` (normally `<vt_home>/pending`).
#[derive(Debug, Clone)]
pub struct PendingStore {
    pub dir: PathBuf,
}

impl PendingStore {
    /// Adds `items` for `project`, moving each blob into the store (renamed
    /// on the same file system; otherwise copied and the source left for
    /// the caller). Existing pending data of the project is kept and the new
    /// items are appended.
    pub fn put(&self, project: &ProjectRef, items: &[PendingItem]) -> Result<()> {
        let project_dir = self.project_dir(&project.id);
        let blobs = project_dir.join(BLOBS_DIR);
        create_private_dir_all(&blobs)
            .with_context(|| format!("bekleme klasörü oluşturulamadı: {}", blobs.display()))?;

        let mut stored = match self.load(&project_dir)? {
            Some(existing) => existing,
            None => PendingProject {
                project: project.clone(),
                items: Vec::new(),
            },
        };
        if stored.project.id != project.id {
            bail!(
                "bekleme klasörü başka bir projeye ait: {}",
                project_dir.display()
            );
        }

        let nonce = nonce_hex();
        for (moved, item) in items.iter().enumerate() {
            let name = format!("{nonce}-{}", item.entry.index);
            if let Err(e) = move_blob(&item.blob, &blobs.join(&name)) {
                // Record the blobs already moved so none is orphaned.
                if moved > 0 {
                    write_json_atomic(&project_dir, &stored, &nonce)?;
                }
                return Err(e);
            }
            stored.items.push(PendingItem {
                entry: item.entry.clone(),
                blob: Path::new(BLOBS_DIR).join(name),
                dest_template: item.dest_template.clone(),
            });
        }
        write_json_atomic(&project_dir, &stored, &nonce)
    }

    /// All pending projects, sorted by project id.
    pub fn list(&self) -> Result<Vec<PendingProject>> {
        let entries = match fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => {
                return Err(e)
                    .with_context(|| format!("bekleme alanı okunamadı: {}", self.dir.display()));
            }
        };
        let mut out = Vec::new();
        for entry in entries {
            let entry = entry
                .with_context(|| format!("bekleme alanı okunamadı: {}", self.dir.display()))?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            if let Some(project) = self.load(&entry.path())? {
                out.push(project);
            }
        }
        out.sort_by(|a, b| a.project.id.cmp(&b.project.id));
        Ok(out)
    }

    /// Returns the pending data of `id` without deleting it; call `remove`
    /// once the data has been applied.
    pub fn take(&self, id: &ProjectId) -> Result<PendingProject> {
        let project_dir = self.project_dir(id);
        match self.load(&project_dir)? {
            Some(project) if project.project.id == *id => Ok(project),
            _ => Err(anyhow!("bu proje için bekleyen veri yok: {}", id.0)),
        }
    }

    /// Deletes the pending data of `id`. Missing data is not an error.
    pub fn remove(&self, id: &ProjectId) -> Result<()> {
        let project_dir = self.project_dir(id);
        match fs::remove_dir_all(&project_dir) {
            Err(e) if e.kind() != ErrorKind::NotFound => Err(e)
                .with_context(|| format!("bekleme klasörü silinemedi: {}", project_dir.display())),
            _ => Ok(()),
        }
    }

    fn project_dir(&self, id: &ProjectId) -> PathBuf {
        let hash = blake3::hash(id.0.as_bytes()).to_hex();
        self.dir.join(&hash.as_str()[..16])
    }

    /// Reads `pending.json` of `project_dir`, resolving blob paths.
    fn load(&self, project_dir: &Path) -> Result<Option<PendingProject>> {
        let path = project_dir.join(PENDING_JSON);
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
            Err(e) => {
                return Err(e).with_context(|| format!("okunamadı: {}", path.display()));
            }
        };
        let mut project: PendingProject = serde_json::from_slice(&bytes)
            .with_context(|| format!("bozuk bekleme kaydı: {}", path.display()))?;
        for item in &mut project.items {
            item.blob = project_dir.join(&item.blob);
        }
        Ok(Some(project))
    }
}

fn nonce_hex() -> String {
    let mut bytes = [0u8; 8];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Moves `src` to `dst` by rename; across file systems copies instead,
/// keeping mode and mtime, and leaves `src` in place.
fn move_blob(src: &Path, dst: &Path) -> Result<()> {
    match fs::rename(src, dst) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == ErrorKind::CrossesDevices => copy_blob(src, dst),
        Err(e) => Err(e).with_context(|| format!("taşınamadı: {}", src.display())),
    }
}

fn copy_blob(src: &Path, dst: &Path) -> Result<()> {
    let md = fs::symlink_metadata(src).with_context(|| format!("okunamadı: {}", src.display()))?;
    if md.file_type().is_symlink() {
        copy_symlink(src, dst)?;
        let mtime = filetime::FileTime::from_last_modification_time(&md);
        filetime::set_symlink_file_times(dst, mtime, mtime)
            .with_context(|| format!("zaman ayarlanamadı: {}", dst.display()))?;
        return Ok(());
    }
    if !md.is_file() {
        bail!("yalnız dosya veya sembolik bağ beklenir: {}", src.display());
    }
    // `fs::copy` keeps the permission bits.
    fs::copy(src, dst).with_context(|| format!("kopyalanamadı: {}", src.display()))?;
    let mtime = filetime::FileTime::from_last_modification_time(&md);
    filetime::set_file_mtime(dst, mtime)
        .with_context(|| format!("zaman ayarlanamadı: {}", dst.display()))?;
    Ok(())
}

#[cfg(unix)]
fn copy_symlink(src: &Path, dst: &Path) -> Result<()> {
    let target = fs::read_link(src).with_context(|| format!("okunamadı: {}", src.display()))?;
    std::os::unix::fs::symlink(target, dst)
        .with_context(|| format!("oluşturulamadı: {}", dst.display()))
}

#[cfg(not(unix))]
fn copy_symlink(src: &Path, _dst: &Path) -> Result<()> {
    bail!("sembolik bağ kopyalanamıyor: {}", src.display())
}

/// Writes `pending.json` through a private temp file and a rename, so a
/// crash leaves either the old or the new record.
fn write_json_atomic(project_dir: &Path, project: &PendingProject, nonce: &str) -> Result<()> {
    let tmp = project_dir.join(format!("{PENDING_JSON}.tmp-{nonce}"));
    let path = project_dir.join(PENDING_JSON);
    let result = (|| -> Result<()> {
        let mut file = create_private_file(&tmp)?;
        serde_json::to_writer_pretty(&mut file, project)?;
        file.flush()?;
        file.sync_all()?;
        fs::rename(&tmp, &path)?;
        Ok(())
    })();
    if let Err(e) = result {
        let _ = fs::remove_file(&tmp);
        return Err(e).with_context(|| format!("yazılamadı: {}", path.display()));
    }
    // Persist the rename itself.
    if let Ok(dir) = fs::File::open(project_dir) {
        let _ = dir.sync_all();
    }
    Ok(())
}
