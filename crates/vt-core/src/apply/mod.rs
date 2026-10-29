//! Apply engine: the only code that writes into the user's target
//! directories. Every replaced or removed target object is moved to
//! `<vt_home>/backups/<session>/abs/<absolute dest>` first, every step is
//! journaled before it runs (see `journal`), and any failure rolls the
//! whole session back.

mod conflict;
mod fsops;
mod journal;

use std::collections::BTreeSet;
use std::fs;
use std::io::{ErrorKind, Write};
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde_json::Value;

pub use conflict::{ConflictResolver, Decision, Resolution, resolve_json};
pub use journal::rollback;

use crate::fsutil::{create_private_dir_all, create_private_file};
use crate::manifest::EntryKind;
use crate::scan::hash_file;
use fsops::{
    file_time, lstat, make_symlink, mode_of, move_path, set_mode, sync_parent, tmp_beside,
};
use journal::{Journal, Op, Placed, hex};

/// One bundle entry to put at `dest`. `blob` is unused for dirs and
/// symlinks. `mode` 0 (non-unix source) keeps the platform default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placement {
    pub entry_index: u32,
    pub blob: PathBuf,
    pub dest: PathBuf,
    pub kind: EntryKind,
    pub mtime_ns: i64,
    pub mode: u32,
    pub expected_hash: Option<[u8; 32]>,
}

