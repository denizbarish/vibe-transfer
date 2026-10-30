//! Lossless waiting area for data of projects without a target path.
//!
//! Layout: `<dir>/<first 16 hex of blake3(project id)>/pending.json` plus
//! `blobs/<put nonce>-<position in the put>`. Directories are owner-only (0700),
//! `pending.json` is 0600; blobs keep their original mode and mtime.
//! `pending.json` stores blob paths relative to its folder; a record that
//! points anywhere else is refused.

use std::collections::HashSet;
use std::fs;
use std::io::{self, ErrorKind, Write};
use std::path::{Component, Path, PathBuf};

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
    /// Path inside `<claude dir>/projects/<slug>/`, where the slug is that of
    /// the project root, or of `<root>/<subpath>` for sessions started in a
    /// subfolder of the project.
    ClaudeProjectSlugDir {
        rel_in_slug: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subpath: Option<String>,
    },
    /// The project's value under `projects` in `~/.claude.json`.
    ClaudeJsonProjectsKey,
    /// The project's paths in `githubRepoPaths` of `~/.claude.json`: the blob
    /// holds `{ repo: [old path, ..] }`.
    ClaudeJsonRepoPaths,
}

/// One waiting entry. `blob` is the content file: the staged source on
/// `put`, the stored copy in results of `list` and `take`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingItem {
    pub entry: Entry,
    pub blob: PathBuf,
    pub dest_template: DestTemplate,
    /// Home of the source machine, for the path rewrite once the project is
    /// linked. `None` in records written before it was stored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_home: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingProject {
    pub project: ProjectRef,
    pub items: Vec<PendingItem>,
}

/// A `pending.json` that could not be read. Its folder is left as it is:
/// the record may still own the blobs beside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CorruptRecord {
    pub path: PathBuf,
    pub error: String,
}

