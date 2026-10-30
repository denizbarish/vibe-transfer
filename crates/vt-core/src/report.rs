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
        let problem = match &p.kind {
            EntryKind::Dir => {
                if fs::metadata(&p.dest).is_ok_and(|m| m.is_dir()) {
                    None
                } else {
                    Some(format!("dizin yok: {}", p.dest.display()))
                }
            }
            EntryKind::Symlink { target } => match fs::read_link(&p.dest) {
                Ok(t) if t == Path::new(target) => None,
                _ if kept_both_matches(&p.dest, applied, |c| {
                    fs::read_link(c).is_ok_and(|t| t == Path::new(target))
                }) =>
                {
                    None
                }
                _ => Some(format!(
                    "symlink beklendiği gibi değil: {}",
                    p.dest.display()
                )),
            },
            EntryKind::File => {
                let Some(want) = p.expected_hash else {
                    continue;
                };
                let matches = |c: &Path| hash_file(c).is_ok_and(|h| h == want);
                if matches(&p.dest) || kept_both_matches(&p.dest, applied, matches) {
                    None
                } else {
                    Some(format!(
                        "içerik beklenen hash ile eşleşmiyor: {}",
                        p.dest.display()
                    ))
                }
            }
        };
        if p.kind == EntryKind::Dir {
            if let Some(problem) = problem {
                report.problems.push(problem);
            }
            continue;
        }
        report.expected += 1;
        match problem {
            None => report.verified += 1,
            Some(problem) => report.problems.push(problem),
        }
    }
    for (dest, value) in &plan.json_writes {
        report.expected += 1;
        let want = json_bytes(value);
        match fs::read(dest) {
            Ok(have) if have == want => report.verified += 1,
            _ => report
                .problems
                .push(format!("JSON beklendiği gibi değil: {}", dest.display())),
        }
    }
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

/// Whether a `KeepBoth` copy written beside `dest` satisfies `check`.
fn kept_both_matches(dest: &Path, applied: &ApplyReport, check: impl Fn(&Path) -> bool) -> bool {
    applied
        .kept_both
        .iter()
        .filter(|c| c.parent() == dest.parent())
        .any(|c| check(c))
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
