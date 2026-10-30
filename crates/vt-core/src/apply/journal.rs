//! Append-only session journal and its reverse replay (rollback).
//!
//! `<vt_home>/journal/<session>.jsonl`, one JSON object per line, each line
//! synced before the step it announces. Rollback appends an `undo` line
//! after finishing each step, so undone steps always form a suffix of the
//! journal and a repeated or interrupted rollback never undoes twice.

use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use super::fsops::{lstat, move_path, sync_parent};
use super::{RollbackReport, backup_root, mirror_path, unique_path, validate_session};
use crate::fsutil::{create_private_dir_all, create_private_file};
use crate::scan::hash_file;

/// What a `place` step put at its destination.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Placed {
    /// Hex BLAKE3 of the written content.
    File {
        hash: String,
    },
    Symlink {
        target: PathBuf,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub(super) enum Op {
    /// `dest` is moved to `backup`.
    Backup {
        dest: PathBuf,
        backup: PathBuf,
    },
    /// New content is written to `tmp` and renamed to `dest`; `backup` is
    /// where the previous `dest` went, if there was one.
    Place {
        dest: PathBuf,
        backup: Option<PathBuf>,
        tmp: PathBuf,
        content: Placed,
    },
    Mkdir {
        dest: PathBuf,
    },
    /// Written by rollback: the step on line `seq` (0-based) is undone.
    Undo {
        seq: usize,
    },
}

pub(super) fn journal_path(vt_home: &Path, session: &str) -> PathBuf {
    vt_home.join("journal").join(format!("{session}.jsonl"))
}

pub(super) struct Journal {
    file: File,
    /// Line number (0-based) the next appended op gets.
    next_seq: usize,
}

impl Journal {
    /// Creates the journal of a new session; fails if it already exists.
    pub(super) fn create(vt_home: &Path, session: &str) -> Result<Self> {
        let path = journal_path(vt_home, session);
        let dir = path.parent().expect("journal path has a parent");
        create_private_dir_all(dir)
            .with_context(|| format!("oluşturulamadı: {}", dir.display()))?;
        let file = create_private_file(&path).map_err(|e| {
            if e.kind() == ErrorKind::AlreadyExists {
                anyhow::anyhow!("oturum zaten var: {session}")
            } else {
                anyhow::Error::new(e).context(format!("oluşturulamadı: {}", path.display()))
            }
        })?;
        sync_parent(&path);
        sync_parent(dir);
        Ok(Self { file, next_seq: 0 })
    }

    /// Opens a journal that already holds `lines` complete lines.
    fn open(path: &Path, lines: usize) -> Result<Self> {
        let file = OpenOptions::new()
            .append(true)
            .open(path)
            .with_context(|| format!("açılamadı: {}", path.display()))?;
        Ok(Self {
            file,
            next_seq: lines,
        })
    }

    /// Appends and syncs `op`; returns its line number.
    pub(super) fn append(&mut self, op: &Op) -> Result<usize> {
        let mut line = serde_json::to_vec(op).context("günlük satırı yazılamadı")?;
        line.push(b'\n');
        self.file
            .write_all(&line)
            .and_then(|()| self.file.sync_all())
            .context("günlük satırı yazılamadı")?;
        self.next_seq += 1;
        Ok(self.next_seq - 1)
    }
}

/// Reads the journal. A torn last line (crash while writing it) is cut
/// off, because the step it announced never started; a complete last line
/// missing only its newline gets one. Either way new lines append cleanly.
fn read_ops(path: &Path) -> Result<Vec<Op>> {
    let text = fs::read_to_string(path).with_context(|| match fs::exists(path) {
        Ok(false) => format!("oturum günlüğü bulunamadı: {}", path.display()),
        _ => format!("okunamadı: {}", path.display()),
    })?;
    let lines: Vec<&str> = text.split_terminator('\n').collect();
    let complete = text.is_empty() || text.ends_with('\n');
    let mut ops = Vec::with_capacity(lines.len());
    for (i, line) in lines.iter().enumerate() {
        match serde_json::from_str::<Op>(line) {
            Ok(op) => ops.push(op),
            Err(_) if i + 1 == lines.len() && !complete => {
                let keep = text.rfind('\n').map_or(0, |n| n + 1);
                OpenOptions::new()
                    .write(true)
                    .open(path)
                    .and_then(|f| f.set_len(keep as u64).and_then(|()| f.sync_all()))
                    .with_context(|| format!("onarılamadı: {}", path.display()))?;
                return Ok(ops);
            }
            Err(e) => bail!("günlük bozuk ({}. satır): {e}", i + 1),
        }
    }
    if !complete {
        let mut f = OpenOptions::new()
            .append(true)
            .open(path)
            .with_context(|| format!("açılamadı: {}", path.display()))?;
        f.write_all(b"\n")
            .and_then(|()| f.sync_all())
            .with_context(|| format!("onarılamadı: {}", path.display()))?;
    }
    Ok(ops)
}

/// Undoes a session: replays its journal backwards. `place` removes what
/// was written, `backup` moves the backup back (mode and mtime included),
/// `mkdir` removes the directory if it is empty. Idempotent; stops at the
/// first failure so the next run resumes where this one stopped.
///
/// A placed file that changed since it was written is not deleted but set
/// aside under `backups/<session>/rollback/`.
pub fn rollback(vt_home: &Path, session_id: &str) -> Result<RollbackReport> {
    validate_session(session_id)?;
    let path = journal_path(vt_home, session_id);
    let ops = read_ops(&path)?;
    let undone: BTreeSet<usize> = ops
        .iter()
        .filter_map(|op| match op {
            Op::Undo { seq } => Some(*seq),
            _ => None,
        })
        .collect();
    let aside_root = backup_root(vt_home, session_id).join("rollback");
    let mut journal = Journal::open(&path, ops.len())?;
    let mut report = RollbackReport::default();
    for (seq, op) in ops.iter().enumerate().rev() {
        if matches!(op, Op::Undo { .. }) || undone.contains(&seq) {
            continue;
        }
        undo(op, &aside_root, &mut report)
            .with_context(|| format!("geri alma {}. adımda durdu", seq + 1))?;
        journal.append(&Op::Undo { seq })?;
    }
    Ok(report)
}

fn undo(op: &Op, aside_root: &Path, report: &mut RollbackReport) -> Result<()> {
    match op {
        Op::Place {
            dest, tmp, content, ..
        } => {
            remove_if_present(tmp)?;
            let Some(md) = lstat(dest)? else {
                return Ok(());
            };
            let ours = match content {
                Placed::File { hash } => md.is_file() && hex(&hash_file(dest)?) == *hash,
                Placed::Symlink { target } => {
                    md.file_type().is_symlink() && fs::read_link(dest)? == *target
                }
            };
            if ours {
                fs::remove_file(dest).with_context(|| format!("silinemedi: {}", dest.display()))?;
                sync_parent(dest);
                report.removed += 1;
            } else {
                set_aside(dest, aside_root, report)?;
            }
        }
        Op::Backup { dest, backup } => {
            if lstat(backup)?.is_none() {
                return Ok(());
            }
            if lstat(dest)?.is_some() && !same_object(dest, backup)? {
                set_aside(dest, aside_root, report)?;
            }
            if let Some(parent) = dest.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("oluşturulamadı: {}", parent.display()))?;
            }
            move_path(backup, dest)?;
            report.restored += 1;
        }
        Op::Mkdir { dest } => {
            let Some(md) = lstat(dest)? else {
                return Ok(());
            };
            let empty = md.is_dir()
                && fs::read_dir(dest)
                    .with_context(|| format!("okunamadı: {}", dest.display()))?
                    .next()
                    .is_none();
            if empty {
                fs::remove_dir(dest).with_context(|| format!("silinemedi: {}", dest.display()))?;
                sync_parent(dest);
                report.dirs_removed += 1;
            }
        }
        Op::Undo { .. } => {}
    }
    Ok(())
}

