//! Import verification: what was expected on the target, what is there,
//! and the report written to `<vt_home>/reports/<session>.json`.

use std::collections::BTreeSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::apply::{ApplyPlan, ApplyReport};
use crate::fsutil::{create_private_dir_all, create_private_file};
use crate::manifest::{EntryKind, ProjectId};
use crate::scan::hash_file;

/// One file whose content was changed on the way to the target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransformRecord {
    pub dest: PathBuf,
    /// BLAKE3 of the source content.
    #[serde(with = "hex32")]
    pub before: [u8; 32],
    /// BLAKE3 of what is written to `dest`.
    #[serde(with = "hex32")]
    pub after: [u8; 32],
    /// What changed it, e.g. `json_paths`, `home_paths`, `memory_merge`.
    pub kind: String,
}

/// Result of an import, saved as `<vt_home>/reports/<session>.json`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifyReport {
    pub session_id: String,
    /// Target objects that should hold transferred content.
    pub expected: u32,
    /// Those whose content was found as expected after the apply.
    pub verified: u32,
    pub transformed: Vec<TransformRecord>,
    pub pending: Vec<ProjectId>,
    /// `(item, reason)` for everything not transferred or not changed.
    pub skipped: Vec<(String, String)>,
    pub problems: Vec<String>,
    /// What `apply` reported as a conflict: target files resolved by a
    /// rule or the user, replace scopes left alone (e.g. symlinked).
    #[serde(default)]
    pub conflicts: Vec<String>,
    /// Every object found as expected after the apply, with what it held;
    /// `verify` checks these again later.
    #[serde(default)]
    pub files: Vec<FileRecord>,
    /// The secret store item the import wrote, for `rollback`. Never the
    /// value: only its digest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret_write: Option<SecretWrite>,
}

/// A secret store write of an import. The previous value, if any, is in
/// `<vt_home>/backups/<session>/secrets.age`, encrypted with the bundle
/// passphrase.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretWrite {
    pub service: String,
    pub account: String,
    pub had_previous: bool,
    /// BLAKE3 of the value written.
    #[serde(with = "hex32")]
    pub written: [u8; 32],
}

/// One verified target object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileRecord {
    pub path: PathBuf,
    #[serde(flatten)]
    pub state: FileState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FileState {
    File {
        #[serde(with = "hex32")]
        hash: [u8; 32],
    },
    Symlink {
        target: PathBuf,
    },
}

/// Paths of `files` that no longer hold what was recorded (changed,
/// replaced, or gone), in order.
pub fn deviations(files: &[FileRecord]) -> Vec<PathBuf> {
    files
        .iter()
        .filter(|f| match &f.state {
            FileState::File { hash } => {
                !(fs::symlink_metadata(&f.path).is_ok_and(|m| m.is_file())
                    && hash_file(&f.path).is_ok_and(|h| h == *hash))
            }
            FileState::Symlink { target } => !fs::read_link(&f.path).is_ok_and(|t| t == *target),
        })
        .map(|f| f.path.clone())
        .collect()
}

/// Checks every object `plan` placed. `kept_target` holds the dests where
/// the user kept the target version: they are not expected to change.
/// Files must hash to their `expected_hash` (at `dest`, or at one of the
/// `KeepBoth` copies beside it), symlinks must point where planned, and
/// JSON writes must hold the planned document byte for byte.
pub fn verify_plan(
    plan: &ApplyPlan,
    applied: &ApplyReport,
    kept_target: &BTreeSet<PathBuf>,
    report: &mut VerifyReport,
) {
    for p in &plan.placements {
        if kept_target.contains(&p.dest) {
            continue;
        }
        let found = match &p.kind {
            EntryKind::Dir => {
                if !fs::metadata(&p.dest).is_ok_and(|m| m.is_dir()) {
                    report
                        .problems
                        .push(format!("dizin yok: {}", p.dest.display()));
                }
                continue;
            }
            EntryKind::Symlink { target } => {
                let check = |c: &Path| fs::read_link(c).is_ok_and(|t| t == Path::new(target));
                let state = FileState::Symlink {
                    target: PathBuf::from(target),
                };
                match found_at(&p.dest, applied, check) {
                    Some(path) => Ok(FileRecord { path, state }),
                    None => Err(format!(
                        "symlink beklendiği gibi değil: {}",
                        p.dest.display()
                    )),
                }
            }
            EntryKind::File => {
                let Some(want) = p.expected_hash else {
                    continue;
                };
                let check = |c: &Path| hash_file(c).is_ok_and(|h| h == want);
                match found_at(&p.dest, applied, check) {
                    Some(path) => Ok(FileRecord {
                        path,
                        state: FileState::File { hash: want },
                    }),
                    None => Err(format!(
                        "içerik beklenen hash ile eşleşmiyor: {}",
                        p.dest.display()
                    )),
                }
            }
        };
        report.expected += 1;
        match found {
            Ok(record) => {
                report.verified += 1;
                report.files.push(record);
            }
            Err(problem) => report.problems.push(problem),
        }
    }
    for (dest, value) in &plan.json_writes {
        report.expected += 1;
        let want = json_bytes(value);
        match fs::read(dest) {
            Ok(have) if have == want => {
                report.verified += 1;
                report.files.push(FileRecord {
                    path: dest.clone(),
                    state: FileState::File {
                        hash: hash_bytes(&want),
                    },
                });
            }
            _ => report
                .problems
                .push(format!("JSON beklendiği gibi değil: {}", dest.display())),
        }
    }
}

