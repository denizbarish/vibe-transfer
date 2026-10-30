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

/// An entry left out of a scan: a special file that cannot be transferred,
/// or one that vanished or could not be read during the walk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedEntry {
    pub rel: String,
    /// `socket`, `fifo`, `device`, `other`, [`NON_UTF8_TARGET`],
    /// [`NON_UTF8_NAME`] (then `rel` holds the lossy name), [`VANISHED`], or
    /// [`UNREADABLE_DIR`] followed by `: ` and the error.
    pub reason: String,
}

/// `SkippedEntry::reason` for a name that is not valid UTF-8.
pub const NON_UTF8_NAME: &str = "utf8-olmayan-ad";
/// `SkippedEntry::reason` for a symlink whose target is not valid UTF-8.
pub const NON_UTF8_TARGET: &str = "utf8-olmayan-hedef";

/// `SkippedEntry::reason` for an entry that disappeared during the walk.
pub const VANISHED: &str = "tarama sırasında silindi";
/// Prefix of `SkippedEntry::reason` for an entry that could not be read;
/// the I/O error follows after `: `.
pub const UNREADABLE_DIR: &str = "okunamadı";

/// Result of [`scan_tree`]: transferable entries plus skipped special files.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScanResult {
    pub entries: Vec<RawEntry>,
    pub skipped: Vec<SkippedEntry>,
}

/// Walks `root` without following symlinks and returns its contents sorted
/// by `rel`. The root itself is not included.
///
/// `exclude` gets the `/`-separated NFC `rel` path; an excluded directory is
/// skipped with its whole subtree. Symlinks are reported with their target.
/// Sockets, FIFOs and device files are not transferable: they are listed in
/// `skipped` so callers can report them, as are non-UTF-8 names (never
/// descended into).
///
/// The tree may change while it is walked (Claude deletes and rotates files
/// as it runs): an entry that disappears is skipped with [`VANISHED`], and a
/// subdirectory or entry that cannot be read is skipped with a reason that
/// starts with [`UNREADABLE_DIR`]; a skipped directory is left out with its
/// whole subtree. Only a failure to read `root` itself is an error.
/// `mode` holds only the permission bits (`& 0o7777`).
pub fn scan_tree(root: &Path, exclude: &dyn Fn(&str) -> bool) -> Result<ScanResult> {
    let mut out = Vec::new();
    let mut skipped = Vec::new();
    // A directory's own entry is kept only once its listing succeeds.
    let mut stack: Vec<(PathBuf, String, Option<RawEntry>)> =
        vec![(root.to_path_buf(), String::new(), None)];

    while let Some((dir, prefix, own)) = stack.pop() {
        let read = match (fs::read_dir(&dir), own) {
            (Ok(read), own) => {
                out.extend(own);
                read
            }
            (Err(e), None) => {
                return Err(e).with_context(|| format!("read_dir {}", dir.display()));
            }
            (Err(e), Some(_)) => {
                skipped.push(skip_for_io(prefix, &e));
                continue;
            }
        };
        for item in read {
            let item = match item {
                Ok(item) => item,
                Err(e) => {
                    // The listing broke off; what was read so far is kept.
                    // An empty `rel` means the root.
                    skipped.push(skip_for_io(prefix.clone(), &e));
                    break;
                }
            };
            let path = item.path();
            let join = |name: &str| {
                if prefix.is_empty() {
                    name.to_owned()
                } else {
                    format!("{prefix}/{name}")
                }
            };
            let name = match item.file_name().into_string() {
                Ok(name) => name,
                Err(raw) => {
                    // Cannot be named in a manifest: report it and move on.
                    skipped.push(SkippedEntry {
                        rel: join(&raw.to_string_lossy()),
                        reason: NON_UTF8_NAME.into(),
                    });
                    continue;
                }
            };
            let name: String = name.nfc().collect();
            let rel = join(&name);
            if exclude(&rel) {
                continue;
            }

            match stat_entry(&path, rel.clone()) {
                Ok(Ok(raw)) if raw.kind == EntryKind::Dir => {
                    stack.push((path, raw.rel.clone(), Some(raw)));
                }
                Ok(Ok(raw)) => out.push(raw),
                Ok(Err(skip)) => skipped.push(skip),
                Err(e) => match e.root_cause().downcast_ref::<std::io::Error>() {
                    Some(io) => skipped.push(skip_for_io(rel, io)),
                    None => skipped.push(SkippedEntry {
                        rel,
                        reason: format!("{UNREADABLE_DIR}: {e:#}"),
                    }),
                },
            }
        }
    }

    out.sort_by(|a, b| a.rel.cmp(&b.rel));
    skipped.sort_by(|a, b| a.rel.cmp(&b.rel));
    Ok(ScanResult {
        entries: out,
        skipped,
    })
}

/// Skip record for an I/O failure on `rel` found during the walk.
fn skip_for_io(rel: String, e: &std::io::Error) -> SkippedEntry {
    let reason = if e.kind() == std::io::ErrorKind::NotFound {
        VANISHED.to_owned()
    } else {
        format!("{UNREADABLE_DIR}: {e}")
    };
    SkippedEntry { rel, reason }
}

/// Describes one path as `rel`, without following a symlink. Special files
/// and symlinks with a non-UTF-8 target come back as `Ok(Err(skipped))`.
pub fn stat_entry(path: &Path, rel: String) -> Result<Result<RawEntry, SkippedEntry>> {
    let md = fs::symlink_metadata(path).with_context(|| format!("metadata {}", path.display()))?;
    let ft = md.file_type();
    let kind = if ft.is_symlink() {
        let target =
            fs::read_link(path).with_context(|| format!("read_link {}", path.display()))?;
        match target.into_os_string().into_string() {
            Ok(target) => EntryKind::Symlink { target },
            Err(_) => {
                return Ok(Err(SkippedEntry {
                    rel,
                    reason: NON_UTF8_TARGET.into(),
                }));
            }
        }
    } else if ft.is_dir() {
        EntryKind::Dir
    } else if ft.is_file() {
        EntryKind::File
    } else {
        return Ok(Err(SkippedEntry {
            rel,
            reason: special_kind(&md).into(),
        }));
    };
    Ok(Ok(RawEntry {
        rel,
        size: if kind == EntryKind::Dir { 0 } else { md.len() },
        kind,
        mtime_ns: mtime_ns(&md)?,
        mode: mode(&md),
    }))
}

#[cfg(unix)]
fn special_kind(md: &Metadata) -> &'static str {
    use std::os::unix::fs::FileTypeExt;
    let ft = md.file_type();
    if ft.is_socket() {
        "socket"
    } else if ft.is_fifo() {
        "fifo"
    } else if ft.is_block_device() || ft.is_char_device() {
        "device"
    } else {
        "other"
    }
}

#[cfg(not(unix))]
fn special_kind(_md: &Metadata) -> &'static str {
    "other"
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
    md.mode() & 0o7777
}

#[cfg(not(unix))]
fn mode(_md: &Metadata) -> u32 {
    0
}