/// Moves something rollback must not delete out of the way.
fn set_aside(path: &Path, aside_root: &Path, report: &mut RollbackReport) -> Result<()> {
    let to = unique_path(&mirror_path(aside_root, path))?;
    let parent = to.parent().expect("mirror path has a parent");
    create_private_dir_all(parent)
        .with_context(|| format!("oluşturulamadı: {}", parent.display()))?;
    move_path(path, &to)?;
    report.set_aside.push(to);
    Ok(())
}

/// Same content (files) or same target (symlinks).
fn same_object(a: &Path, b: &Path) -> Result<bool> {
    let (Some(ma), Some(mb)) = (lstat(a)?, lstat(b)?) else {
        return Ok(false);
    };
    if ma.file_type().is_symlink() && mb.file_type().is_symlink() {
        return Ok(fs::read_link(a)? == fs::read_link(b)?);
    }
    Ok(ma.is_file() && mb.is_file() && hash_file(a)? == hash_file(b)?)
}

fn remove_if_present(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Err(e) if e.kind() != ErrorKind::NotFound => {
            Err(e).with_context(|| format!("silinemedi: {}", path.display()))
        }
        _ => Ok(()),
    }
}

pub(super) fn hex(h: &[u8; 32]) -> String {
    blake3::Hash::from_bytes(*h).to_hex().to_string()
}