/// On a `Replace` decision: every file or symlink under `dir` that is not
/// in `keep` is moved to the backup. `keep` holds absolute paths (like
/// `Placement::dest`); paths relative to `dir` are accepted too.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplaceScope {
    pub dir: PathBuf,
    pub keep: BTreeSet<PathBuf>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ApplyPlan {
    /// File name safe: ASCII letters, digits, `-`, `_`, `.`.
    pub session_id: String,
    /// Host name of the source machine, used for `KeepBoth` names.
    pub source_host: String,
    pub placements: Vec<Placement>,
    pub replace_scopes: Vec<ReplaceScope>,
    /// Full-file replacements with already merged JSON.
    pub json_writes: Vec<(PathBuf, Value)>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ApplyReport {
    /// Files, symlinks and JSON documents written.
    pub placed: u32,
    pub skipped_identical: u32,
    /// Suffixed paths written by `KeepBoth`.
    pub kept_both: Vec<PathBuf>,
    /// Target paths whose previous object went to the backup.
    pub backed_up: Vec<PathBuf>,
    pub conflicts: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RollbackReport {
    /// Backups moved back to their target path.
    pub restored: u32,
    /// Placed files and symlinks removed.
    pub removed: u32,
    pub dirs_removed: u32,
    /// Changed objects rollback moved away instead of deleting.
    pub set_aside: Vec<PathBuf>,
}

/// Applies `plan` under session `plan.session_id`. Order: replace scopes,
/// placements, JSON writes. On any error the session is rolled back and
/// the error returned (with the rollback error, if that failed too).
pub fn apply(
    plan: &ApplyPlan,
    vt_home: &Path,
    resolver: &mut dyn ConflictResolver,
) -> Result<ApplyReport> {
    validate_session(&plan.session_id)?;
    validate_plan(plan)?;
    let journal = Journal::create(vt_home, &plan.session_id)?;
    let mut run = Run {
        journal,
        backups: backup_root(vt_home, &plan.session_id),
        host: &plan.source_host,
        report: ApplyReport::default(),
        new_dirs: Vec::new(),
    };
    match run.execute(plan, resolver) {
        Ok(()) => Ok(run.report),
        Err(e) => {
            drop(run);
            match rollback(vt_home, &plan.session_id) {
                Ok(_) => Err(e.context("uygulama başarısız, değişiklikler geri alındı")),
                Err(re) => Err(e.context(format!(
                    "uygulama başarısız, geri alma da başarısız: {re:#}"
                ))),
            }
        }
    }
}

struct Run<'a> {
    journal: Journal,
    backups: PathBuf,
    host: &'a str,
    report: ApplyReport,
    /// Directories created from `Dir` placements and their mtime.
    new_dirs: Vec<(PathBuf, i64)>,
}

impl Run<'_> {
    fn execute(&mut self, plan: &ApplyPlan, resolver: &mut dyn ConflictResolver) -> Result<()> {
        for scope in &plan.replace_scopes {
            self.clear_scope(scope)?;
        }
        for p in &plan.placements {
            self.place(p, resolver)
                .with_context(|| format!("yerleştirilemedi: {}", p.dest.display()))?;
        }
        for (dest, value) in &plan.json_writes {
            self.write_json(dest, value)
                .with_context(|| format!("yazılamadı: {}", dest.display()))?;
        }
        // Children changed the mtime of new dirs; set it last, deepest first.
        self.new_dirs
            .sort_by_key(|(p, _)| std::cmp::Reverse(p.components().count()));
        for (dir, ns) in &self.new_dirs {
            filetime::set_file_mtime(dir, file_time(*ns))
                .with_context(|| format!("zaman ayarlanamadı: {}", dir.display()))?;
        }
        Ok(())
    }

    fn clear_scope(&mut self, scope: &ReplaceScope) -> Result<()> {
        // A symlinked or missing scope dir is not entered.
        match lstat(&scope.dir)? {
            Some(md) if md.is_dir() => {}
            _ => return Ok(()),
        }
        let mut victims = Vec::new();
        collect_files(&scope.dir, &mut victims)?;
        victims.sort();
        for path in victims {
            let rel = path.strip_prefix(&scope.dir).unwrap_or(&path);
            if scope.keep.contains(&path) || scope.keep.contains(rel) {
                continue;
            }
            self.backup(&path)?;
        }
        Ok(())
    }

    fn place(&mut self, p: &Placement, resolver: &mut dyn ConflictResolver) -> Result<()> {
        if p.kind == EntryKind::Dir {
            return self.place_dir(p);
        }
        let existing = lstat(&p.dest)?;
        let (target, backup) = match existing {
            None => (p.dest.clone(), None),
            Some(md) if md.is_dir() => bail!("hedefte aynı adla bir dizin var"),
            Some(md) => {
                if is_identical(p, &md)? {
                    self.report.skipped_identical += 1;
                    return Ok(());
                }
                let res = resolver.file(&p.dest);
                self.report
                    .conflicts
                    .push(format!("{}: {}", p.dest.display(), describe(res)));
                match res {
                    Resolution::KeepTarget => return Ok(()),
                    Resolution::KeepBoth => {
                        let t = self.free_keep_both_name(&p.dest)?;
                        self.report.kept_both.push(t.clone());
                        (t, None)
                    }
                    Resolution::KeepSource => {
                        let b = self.backup(&p.dest)?;
                        (p.dest.clone(), Some(b))
                    }
                }
            }
        };
        self.ensure_parent(&target)?;
        match &p.kind {
            EntryKind::File => self.write_file(p, &target, backup)?,
            EntryKind::Symlink { target: link } => {
                self.write_symlink(p, Path::new(link), &target, backup)?;
            }
            EntryKind::Dir => unreachable!("handled above"),
        }
        self.report.placed += 1;
        Ok(())
    }

    fn place_dir(&mut self, p: &Placement) -> Result<()> {
        match lstat(&p.dest)? {
            Some(md) if md.is_dir() => return Ok(()),
            Some(_) => bail!("hedefte dizin yerine başka bir öğe var"),
            None => {}
        }
        self.ensure_parent(&p.dest)?;
        self.journal.append(&Op::Mkdir {
            dest: p.dest.clone(),
        })?;
        fs::create_dir(&p.dest).with_context(|| format!("oluşturulamadı: {}", p.dest.display()))?;
        if p.mode != 0 {
            set_mode(&p.dest, p.mode & 0o777)?;
        }
        sync_parent(&p.dest);
        self.new_dirs.push((p.dest.clone(), p.mtime_ns));
        Ok(())
    }

    /// Copies the blob next to `target`, sets mode and mtime, verifies the
    /// hash and renames it into place.
    fn write_file(&mut self, p: &Placement, target: &Path, backup: Option<PathBuf>) -> Result<()> {
        let hash = match p.expected_hash {
            Some(h) => h,
            None => hash_file(&p.blob)?,
        };
        let tmp = tmp_beside(target);
        self.journal.append(&Op::Place {
            dest: target.to_path_buf(),
            backup,
            tmp: tmp.clone(),
            content: Placed::File { hash: hex(&hash) },
        })?;
        let result = (|| -> Result<()> {
            fs::copy(&p.blob, &tmp)
                .with_context(|| format!("kopyalanamadı: {}", p.blob.display()))?;
            if p.mode != 0 {
                set_mode(&tmp, p.mode & 0o777)?;
            }
            fs::File::open(&tmp)
                .and_then(|f| f.sync_all())
                .with_context(|| format!("diske yazılamadı: {}", tmp.display()))?;
            if hash_file(&tmp)? != hash {
                bail!("hash uyuşmuyor (kayıt {})", p.entry_index);
            }
            filetime::set_file_mtime(&tmp, file_time(p.mtime_ns))
                .with_context(|| format!("zaman ayarlanamadı: {}", tmp.display()))?;
            fs::rename(&tmp, target).context("yerine taşınamadı")
        })();
        if result.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        result?;
        sync_parent(target);
        Ok(())
    }

    fn write_symlink(
        &mut self,
        p: &Placement,
        link: &Path,
        target: &Path,
        backup: Option<PathBuf>,
    ) -> Result<()> {
        let tmp = tmp_beside(target);
        self.journal.append(&Op::Place {
            dest: target.to_path_buf(),
            backup,
            tmp: tmp.clone(),
            content: Placed::Symlink {
                target: link.to_path_buf(),
            },
        })?;
        let result = (|| -> Result<()> {
            make_symlink(link, &tmp)?;
            let t = file_time(p.mtime_ns);
            filetime::set_symlink_file_times(&tmp, t, t)
                .with_context(|| format!("zaman ayarlanamadı: {}", tmp.display()))?;
            fs::rename(&tmp, target).context("yerine taşınamadı")
        })();
        if result.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        result?;
        sync_parent(target);
        Ok(())
    }

    /// Replaces `dest` with pretty JSON plus a newline, keeping the mode of
    /// a replaced regular file (0600 otherwise).
    fn write_json(&mut self, dest: &Path, value: &Value) -> Result<()> {
        let mut bytes = serde_json::to_vec_pretty(value).context("JSON yazılamadı")?;
        bytes.push(b'\n');
        let mut mode = 0o600;
        let mut backup = None;
        match lstat(dest)? {
            None => {}
            Some(md) if md.is_dir() => bail!("hedefte aynı adla bir dizin var"),
            Some(md) => {
                if md.is_file() {
                    if fs::read(dest).ok().as_deref() == Some(bytes.as_slice()) {
                        self.report.skipped_identical += 1;
                        return Ok(());
                    }
                    let m = mode_of(&md) & 0o777;
                    if m != 0 {
                        mode = m;
                    }
                }
                backup = Some(self.backup(dest)?);
            }
        }
        self.ensure_parent(dest)?;
        let tmp = tmp_beside(dest);
        self.journal.append(&Op::Place {
            dest: dest.to_path_buf(),
            backup,
            tmp: tmp.clone(),
            content: Placed::File {
                hash: blake3::hash(&bytes).to_hex().to_string(),
            },
        })?;
        let result = (|| -> Result<()> {
            let mut f = create_private_file(&tmp)
                .with_context(|| format!("oluşturulamadı: {}", tmp.display()))?;
            f.write_all(&bytes)?;
            set_mode(&tmp, mode)?;
            f.sync_all()?;
            fs::rename(&tmp, dest).context("yerine taşınamadı")
        })();
        if result.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        result?;
        sync_parent(dest);
        self.report.placed += 1;
        Ok(())
    }

    /// Moves the object at `dest` (not following symlinks) to the backup.
    fn backup(&mut self, dest: &Path) -> Result<PathBuf> {
        let to = unique_path(&mirror_path(&self.backups.join("abs"), dest))?;
        let parent = to.parent().expect("mirror path has a parent");
        create_private_dir_all(parent)
            .with_context(|| format!("oluşturulamadı: {}", parent.display()))?;
        self.journal.append(&Op::Backup {
            dest: dest.to_path_buf(),
            backup: to.clone(),
        })?;
        move_path(dest, &to).with_context(|| format!("yedeklenemedi: {}", dest.display()))?;
        self.report.backed_up.push(dest.to_path_buf());
        Ok(to)
    }

    /// Creates missing ancestors of `path`, outermost first, each journaled.
    fn ensure_parent(&mut self, path: &Path) -> Result<()> {
        let Some(parent) = path.parent() else {
            return Ok(());
        };
        let missing: Vec<&Path> = parent
            .ancestors()
            .take_while(|a| !a.as_os_str().is_empty() && !a.exists())
            .collect();
        for dir in missing.into_iter().rev() {
            self.journal.append(&Op::Mkdir {
                dest: dir.to_path_buf(),
            })?;
            match fs::create_dir(dir) {
                Err(e) if e.kind() != ErrorKind::AlreadyExists => {
                    return Err(e).with_context(|| format!("oluşturulamadı: {}", dir.display()));
                }
                _ => sync_parent(dir),
            }
        }
        Ok(())
    }

    fn free_keep_both_name(&self, dest: &Path) -> Result<PathBuf> {
        for n in 1..10_000 {
            let candidate = conflict::keep_both_name(dest, self.host, n);
            if lstat(&candidate)?.is_none() {
                return Ok(candidate);
            }
        }
        bail!("boş ad bulunamadı: {}", dest.display())
    }
}

