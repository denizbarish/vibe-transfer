//! Lossless waiting area for data of projects without a target path.
//!
//! Layout: `<dir>/<first 16 hex of blake3(project id)>/pending.json` plus
//! `blobs/<put nonce>-<entry index>`. Directories are owner-only (0700),
//! `pending.json` is 0600; blobs keep their original mode and mtime.

use std::collections::HashSet;
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
    /// items are appended. Empty `items` is a no-op.
    ///
    /// On failure every blob already moved is rolled back to its source
    /// (renames are reversed, copies removed). A crash between moving blobs
    /// and writing `pending.json` can leave unreferenced blobs or a temp
    /// record behind; `list_orphans` reports them, so nothing is unreachable.
    pub fn put(&self, project: &ProjectRef, items: &[PendingItem]) -> Result<()> {
        if items.is_empty() {
            return Ok(());
        }
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
        let mut moved: Vec<Moved> = Vec::with_capacity(items.len());
        let result = (|| -> Result<()> {
            for item in items {
                let name = format!("{nonce}-{}", item.entry.index);
                let dst = blobs.join(&name);
                let renamed = move_blob(&item.blob, &dst)?;
                moved.push(Moved {
                    src: item.blob.clone(),
                    dst,
                    renamed,
                });
                stored.items.push(PendingItem {
                    entry: item.entry.clone(),
                    blob: Path::new(BLOBS_DIR).join(name),
                    dest_template: item.dest_template.clone(),
                });
            }
            write_json_atomic(&project_dir, &stored, &nonce)
        })();
        let Err(err) = result else { return Ok(()) };
        let rollback_errors = roll_back(&moved);
        // Leaves a folder created only for this call; fails harmlessly if
        // it holds anything.
        let _ = fs::remove_dir(&blobs);
        let _ = fs::remove_dir(&project_dir);
        if rollback_errors.is_empty() {
            Err(err)
        } else {
            Err(err.context(format!(
                "geri alma da başarısız oldu: {}",
                rollback_errors.join("; ")
            )))
        }
    }

    /// Files under the store that no `pending.json` references: blobs left
    /// by an interrupted `put` and leftover temp records. Sorted.
    pub fn list_orphans(&self) -> Result<Vec<PathBuf>> {
        let mut out = Vec::new();
        for project_dir in self.project_dirs()? {
            let referenced: HashSet<PathBuf> = match self.load(&project_dir)? {
                Some(project) => project.items.into_iter().map(|i| i.blob).collect(),
                None => HashSet::new(),
            };
            for path in read_dir_paths(&project_dir)? {
                let is_tmp = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with(&format!("{PENDING_JSON}.tmp-")));
                if is_tmp {
                    out.push(path);
                }
            }
            let blobs = project_dir.join(BLOBS_DIR);
            if blobs.is_dir() {
                for path in read_dir_paths(&blobs)? {
                    if !referenced.contains(&path) {
                        out.push(path);
                    }
                }
            }
        }
        out.sort();
        Ok(out)
    }

    /// Project folders under `dir`; none when `dir` does not exist.
    fn project_dirs(&self) -> Result<Vec<PathBuf>> {
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
            let ft = entry
                .file_type()
                .with_context(|| format!("okunamadı: {}", entry.path().display()))?;
            if ft.is_dir() {
                out.push(entry.path());
            }
        }
        Ok(out)
    }

    /// All pending projects, sorted by project id.
    pub fn list(&self) -> Result<Vec<PendingProject>> {
        let mut out = Vec::new();
        for project_dir in self.project_dirs()? {
            if let Some(project) = self.load(&project_dir)? {
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

/// A blob moved by the current `put`, kept for rollback.
struct Moved {
    src: PathBuf,
    dst: PathBuf,
    /// `true` when renamed (source gone), `false` when copied.
    renamed: bool,
}

/// Undoes `moved` in reverse order; returns the errors it hit.
fn roll_back(moved: &[Moved]) -> Vec<String> {
    let mut errors = Vec::new();
    for m in moved.iter().rev() {
        let result = if m.renamed {
            fs::rename(&m.dst, &m.src)
        } else {
            fs::remove_file(&m.dst)
        };
        if let Err(e) = result {
            errors.push(format!("{}: {e}", m.dst.display()));
        }
    }
    errors
}

fn read_dir_paths(dir: &Path) -> Result<Vec<PathBuf>> {
    let entries = fs::read_dir(dir).with_context(|| format!("okunamadı: {}", dir.display()))?;
    let mut out = Vec::new();
    for entry in entries {
        out.push(
            entry
                .with_context(|| format!("okunamadı: {}", dir.display()))?
                .path(),
        );
    }
    Ok(out)
}

/// Moves `src` to `dst` by rename (returns `true`); across file systems
/// copies instead, keeping mode and mtime, and leaves `src` in place
/// (returns `false`).
fn move_blob(src: &Path, dst: &Path) -> Result<bool> {
    match fs::rename(src, dst) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == ErrorKind::CrossesDevices => {
            if let Err(e) = copy_blob(src, dst) {
                // Never leave a partial copy behind.
                let _ = fs::remove_file(dst);
                return Err(e);
            }
            Ok(false)
        }
        Err(e) => Err(e).with_context(|| format!("taşınamadı: {}", src.display())),
    }
}

fn copy_blob(src: &Path, dst: &Path) -> Result<()> {
    let md = fs::symlink_metadata(src).with_context(|| format!("okunamadı: {}", src.display()))?;
    let mtime = filetime::FileTime::from_last_modification_time(&md);
    if md.file_type().is_symlink() {
        copy_symlink(src, dst)?;
        filetime::set_symlink_file_times(dst, mtime, mtime)
            .with_context(|| format!("zaman ayarlanamadı: {}", dst.display()))?;
        return Ok(());
    }
    if !md.is_file() {
        bail!("yalnız dosya veya sembolik bağ beklenir: {}", src.display());
    }
    // `fs::copy` keeps the permission bits.
    fs::copy(src, dst).with_context(|| format!("kopyalanamadı: {}", src.display()))?;
    filetime::set_file_mtime(dst, mtime)
        .with_context(|| format!("zaman ayarlanamadı: {}", dst.display()))?;
    // Durable before `pending.json` references it.
    fs::File::open(dst)
        .and_then(|f| f.sync_all())
        .with_context(|| format!("diske yazılamadı: {}", dst.display()))?;
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
        #[cfg(test)]
        if tests::FAIL_JSON_WRITE.get() {
            bail!("test: yazma hatası");
        }
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

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;
    use crate::manifest::{Category, EntryKind, RootKind, Tool};

    thread_local! {
        pub(super) static FAIL_JSON_WRITE: Cell<bool> = const { Cell::new(false) };
    }

    fn project() -> ProjectRef {
        ProjectRef {
            id: ProjectId("path:/private/tmp/acme".into()),
            old_path: "/private/tmp/acme".into(),
            remote: None,
            is_git: false,
        }
    }

    fn item(dir: &Path, index: u32, body: &[u8]) -> PendingItem {
        let blob = dir.join(format!("src-{index}"));
        fs::write(&blob, body).unwrap();
        PendingItem {
            entry: Entry {
                index,
                tool: Tool::Claude,
                category: Category::Sessions,
                root: RootKind::ClaudeDir,
                rel: format!("{index}.jsonl"),
                kind: EntryKind::File,
                size: body.len() as u64,
                mtime_ns: 0,
                mode: 0o600,
                hash: None,
                partial_tail: false,
            },
            blob,
            dest_template: DestTemplate::ProjectRel(format!("{index}.jsonl")),
        }
    }

    #[test]
    fn failed_json_write_rolls_blobs_back() {
        let tmp = tempfile::tempdir().unwrap();
        let store = PendingStore {
            dir: tmp.path().join("pending"),
        };
        // Existing record must survive a failed second put untouched.
        store
            .put(&project(), &[item(tmp.path(), 0, b"keep")])
            .unwrap();

        let items = [item(tmp.path(), 1, b"one"), item(tmp.path(), 2, b"two")];
        FAIL_JSON_WRITE.set(true);
        let result = store.put(&project(), &items);
        FAIL_JSON_WRITE.set(false);
        assert!(result.is_err());

        for it in &items {
            assert!(it.blob.exists(), "source restored: {}", it.blob.display());
        }
        let listed = store.list().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].items.len(), 1);
        assert_eq!(fs::read(&listed[0].items[0].blob).unwrap(), b"keep");
        assert!(store.list_orphans().unwrap().is_empty());
    }

    #[test]
    fn failed_first_put_leaves_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let store = PendingStore {
            dir: tmp.path().join("pending"),
        };
        let it = item(tmp.path(), 0, b"x");
        FAIL_JSON_WRITE.set(true);
        assert!(store.put(&project(), std::slice::from_ref(&it)).is_err());
        FAIL_JSON_WRITE.set(false);
        assert!(it.blob.exists());
        assert!(store.list().unwrap().is_empty());
        assert_eq!(read_dir_paths(&store.dir).unwrap().len(), 0);
    }

    #[test]
    fn missing_source_rolls_back_earlier_moves() {
        let tmp = tempfile::tempdir().unwrap();
        let store = PendingStore {
            dir: tmp.path().join("pending"),
        };
        let ok = item(tmp.path(), 0, b"x");
        let mut missing = item(tmp.path(), 1, b"y");
        missing.blob = tmp.path().join("absent");
        assert!(store.put(&project(), &[ok.clone(), missing]).is_err());
        assert!(ok.blob.exists());
        assert!(store.list().unwrap().is_empty());
        assert!(store.list_orphans().unwrap().is_empty());
    }

    #[test]
    fn crash_leftovers_are_listed_as_orphans() {
        let tmp = tempfile::tempdir().unwrap();
        let store = PendingStore {
            dir: tmp.path().join("pending"),
        };
        store.put(&project(), &[item(tmp.path(), 0, b"a")]).unwrap();
        // Simulate a crash in a later put and one in a new project.
        let project_dir = store.project_dir(&project().id);
        let stray = project_dir.join(BLOBS_DIR).join("ffff-9");
        let tmp_record = project_dir.join(format!("{PENDING_JSON}.tmp-ffff"));
        fs::write(&stray, b"lost").unwrap();
        fs::write(&tmp_record, b"{").unwrap();
        let other = store.dir.join("0123456789abcdef").join(BLOBS_DIR);
        fs::create_dir_all(&other).unwrap();
        fs::write(other.join("eeee-0"), b"lost too").unwrap();

        let mut expected = vec![stray, tmp_record, other.join("eeee-0")];
        expected.sort();
        assert_eq!(store.list_orphans().unwrap(), expected);
        assert_eq!(store.list().unwrap().len(), 1);
    }

    #[test]
    fn empty_put_is_noop() {
        let tmp = tempfile::tempdir().unwrap();
        let store = PendingStore {
            dir: tmp.path().join("pending"),
        };
        store.put(&project(), &[]).unwrap();
        assert!(!store.dir.exists());
    }
}