/// `dest`, or a `KeepBoth` copy written beside it, that satisfies `check`.
fn found_at(dest: &Path, applied: &ApplyReport, check: impl Fn(&Path) -> bool) -> Option<PathBuf> {
    if check(dest) {
        return Some(dest.to_path_buf());
    }
    applied
        .kept_both
        .iter()
        .filter(|c| c.parent() == dest.parent())
        .find(|c| check(c))
        .cloned()
}

/// BLAKE3 of `bytes`.
pub fn hash_bytes(bytes: &[u8]) -> [u8; 32] {
    *blake3::hash(bytes).as_bytes()
}

/// The exact bytes `apply` writes for a JSON document.
pub fn json_bytes(value: &serde_json::Value) -> Vec<u8> {
    let mut bytes = serde_json::to_vec_pretty(value).unwrap_or_default();
    bytes.push(b'\n');
    bytes
}

/// Where an import records its secret store write before making it.
fn secret_write_path(vt_home: &Path, session: &str) -> PathBuf {
    vt_home
        .join("backups")
        .join(session)
        .join("secret-write.json")
}

/// Persists `w` for `rollback` (dirs 0700, file 0600, written through a
/// temp file and a rename), before the store is changed, so a failure
/// later in the import still leaves the record behind.
pub fn write_secret_write(vt_home: &Path, session: &str, w: &SecretWrite) -> Result<()> {
    crate::apply::validate_session(session)?;
    let path = secret_write_path(vt_home, session);
    let dir = path.parent().context("kayıt yolunun klasörü yok")?;
    create_private_dir_all(dir).with_context(|| format!("oluşturulamadı: {}", dir.display()))?;
    let tmp = dir.join("secret-write.json.tmp");
    let _ = fs::remove_file(&tmp);
    let result = (|| -> Result<()> {
        let mut file = create_private_file(&tmp)?;
        file.write_all(&serde_json::to_vec_pretty(w)?)?;
        file.sync_all()?;
        fs::rename(&tmp, &path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result.with_context(|| format!("sır kaydı yazılamadı: {}", path.display()))
}

/// The secret write recorded by `write_secret_write`, if any.
pub fn read_secret_write(vt_home: &Path, session: &str) -> Result<Option<SecretWrite>> {
    let path = secret_write_path(vt_home, session);
    match fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .with_context(|| format!("sır kaydı bozuk: {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("okunamadı: {}", path.display())),
    }
}

/// Writes `report` to `<vt_home>/reports/<session>.json` (dirs 0700, file
/// 0600) and returns the path.
pub fn write_report(vt_home: &Path, report: &VerifyReport) -> Result<PathBuf> {
    crate::apply::validate_session(&report.session_id)?;
    let dir = vt_home.join("reports");
    create_private_dir_all(&dir).with_context(|| format!("oluşturulamadı: {}", dir.display()))?;
    let path = dir.join(format!("{}.json", report.session_id));
    let mut file = create_private_file(&path)
        .with_context(|| format!("oluşturulamadı: {}", path.display()))?;
    let mut bytes = serde_json::to_vec_pretty(report).context("rapor yazılamadı")?;
    bytes.push(b'\n');
    file.write_all(&bytes)
        .and_then(|()| file.sync_all())
        .with_context(|| format!("yazılamadı: {}", path.display()))?;
    Ok(path)
}

/// Serializes `[u8; 32]` as lowercase hex.
mod hex32 {
    use serde::de::Error;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &[u8; 32], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(blake3::Hash::from_bytes(*v).to_hex().as_str())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 32], D::Error> {
        let s = String::deserialize(d)?;
        blake3::Hash::from_hex(&s)
            .map(|h| *h.as_bytes())
            .map_err(D::Error::custom)
    }
}
