//! File moves that never lose the source before the copy is durable.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use filetime::FileTime;
use rand::RngCore;

#[cfg(test)]
thread_local! {
    /// Makes `move_path` take the cross file system branch.
    pub(super) static FORCE_COPY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// `<parent>/.vt-tmp-<random>` beside `path`, on the same file system.
pub(super) fn tmp_beside(path: &Path) -> PathBuf {
    let mut bytes = [0u8; 8];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    path.with_file_name(format!(".vt-tmp-{hex}"))
}

pub(super) fn file_time(ns: i64) -> FileTime {
    // rem_euclid of 1e9 always fits in u32.
    FileTime::from_unix_time(
        ns.div_euclid(1_000_000_000),
        ns.rem_euclid(1_000_000_000) as u32,
    )
}

/// `Ok(None)` when nothing is at `path`; symlinks are not followed.
pub(super) fn lstat(path: &Path) -> Result<Option<fs::Metadata>> {
    match fs::symlink_metadata(path) {
        Ok(md) => Ok(Some(md)),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("okunamadı: {}", path.display())),
    }
}

/// Persists renames and new entries in `dir`; best effort.
pub(super) fn sync_dir(dir: &Path) {
    if let Ok(d) = fs::File::open(dir) {
        let _ = d.sync_all();
    }
}

pub(super) fn sync_parent(path: &Path) {
    if let Some(p) = path.parent() {
        sync_dir(p);
    }
}

/// Moves the file or symlink `src` to `dst`, replacing `dst`. Same file
/// system: one rename. Otherwise `src` is copied to a temp name beside
/// `dst` with mode and mtime, synced, renamed into place, and only then
/// removed.
pub(super) fn move_path(src: &Path, dst: &Path) -> Result<()> {
    #[cfg(test)]
    let forced = FORCE_COPY.get();
    #[cfg(not(test))]
    let forced = false;
    let renamed = if forced {
        Err(std::io::Error::from(ErrorKind::CrossesDevices))
    } else {
        fs::rename(src, dst)
    };
    match renamed {
        Ok(()) => {
            sync_parent(dst);
            sync_parent(src);
            Ok(())
        }
        Err(e) if e.kind() == ErrorKind::CrossesDevices => {
            let tmp = tmp_beside(dst);
            if let Err(e) = copy_preserving(src, &tmp) {
                let _ = fs::remove_file(&tmp);
                return Err(e);
            }
            if let Err(e) = fs::rename(&tmp, dst) {
                let _ = fs::remove_file(&tmp);
                return Err(e).with_context(|| format!("taşınamadı: {}", dst.display()));
            }
            sync_parent(dst);
            fs::remove_file(src).with_context(|| format!("silinemedi: {}", src.display()))?;
            sync_parent(src);
            Ok(())
        }
        Err(e) => Err(e).with_context(|| format!("taşınamadı: {}", src.display())),
    }
}

/// Copies a file (mode, mtime, synced) or a symlink (target, mtime).
fn copy_preserving(src: &Path, dst: &Path) -> Result<()> {
    let md = fs::symlink_metadata(src).with_context(|| format!("okunamadı: {}", src.display()))?;
    let mtime = FileTime::from_last_modification_time(&md);
    if md.file_type().is_symlink() {
        let target = fs::read_link(src).with_context(|| format!("okunamadı: {}", src.display()))?;
        make_symlink(&target, dst)?;
        filetime::set_symlink_file_times(dst, mtime, mtime)
            .with_context(|| format!("zaman ayarlanamadı: {}", dst.display()))?;
        return Ok(());
    }
    if !md.is_file() {
        bail!(
            "yalnız dosya veya sembolik bağ taşınabilir: {}",
            src.display()
        );
    }
    // `fs::copy` keeps the permission bits.
    fs::copy(src, dst).with_context(|| format!("kopyalanamadı: {}", src.display()))?;
    fs::File::open(dst)
        .and_then(|f| f.sync_all())
        .with_context(|| format!("diske yazılamadı: {}", dst.display()))?;
    filetime::set_file_mtime(dst, mtime)
        .with_context(|| format!("zaman ayarlanamadı: {}", dst.display()))
}

#[cfg(unix)]
pub(super) fn make_symlink(target: &Path, at: &Path) -> Result<()> {
    std::os::unix::fs::symlink(target, at)
        .with_context(|| format!("oluşturulamadı: {}", at.display()))
}

#[cfg(not(unix))]
pub(super) fn make_symlink(_target: &Path, at: &Path) -> Result<()> {
    bail!("sembolik bağ oluşturulamıyor: {}", at.display())
}

#[cfg(unix)]
pub(super) fn mode_of(md: &fs::Metadata) -> u32 {
    use std::os::unix::fs::MetadataExt;
    md.mode() & 0o7777
}

#[cfg(not(unix))]
pub(super) fn mode_of(_md: &fs::Metadata) -> u32 {
    0
}

#[cfg(unix)]
pub(super) fn set_mode(path: &Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .with_context(|| format!("izin ayarlanamadı: {}", path.display()))
}

#[cfg(not(unix))]
pub(super) fn set_mode(_path: &Path, _mode: u32) -> Result<()> {
    Ok(())
}
