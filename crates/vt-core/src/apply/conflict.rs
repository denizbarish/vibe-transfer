//! Conflict decisions and the `KeepBoth` naming rule.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::manifest::JsonConflict;

/// How an import treats a target directory that already has content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Source and target content live side by side.
    Merge,
    /// Source content wins; extra target files go to the backup.
    Replace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    KeepSource,
    KeepTarget,
    /// Writes the source next to the target under a suffixed name.
    /// Not valid for JSON keys, where it acts as `KeepTarget`.
    KeepBoth,
}

/// Asked once per conflicting target file or JSON key.
pub trait ConflictResolver {
    fn file(&mut self, dest: &Path) -> Resolution;
    fn json(&mut self, c: &JsonConflict) -> Resolution;
}

/// Asks `resolver` about a JSON key. `KeepBoth` has no meaning for a single
/// key, so it is turned into `KeepTarget` and a note is pushed to `notes`.
pub fn resolve_json(
    resolver: &mut dyn ConflictResolver,
    c: &JsonConflict,
    notes: &mut Vec<String>,
) -> Resolution {
    match resolver.json(c) {
        Resolution::KeepBoth => {
            notes.push(format!(
                "{}: JSON anahtarında ikisini tutmak mümkün değil, hedef korundu",
                c.path
            ));
            Resolution::KeepTarget
        }
        other => other,
    }
}

/// `<stem>.from-<host><.ext>` beside `dest`; attempt `n >= 2` appends `-n`
/// to the host part.
pub(crate) fn keep_both_name(dest: &Path, host: &str, n: u32) -> PathBuf {
    let host = sanitize_host(host);
    let tag = if n < 2 {
        format!("from-{host}")
    } else {
        format!("from-{host}-{n}")
    };
    let stem = dest.file_stem().unwrap_or_default();
    let mut name = OsString::from(stem);
    name.push(".");
    name.push(&tag);
    if let Some(ext) = dest.extension() {
        name.push(".");
        name.push(ext);
    }
    dest.with_file_name(name)
}

/// Keeps the host name usable as part of a file name.
fn sanitize_host(host: &str) -> String {
    let s: String = host
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if s.is_empty() { "kaynak".into() } else { s }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        let p = Path::new("/d/settings.json");
        assert_eq!(
            keep_both_name(p, "src", 1),
            Path::new("/d/settings.from-src.json")
        );
        assert_eq!(
            keep_both_name(p, "src", 3),
            Path::new("/d/settings.from-src-3.json")
        );
        assert_eq!(
            keep_both_name(Path::new("/d/.mcp.json"), "a.b/c", 1),
            Path::new("/d/.mcp.from-a_b_c.json")
        );
        assert_eq!(
            keep_both_name(Path::new("/d/CLAUDE"), "", 1),
            Path::new("/d/CLAUDE.from-kaynak")
        );
    }
}
