//! Read-only tree scanning and content hashing.

use std::fs::{self, File, Metadata};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use unicode_normalization::UnicodeNormalization;

use crate::manifest::EntryKind;

/// A scanned file system object before it is classified into an `Entry`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawEntry {
    pub rel: String,
    pub kind: EntryKind,
    pub size: u64,
    pub mtime_ns: i64,
    pub mode: u32,
}

/// Walks `root` without following symlinks and returns its contents sorted
/// by `rel`. The root itself is not included.
///
/// `exclude` gets the `/`-separated NFC `rel` path; an excluded directory is
/// skipped with its whole subtree. Symlinks are reported with their target.
/// Sockets, FIFOs and device files are skipped. Non-UTF-8 names are an error.
pub fn scan_tree(root: &Path, exclude: &dyn Fn(&str) -> bool) -> Result<Vec<RawEntry>> {
    let mut out = Vec::new();
    let mut stack: Vec<(PathBuf, String)> = vec![(root.to_path_buf(), String::new())];

    while let Some((dir, prefix)) = stack.pop() {
        let read = fs::read_dir(&dir).with_context(|| format!("read_dir {}", dir.display()))?;
        for item in read {
            let item = item.with_context(|| format!("read_dir {}", dir.display()))?;
            let path = item.path();
            let name = item
                .file_name()
                .into_string()
                .map_err(|n| anyhow!("UTF-8 olmayan dosya adı: {}", Path::new(&n).display()))?;
            let name: String = name.nfc().collect();
            let rel = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            };
            if exclude(&rel) {
                continue;
            }

            let md = fs::symlink_metadata(&path)
                .with_context(|| format!("metadata {}", path.display()))?;
            let ft = md.file_type();
            let kind = if ft.is_symlink() {
                let target = fs::read_link(&path)
                    .with_context(|| format!("read_link {}", path.display()))?;
                let target = target.into_os_string().into_string().map_err(|t| {
                    anyhow!("UTF-8 olmayan symlink hedefi: {}", Path::new(&t).display())
                })?;
                EntryKind::Symlink { target }
            } else if ft.is_dir() {
                stack.push((path.clone(), rel.clone()));
                EntryKind::Dir
            } else if ft.is_file() {
                EntryKind::File
            } else {
                continue;
            };

            out.push(RawEntry {
                rel,
                size: if kind == EntryKind::Dir { 0 } else { md.len() },
                kind,
                mtime_ns: mtime_ns(&md)?,
                mode: mode(&md),
            });
        }
    }

    out.sort_by(|a, b| a.rel.cmp(&b.rel));
    Ok(out)
}

/// BLAKE3 of a file, read as a stream.
pub fn hash_file(path: &Path) -> Result<[u8; 32]> {
    let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut hasher = blake3::Hasher::new();
    hasher
        .update_reader(file)
        .with_context(|| format!("read {}", path.display()))?;
    Ok(*hasher.finalize().as_bytes())
}

#[cfg(unix)]
fn mtime_ns(md: &Metadata) -> Result<i64> {
    use std::os::unix::fs::MetadataExt;
    md.mtime()
        .checked_mul(1_000_000_000)
        .and_then(|s| s.checked_add(md.mtime_nsec()))
        .ok_or_else(|| anyhow!("mtime aralık dışı"))
}

#[cfg(not(unix))]
fn mtime_ns(md: &Metadata) -> Result<i64> {
    let t = md.modified()?;
    let ns = match t.duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => i64::try_from(d.as_nanos()),
        Err(e) => i64::try_from(e.duration().as_nanos()).map(|n| -n),
    };
    ns.map_err(|_| anyhow!("mtime aralık dışı"))
}

#[cfg(unix)]
fn mode(md: &Metadata) -> u32 {
    use std::os::unix::fs::MetadataExt;
    md.mode()
}

#[cfg(not(unix))]
fn mode(_md: &Metadata) -> u32 {
    0
}