fn describe(res: Resolution) -> &'static str {
    match res {
        Resolution::KeepSource => "kaynak yazıldı, hedef yedeklendi",
        Resolution::KeepTarget => "hedef korundu",
        Resolution::KeepBoth => "ikisi de tutuldu",
    }
}

/// Same content at `p.dest` (`md` is its lstat) as the placement brings.
fn is_identical(p: &Placement, md: &fs::Metadata) -> Result<bool> {
    match &p.kind {
        EntryKind::File => {
            if !md.is_file() {
                return Ok(false);
            }
            let blob_len = fs::metadata(&p.blob)
                .with_context(|| format!("okunamadı: {}", p.blob.display()))?
                .len();
            if blob_len != md.len() {
                return Ok(false);
            }
            let source = match p.expected_hash {
                Some(h) => h,
                None => hash_file(&p.blob)?,
            };
            Ok(hash_file(&p.dest)? == source)
        }
        EntryKind::Symlink { target } => Ok(md.file_type().is_symlink()
            && fs::read_link(&p.dest)
                .with_context(|| format!("okunamadı: {}", p.dest.display()))?
                == Path::new(target)),
        EntryKind::Dir => Ok(md.is_dir()),
    }
}

/// Files and symlinks under `dir`; symlinked dirs are not entered.
fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    let entries = fs::read_dir(dir).with_context(|| format!("okunamadı: {}", dir.display()))?;
    for entry in entries {
        let entry = entry.with_context(|| format!("okunamadı: {}", dir.display()))?;
        let ft = entry
            .file_type()
            .with_context(|| format!("okunamadı: {}", entry.path().display()))?;
        if ft.is_dir() {
            collect_files(&entry.path(), out)?;
        } else if ft.is_file() || ft.is_symlink() {
            out.push(entry.path());
        }
    }
    Ok(())
}

