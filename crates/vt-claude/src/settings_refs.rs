//! Files under the Claude dir that `settings.json` points at: the status
//! line script and hook commands. A file Claude runs from there must move
//! with the settings, or the target's status line and hooks break.

use std::fs;
use std::path::Path;

use serde_json::Value;

/// Paths (relative to the Claude dir) named in `settings.json` commands.
#[derive(Debug, Default)]
pub(crate) struct SettingsRefs {
    /// From `statusLine.command`: classified as settings.
    pub status_line: Vec<String>,
    /// From `statusLine.command` and every hook `command`.
    pub all: Vec<String>,
}

/// Reads `<claude_dir>/settings.json`; a missing or unparsable file names
/// nothing.
pub(crate) fn settings_refs(claude_dir: &Path, home: &Path) -> SettingsRefs {
    let mut out = SettingsRefs::default();
    let Ok(bytes) = fs::read(claude_dir.join("settings.json")) else {
        return out;
    };
    let Ok(doc) = serde_json::from_slice::<Value>(&bytes) else {
        return out;
    };
    if let Some(cmd) = doc
        .get("statusLine")
        .and_then(|s| s.get("command"))
        .and_then(Value::as_str)
    {
        out.status_line = command_paths(cmd, claude_dir, home);
    }
    out.all = out.status_line.clone();
    if let Some(hooks) = doc.get("hooks") {
        let mut cmds = Vec::new();
        collect_commands(hooks, &mut cmds);
        for cmd in cmds {
            out.all.extend(command_paths(cmd, claude_dir, home));
        }
    }
    out.all.sort();
    out.all.dedup();
    out
}

/// Every `command` string anywhere in `v`.
fn collect_commands<'a>(v: &'a Value, out: &mut Vec<&'a str>) {
    match v {
        Value::Object(m) => {
            for (k, child) in m {
                match (k.as_str(), child) {
                    ("command", Value::String(s)) => out.push(s),
                    _ => collect_commands(child, out),
                }
            }
        }
        Value::Array(items) => items.iter().for_each(|i| collect_commands(i, out)),
        _ => {}
    }
}

/// Words of `cmd` that name a path below `claude_dir`, as relative paths.
/// `~/`, `$HOME/` and `${HOME}/` are expanded; quotes are stripped.
fn command_paths(cmd: &str, claude_dir: &Path, home: &Path) -> Vec<String> {
    let home = home.to_string_lossy();
    cmd.split_whitespace()
        .filter_map(|word| {
            let word = word.trim_matches(|c| c == '"' || c == '\'' || c == ';');
            let expanded = ["~/", "$HOME/", "${HOME}/"]
                .iter()
                .find_map(|p| word.strip_prefix(p))
                .map_or_else(|| word.to_owned(), |rest| format!("{home}/{rest}"));
            let rel = Path::new(&expanded).strip_prefix(claude_dir).ok()?;
            let rel = rel.to_str()?;
            (!rel.is_empty() && !rel.split('/').any(|c| c == "..")).then(|| rel.to_owned())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_words_below_the_claude_dir_are_found() {
        let home = Path::new("/h");
        let dir = Path::new("/h/.claude");
        assert_eq!(
            command_paths(
                "bash ~/.claude/a.sh '$HOME/.claude/b/c.sh' /h/.claude/d \"${HOME}/.claude/e\" /h/x ~/.claude/../f",
                dir,
                home
            ),
            vec!["a.sh", "b/c.sh", "d", "e"]
        );
    }
}