/// What enumerating the store found, plus the records it could not read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listing<T> {
    pub items: Vec<T>,
    pub corrupt: Vec<CorruptRecord>,
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
            // Named by position: items of one put may share an entry index
            // (e.g. the `projects` and `githubRepoPaths` parts of one
            // `~/.claude.json` entry).
            for (pos, item) in items.iter().enumerate() {
                let name = format!("{nonce}-{pos}");
                let dst = blobs.join(&name);
                let renamed = move_blob(&item.blob, &dst)?;
                moved.push(Moved {
                    src: item.blob.clone(),
                    dst,
                    renamed,
                });
                stored.items.push(PendingItem {
                    entry: item.entry.clone(),
                    blob: project_dir.join(BLOBS_DIR).join(name),
                    dest_template: item.dest_template.clone(),
                    source_home: item.source_home.clone(),
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
    /// by an interrupted `put` and leftover temp records. Sorted. A folder
    /// whose record cannot be read is reported in `corrupt` and its files
    /// are not listed.
    pub fn list_orphans(&self) -> Result<Listing<PathBuf>> {
        let mut out = Vec::new();
        let mut corrupt = Vec::new();
        for project_dir in self.project_dirs()? {
            let referenced: HashSet<PathBuf> = match self.load(&project_dir) {
                Ok(Some(project)) => project.items.into_iter().map(|i| i.blob).collect(),
                Ok(None) => HashSet::new(),
                Err(e) => {
                    corrupt.push(corrupt_record(&project_dir, &e));
                    continue;
                }
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
        Ok(Listing {
            items: out,
            corrupt,
        })
    }

    /// Removes those of `paths` that are orphans right now (see
    /// `list_orphans`); anything else is left alone. Folders left empty
    /// with no record are removed too. Returns what was removed.
    pub fn remove_orphans(&self, paths: &[PathBuf]) -> Result<Vec<PathBuf>> {
        let current: HashSet<PathBuf> = self.list_orphans()?.items.into_iter().collect();
        let mut removed = Vec::new();
        for path in paths.iter().filter(|p| current.contains(*p)) {
            fs::remove_file(path).with_context(|| format!("silinemedi: {}", path.display()))?;
            removed.push(path.clone());
        }
        for project_dir in self.project_dirs()? {
            if !project_dir.join(PENDING_JSON).exists() {
                // Each fails harmlessly when the folder still holds anything.
                let _ = fs::remove_dir(project_dir.join(BLOBS_DIR));
                let _ = fs::remove_dir(&project_dir);
            }
        }
        Ok(removed)
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

    /// All pending projects, sorted by project id. One unreadable record
    /// does not hide the others: it is reported in `corrupt`.
    pub fn list(&self) -> Result<Listing<PendingProject>> {
        let mut out = Vec::new();
        let mut corrupt = Vec::new();
        for project_dir in self.project_dirs()? {
            match self.load(&project_dir) {
                Ok(Some(project)) => out.push(project),
                Ok(None) => {}
                Err(e) => corrupt.push(corrupt_record(&project_dir, &e)),
            }
        }
        out.sort_by(|a, b| a.project.id.cmp(&b.project.id));
        corrupt.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(Listing {
            items: out,
            corrupt,
        })
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

    /// Moves the pending data of `id` into `into` (created owner-only), for
    /// `restore` to bring back. The record is rewritten with relative blob
    /// paths first, so the moved folder stays readable.
    pub fn archive(&self, id: &ProjectId, into: &Path) -> Result<()> {
        let project = self.take(id)?;
        let project_dir = self.project_dir(id);
        // Older records hold absolute blob paths, which break once the
        // folder moves: store the relative form first.
        write_json_atomic(&project_dir, &project, &nonce_hex())?;
        create_private_dir_all(into)
            .with_context(|| format!("oluşturulamadı: {}", into.display()))?;
        let name = project_dir
            .file_name()
            .context("bekleme klasörünün adı yok")?;
        let dest = into.join(name);
        if fs::symlink_metadata(&dest).is_ok() {
            bail!("arşivde zaten var: {}", dest.display());
        }
        fs::rename(&project_dir, &dest)
            .with_context(|| format!("arşive taşınamadı: {}", project_dir.display()))
    }

    /// Moves every project archived in `from` back into the store. A
    /// project that has pending data again is not overwritten: that is an
    /// error and its archive stays. A missing `from` restores nothing.
    pub fn restore(&self, from: &Path) -> Result<Vec<ProjectId>> {
        let dirs = match fs::read_dir(from) {
            Ok(_) => read_dir_paths(from)?,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e).with_context(|| format!("okunamadı: {}", from.display())),
        };
        let mut out = Vec::new();
        for dir in dirs.into_iter().filter(|d| d.is_dir()) {
            let Some(project) = self.load(&dir)? else {
                continue;
            };
            let dest = self.project_dir(&project.project.id);
            if dir.file_name() != dest.file_name() {
                bail!("arşiv klasörü kaydıyla uyuşmuyor: {}", dir.display());
            }
            if fs::symlink_metadata(&dest).is_ok() {
                bail!(
                    "bu proje için bekleme alanında yeni veri var, arşiv yerinde bırakıldı: {}",
                    project.project.id.0
                );
            }
            create_private_dir_all(&self.dir)
                .with_context(|| format!("oluşturulamadı: {}", self.dir.display()))?;
            fs::rename(&dir, &dest)
                .with_context(|| format!("geri taşınamadı: {}", dir.display()))?;
            out.push(project.project.id);
        }
        Ok(out)
    }

    fn project_dir(&self, id: &ProjectId) -> PathBuf {
        let hash = blake3::hash(id.0.as_bytes()).to_hex();
        self.dir.join(&hash.as_str()[..16])
    }

    /// Reads `pending.json` of `project_dir`, resolving blob paths. Each must
    /// be `blobs/<name>`; an absolute path (written by older versions) is
    /// accepted only when it names such a file of this folder.
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
            item.blob = resolve_blob(project_dir, &item.blob)
                .with_context(|| format!("bozuk bekleme kaydı: {}", path.display()))?;
        }
        Ok(Some(project))
    }
}

fn corrupt_record(project_dir: &Path, e: &anyhow::Error) -> CorruptRecord {
    CorruptRecord {
        path: project_dir.join(PENDING_JSON),
        error: format!("{e:#}"),
    }
}

/// `project_dir` + the stored blob path, which must be `blobs/<name>`
/// (relative, or absolute below `project_dir`).
fn resolve_blob(project_dir: &Path, stored: &Path) -> Result<PathBuf> {
    let rel = if stored.is_absolute() {
        stored
            .strip_prefix(project_dir)
            .map_err(|_| anyhow!("blob yolu kayıt klasörünün dışında: {}", stored.display()))?
    } else {
        stored
    };
    let parts: Vec<Component> = rel.components().collect();
    let ok = matches!(
        parts.as_slice(),
        [Component::Normal(dir), Component::Normal(_)] if *dir == BLOBS_DIR
    );
    if !ok {
        bail!("geçersiz blob yolu: {}", stored.display());
    }
    Ok(project_dir.join(rel))
}

/// The record as stored: blob paths relative to `project_dir`.
fn stored_form(project_dir: &Path, project: &PendingProject) -> PendingProject {
    let mut out = project.clone();
    for item in &mut out.items {
        if let Ok(rel) = item.blob.strip_prefix(project_dir) {
            item.blob = rel.to_path_buf();
        }
    }
    out
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
    match rename_blob(src, dst) {
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

fn rename_blob(src: &Path, dst: &Path) -> io::Result<()> {
    #[cfg(test)]
    if tests::FORCE_CROSS_DEVICE.get() {
        return Err(io::Error::from(ErrorKind::CrossesDevices));
    }
    fs::rename(src, dst)
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
        serde_json::to_writer_pretty(&mut file, &stored_form(project_dir, project))?;
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
        /// Makes every blob rename fail as if it crossed file systems.
        pub(super) static FORCE_CROSS_DEVICE: Cell<bool> = const { Cell::new(false) };
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
            source_home: None,
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
        let listed = store.list().unwrap().items;
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].items.len(), 1);
        assert_eq!(fs::read(&listed[0].items[0].blob).unwrap(), b"keep");
        assert!(store.list_orphans().unwrap().items.is_empty());
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
        assert!(store.list().unwrap().items.is_empty());
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
        assert!(store.list().unwrap().items.is_empty());
        assert!(store.list_orphans().unwrap().items.is_empty());
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
        assert_eq!(store.list_orphans().unwrap().items, expected);
        assert_eq!(store.list().unwrap().items.len(), 1);
    }

    fn raw_record(store: &PendingStore) -> serde_json::Value {
        let path = store.project_dir(&project().id).join(PENDING_JSON);
        serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
    }

    #[test]
    fn second_put_keeps_blob_paths_relative() {
        let tmp = tempfile::tempdir().unwrap();
        let store = PendingStore {
            dir: tmp.path().join("pending"),
        };
        store.put(&project(), &[item(tmp.path(), 0, b"a")]).unwrap();
        store.put(&project(), &[item(tmp.path(), 1, b"b")]).unwrap();
        let record = raw_record(&store);
        for it in record["items"].as_array().unwrap() {
            let blob = it["blob"].as_str().unwrap();
            assert!(blob.starts_with("blobs/"), "relative blob path: {blob}");
        }
        let taken = store.take(&project().id).unwrap();
        assert_eq!(fs::read(&taken.items[0].blob).unwrap(), b"a");
        assert_eq!(fs::read(&taken.items[1].blob).unwrap(), b"b");
    }

    #[test]
    fn legacy_absolute_blob_inside_store_is_migrated() {
        let tmp = tempfile::tempdir().unwrap();
        let store = PendingStore {
            dir: tmp.path().join("pending"),
        };
        store.put(&project(), &[item(tmp.path(), 0, b"a")]).unwrap();
        // An older version wrote loaded items back with absolute paths.
        let project_dir = store.project_dir(&project().id);
        let mut record = raw_record(&store);
        let rel = record["items"][0]["blob"].as_str().unwrap().to_owned();
        record["items"][0]["blob"] = project_dir.join(&rel).to_str().unwrap().into();
        fs::write(project_dir.join(PENDING_JSON), record.to_string()).unwrap();

        let taken = store.take(&project().id).unwrap();
        assert_eq!(fs::read(&taken.items[0].blob).unwrap(), b"a");
        store.put(&project(), &[item(tmp.path(), 1, b"b")]).unwrap();
        let record = raw_record(&store);
        assert_eq!(record["items"][0]["blob"].as_str().unwrap(), rel);
    }

    #[test]
    fn blob_paths_outside_the_project_dir_are_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let store = PendingStore {
            dir: tmp.path().join("pending"),
        };
        store.put(&project(), &[item(tmp.path(), 0, b"a")]).unwrap();
        let project_dir = store.project_dir(&project().id);
        let outside = tmp.path().join("outside");
        fs::write(&outside, b"not ours").unwrap();
        for bad in [
            outside.to_str().unwrap().to_owned(),
            "../../outside".to_owned(),
            "blobs/../../../outside".to_owned(),
            format!("{}/blobs/../../../outside", project_dir.display()),
        ] {
            let mut record = raw_record(&store);
            record["items"][0]["blob"] = bad.clone().into();
            fs::write(project_dir.join(PENDING_JSON), record.to_string()).unwrap();
            assert!(store.take(&project().id).is_err(), "accepted {bad}");
        }
    }

    #[test]
    fn corrupt_record_does_not_hide_the_others() {
        let tmp = tempfile::tempdir().unwrap();
        let store = PendingStore {
            dir: tmp.path().join("pending"),
        };
        store.put(&project(), &[item(tmp.path(), 0, b"a")]).unwrap();
        let broken = store.dir.join("0123456789abcdef");
        fs::create_dir_all(broken.join(BLOBS_DIR)).unwrap();
        fs::write(broken.join(PENDING_JSON), b"{ not json").unwrap();
        fs::write(broken.join(BLOBS_DIR).join("x-0"), b"kept").unwrap();

        let listed = store.list().unwrap();
        assert_eq!(listed.items.len(), 1);
        assert_eq!(listed.corrupt.len(), 1);
        assert_eq!(listed.corrupt[0].path, broken.join(PENDING_JSON));
        let orphans = store.list_orphans().unwrap();
        // Blobs of an unreadable record are not orphans: it may still own them.
        assert!(orphans.items.is_empty(), "{:?}", orphans.items);
        assert_eq!(orphans.corrupt.len(), 1);
    }

    #[test]
    fn remove_orphans_removes_only_current_orphans() {
        let tmp = tempfile::tempdir().unwrap();
        let store = PendingStore {
            dir: tmp.path().join("pending"),
        };
        store.put(&project(), &[item(tmp.path(), 0, b"a")]).unwrap();
        let project_dir = store.project_dir(&project().id);
        let stray = project_dir.join(BLOBS_DIR).join("ffff-9");
        fs::write(&stray, b"lost").unwrap();
        let outside = tmp.path().join("outside");
        fs::write(&outside, b"x").unwrap();

        let removed = store
            .remove_orphans(&[stray.clone(), outside.clone()])
            .unwrap();
        assert_eq!(removed, vec![stray.clone()]);
        assert!(!stray.exists());
        assert!(outside.exists());
        assert_eq!(store.list().unwrap().items[0].items.len(), 1);
    }

    #[test]
    fn archive_and_restore_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let store = PendingStore {
            dir: tmp.path().join("pending"),
        };
        store.put(&project(), &[item(tmp.path(), 0, b"a")]).unwrap();
        let archive = tmp.path().join("archive");
        store.archive(&project().id, &archive).unwrap();
        assert!(store.list().unwrap().items.is_empty());
        let restored = store.restore(&archive).unwrap();
        assert_eq!(restored, vec![project().id]);
        let taken = store.take(&project().id).unwrap();
        assert_eq!(fs::read(&taken.items[0].blob).unwrap(), b"a");
        // Nothing left to restore; a second run is harmless.
        assert!(store.restore(&archive).unwrap().is_empty());
    }

    #[test]
    fn legacy_absolute_record_survives_archive_and_restore() {
        let tmp = tempfile::tempdir().unwrap();
        let store = PendingStore {
            dir: tmp.path().join("pending"),
        };
        store.put(&project(), &[item(tmp.path(), 0, b"a")]).unwrap();
        let project_dir = store.project_dir(&project().id);
        let mut record = raw_record(&store);
        let rel = record["items"][0]["blob"].as_str().unwrap().to_owned();
        record["items"][0]["blob"] = project_dir.join(&rel).to_str().unwrap().into();
        fs::write(project_dir.join(PENDING_JSON), record.to_string()).unwrap();

        let archive = tmp.path().join("archive");
        store.archive(&project().id, &archive).unwrap();
        assert_eq!(store.restore(&archive).unwrap(), vec![project().id]);
        let taken = store.take(&project().id).unwrap();
        assert_eq!(fs::read(&taken.items[0].blob).unwrap(), b"a");
        assert_eq!(
            raw_record(&store)["items"][0]["blob"].as_str().unwrap(),
            rel
        );
    }

    #[test]
    fn cross_device_put_copies_and_keeps_metadata() {
        let tmp = tempfile::tempdir().unwrap();
        let store = PendingStore {
            dir: tmp.path().join("pending"),
        };
        let it = item(tmp.path(), 0, b"copied");
        let mtime = filetime::FileTime::from_unix_time(1_600_000_000, 0);
        filetime::set_file_mtime(&it.blob, mtime).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&it.blob, fs::Permissions::from_mode(0o640)).unwrap();
        }
        FORCE_CROSS_DEVICE.set(true);
        let result = store.put(&project(), std::slice::from_ref(&it));
        FORCE_CROSS_DEVICE.set(false);
        result.unwrap();
        // Copied, so the source stays for the caller.
        assert!(it.blob.exists());
        let taken = store.take(&project().id).unwrap();
        let blob = &taken.items[0].blob;
        assert_eq!(fs::read(blob).unwrap(), b"copied");
        let md = fs::metadata(blob).unwrap();
        assert_eq!(filetime::FileTime::from_last_modification_time(&md), mtime);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(md.permissions().mode() & 0o777, 0o640);
        }
    }

    #[test]
    fn cross_device_failure_removes_copies() {
        let tmp = tempfile::tempdir().unwrap();
        let store = PendingStore {
            dir: tmp.path().join("pending"),
        };
        let items = [item(tmp.path(), 0, b"one"), item(tmp.path(), 1, b"two")];
        FORCE_CROSS_DEVICE.set(true);
        FAIL_JSON_WRITE.set(true);
        let result = store.put(&project(), &items);
        FAIL_JSON_WRITE.set(false);
        FORCE_CROSS_DEVICE.set(false);
        assert!(result.is_err());
        for it in &items {
            assert!(it.blob.exists());
        }
        assert!(store.list().unwrap().items.is_empty());
        assert!(store.list_orphans().unwrap().items.is_empty());
    }

    #[test]
    fn items_sharing_an_entry_index_keep_their_own_blobs() {
        let tmp = tempfile::tempdir().unwrap();
        let store = PendingStore {
            dir: tmp.path().join("pending"),
        };
        let first = item(tmp.path(), 7, b"projects part");
        let mut second = item(tmp.path(), 8, b"repo paths part");
        second.entry.index = 7;
        store.put(&project(), &[first, second]).unwrap();
        let taken = store.take(&project().id).unwrap();
        assert_eq!(fs::read(&taken.items[0].blob).unwrap(), b"projects part");
        assert_eq!(fs::read(&taken.items[1].blob).unwrap(), b"repo paths part");
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