pub(crate) fn backup_root(vt_home: &Path, session: &str) -> PathBuf {
    vt_home.join("backups").join(session)
}

/// `root` + the components of the absolute `path` (root dir dropped).
pub(crate) fn mirror_path(root: &Path, path: &Path) -> PathBuf {
    let mut out = root.to_path_buf();
    for c in path.components() {
        if let Component::Normal(part) = c {
            out.push(part);
        }
    }
    out
}

/// `path`, or `path.<n>` for the first `n` with nothing there yet.
pub(crate) fn unique_path(path: &Path) -> Result<PathBuf> {
    if lstat(path)?.is_none() {
        return Ok(path.to_path_buf());
    }
    for n in 1..10_000 {
        let mut name = path.file_name().unwrap_or_default().to_os_string();
        name.push(format!(".{n}"));
        let candidate = path.with_file_name(name);
        if lstat(&candidate)?.is_none() {
            return Ok(candidate);
        }
    }
    bail!("boş ad bulunamadı: {}", path.display())
}

pub(crate) fn validate_session(id: &str) -> Result<()> {
    let ok = !id.is_empty()
        && !id.starts_with('.')
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if !ok {
        bail!("geçersiz oturum kimliği: {id:?}");
    }
    Ok(())
}

/// Every target path must be absolute and free of `..`, so nothing can
/// reach outside the place the caller computed.
fn validate_plan(plan: &ApplyPlan) -> Result<()> {
    let dests = plan
        .placements
        .iter()
        .map(|p| &p.dest)
        .chain(plan.replace_scopes.iter().map(|s| &s.dir))
        .chain(plan.json_writes.iter().map(|(d, _)| d));
    for d in dests {
        let clean = d.is_absolute()
            && d.file_name().is_some()
            && d.components().all(|c| c != Component::ParentDir);
        if !clean {
            bail!("geçersiz hedef yolu: {}", d.display());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Always(Resolution);
    impl ConflictResolver for Always {
        fn file(&mut self, _dest: &Path) -> Resolution {
            self.0
        }
        fn json(&mut self, _c: &crate::manifest::JsonConflict) -> Resolution {
            self.0
        }
    }

    /// The copy path used across file systems keeps content, mode and
    /// mtime in both directions.
    #[cfg(unix)]
    #[test]
    fn cross_device_backup_and_restore() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let vt = root.join("vt");
        let dest = root.join("dst/a.txt");
        fs::create_dir_all(dest.parent().unwrap()).unwrap();
        fs::write(&dest, b"old").unwrap();
        fs::set_permissions(&dest, fs::Permissions::from_mode(0o640)).unwrap();
        filetime::set_file_mtime(&dest, file_time(1_500_000_000_000_000_007)).unwrap();
        let blob = root.join("blob");
        fs::write(&blob, b"new").unwrap();

        fsops::FORCE_COPY.set(true);
        let plan = ApplyPlan {
            session_id: "s1".into(),
            source_host: "src".into(),
            placements: vec![Placement {
                entry_index: 0,
                blob,
                dest: dest.clone(),
                kind: EntryKind::File,
                mtime_ns: 0,
                mode: 0o600,
                expected_hash: None,
            }],
            replace_scopes: Vec::new(),
            json_writes: Vec::new(),
        };
        let report = apply(&plan, &vt, &mut Always(Resolution::KeepSource)).unwrap();
        assert_eq!(report.backed_up, vec![dest.clone()]);
        assert_eq!(fs::read(&dest).unwrap(), b"new");
        let b = mirror_path(&vt.join("backups/s1/abs"), &dest);
        assert_eq!(fs::read(&b).unwrap(), b"old");

        let rb = rollback(&vt, "s1").unwrap();
        fsops::FORCE_COPY.set(false);
        assert_eq!(rb.restored, 1);
        assert!(fs::symlink_metadata(&b).is_err());
        let md = fs::metadata(&dest).unwrap();
        assert_eq!(fs::read(&dest).unwrap(), b"old");
        assert_eq!(md.mode() & 0o7777, 0o640);
        assert_eq!(
            md.mtime() * 1_000_000_000 + md.mtime_nsec(),
            1_500_000_000_000_000_007
        );
    }

    /// A placed file edited after the import is set aside, not deleted.
    #[test]
    fn rollback_sets_aside_changed_placed_file() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let vt = root.join("vt");
        let dest = root.join("dst/new.txt");
        let blob = root.join("blob");
        fs::write(&blob, b"placed").unwrap();
        let plan = ApplyPlan {
            session_id: "s1".into(),
            source_host: "src".into(),
            placements: vec![Placement {
                entry_index: 0,
                blob,
                dest: dest.clone(),
                kind: EntryKind::File,
                mtime_ns: 0,
                mode: 0o600,
                expected_hash: None,
            }],
            replace_scopes: Vec::new(),
            json_writes: Vec::new(),
        };
        apply(&plan, &vt, &mut Always(Resolution::KeepSource)).unwrap();
        fs::write(&dest, b"user edit").unwrap();
        let rb = rollback(&vt, "s1").unwrap();
        assert!(!dest.exists());
        assert_eq!(rb.set_aside.len(), 1);
        assert_eq!(fs::read(&rb.set_aside[0]).unwrap(), b"user edit");
    }

    #[test]
    fn torn_last_journal_line_is_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        let vt = tmp.path().join("vt");
        let mut j = Journal::create(&vt, "s1").unwrap();
        j.append(&Op::Mkdir {
            dest: tmp.path().join("never-created"),
        })
        .unwrap();
        drop(j);
        let path = journal::journal_path(&vt, "s1");
        let mut f = fs::OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(br#"{"op":"backup","dest":"/x"#).unwrap();
        let rb = rollback(&vt, "s1").unwrap();
        assert_eq!(rb, RollbackReport::default());
        // The torn line was cut, so the undo lines parse on the next run.
        assert_eq!(rollback(&vt, "s1").unwrap(), RollbackReport::default());
        let text = fs::read_to_string(&path).unwrap();
        assert_eq!(text.lines().count(), 2, "{text}");
    }
}
