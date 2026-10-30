//! Merging source Claude data into what the target already has.

use std::collections::HashSet;

use serde_json::{Map, Value};
use vt_core::manifest::JsonConflict;

/// Keys of `~/.claude.json` whose entries are merged one by one.
const ENTRY_MERGED_KEYS: &[&str] = &["projects", "mcpServers"];
const REPO_PATHS_KEY: &str = "githubRepoPaths";
const OAUTH_ACCOUNT_KEY: &str = "oauthAccount";

/// Merges the transferable parts of a source `~/.claude.json` (paths already
/// rewritten) into the target. Only `projects`, `mcpServers`,
/// `githubRepoPaths` and, with `take_oauth_account`, `oauthAccount` are
/// touched; identity, caches and every other key stay the target's.
///
/// `projects` and `mcpServers` are merged per entry: new entries are added,
/// equal ones are left alone, and an entry whose value differs is returned
/// as a conflict (path `projects.<key>`) with the target unchanged.
/// `githubRepoPaths` arrays are united: target order first, then source
/// paths the target lacks. `oauthAccount` replaces the target value,
/// because the login token moves with it.
///
/// A null source has nothing to merge. Any other non-object source, or a
/// non-null non-object target, cannot be merged at all and comes back as a
/// single conflict with path `""` (the whole document), target unchanged.
pub fn merge_claude_json(
    target: &mut Value,
    source: &Value,
    take_oauth_account: bool,
) -> Vec<JsonConflict> {
    let mut conflicts = Vec::new();
    if source.is_null() {
        return conflicts;
    }
    if target.is_null() && source.is_object() {
        *target = Value::Object(Map::new());
    }
    let (Value::Object(dst), Value::Object(src)) = (&mut *target, source) else {
        conflicts.push(JsonConflict {
            path: String::new(),
            source: source.clone(),
            target: target.clone(),
        });
        return conflicts;
    };

    for &key in ENTRY_MERGED_KEYS {
        if let Some(src_value) = src.get(key) {
            merge_entries(dst, key, src_value, &mut conflicts, |_, _| None);
        }
    }
    if let Some(src_value) = src.get(REPO_PATHS_KEY) {
        merge_entries(dst, REPO_PATHS_KEY, src_value, &mut conflicts, union_arrays);
    }
    if take_oauth_account && let Some(account) = src.get(OAUTH_ACCOUNT_KEY) {
        dst.insert(OAUTH_ACCOUNT_KEY.to_owned(), account.clone());
    }
    conflicts
}

/// Top-level keys of a source `~/.claude.json` that `merge_claude_json`
/// leaves out (identity, caches, counters, and `oauthAccount` unless taken),
/// sorted. Names only: the values may be identifying.
pub fn claude_json_untransferred_keys(source: &Value, take_oauth_account: bool) -> Vec<String> {
    let Value::Object(src) = source else {
        return Vec::new();
    };
    src.keys()
        .filter(|k| {
            let k = k.as_str();
            !ENTRY_MERGED_KEYS.contains(&k)
                && k != REPO_PATHS_KEY
                && !(take_oauth_account && k == OAUTH_ACCOUNT_KEY)
        })
        .cloned()
        .collect()
}

/// Merges the object `src_value` into `dst[key]` entry by entry. When an
/// entry differs, `combine` may produce a merged value; otherwise the entry
/// is a conflict. A non-object on either side is a conflict on `key` itself.
fn merge_entries(
    dst: &mut Map<String, Value>,
    key: &str,
    src_value: &Value,
    conflicts: &mut Vec<JsonConflict>,
    combine: fn(&Value, &Value) -> Option<Value>,
) {
    let Some(dst_value) = dst.get_mut(key) else {
        dst.insert(key.to_owned(), src_value.clone());
        return;
    };
    let (Value::Object(dst_entries), Value::Object(src_entries)) = (&mut *dst_value, src_value)
    else {
        if dst_value != src_value {
            conflicts.push(JsonConflict {
                path: key.to_owned(),
                source: src_value.clone(),
                target: dst_value.clone(),
            });
        }
        return;
    };
    for (name, src_entry) in src_entries {
        match dst_entries.get_mut(name) {
            None => {
                dst_entries.insert(name.clone(), src_entry.clone());
            }
            Some(dst_entry) if dst_entry == src_entry => {}
            Some(dst_entry) => match combine(dst_entry, src_entry) {
                Some(merged) => *dst_entry = merged,
                None => conflicts.push(JsonConflict {
                    path: format!("{key}.{name}"),
                    source: src_entry.clone(),
                    target: dst_entry.clone(),
                }),
            },
        }
    }
}

/// Merges a source JSON settings document (`settings.json`, plugin
/// registries, `.mcp.json`; paths already rewritten) into the target's, key
/// by key: missing keys are added, objects are merged recursively, arrays
/// are united (target order first), and a differing scalar or type is a
/// conflict. For each conflict `take_source` decides: `true` puts the
/// source value in, `false` keeps the target's. Returns the conflicts.
///
/// A non-object on either side is one conflict on the whole document
/// (path `""`).
pub fn merge_json_settings(
    target: &mut Value,
    source: &Value,
    take_source: &mut dyn FnMut(&JsonConflict) -> bool,
) -> Vec<JsonConflict> {
    let mut conflicts = Vec::new();
    merge_value(target, source, "", take_source, &mut conflicts);
    conflicts
}

fn merge_value(
    target: &mut Value,
    source: &Value,
    path: &str,
    take_source: &mut dyn FnMut(&JsonConflict) -> bool,
    conflicts: &mut Vec<JsonConflict>,
) {
    if target == source {
        return;
    }
    match (&mut *target, source) {
        (Value::Object(dst), Value::Object(src)) => {
            for (key, src_value) in src {
                let child = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                match dst.get_mut(key) {
                    None => {
                        dst.insert(key.clone(), src_value.clone());
                    }
                    Some(dst_value) => {
                        merge_value(dst_value, src_value, &child, take_source, conflicts);
                    }
                }
            }
        }
        (Value::Array(_), Value::Array(_)) if !path.is_empty() => {
            if let Some(merged) = union_arrays(target, source) {
                *target = merged;
            }
        }
        _ => {
            let c = JsonConflict {
                path: path.to_owned(),
                source: source.clone(),
                target: target.clone(),
            };
            if take_source(&c) {
                *target = source.clone();
            }
            conflicts.push(c);
        }
    }
}

/// Union of two arrays: `target` in order, then `source` items it lacks.
fn union_arrays(target: &Value, source: &Value) -> Option<Value> {
    let (Value::Array(t), Value::Array(s)) = (target, source) else {
        return None;
    };
    let mut merged = t.clone();
    for item in s {
        if !merged.contains(item) {
            merged.push(item.clone());
        }
    }
    Some(Value::Array(merged))
}

/// Merges a source `MEMORY.md` into the target's: target lines unchanged and
/// in order, then source lines not found verbatim anywhere in the target.
///
/// Blank source lines survive only between two added lines, collapsed to
/// one. The result ends with a newline when the target does (or, for an
/// empty target, when the source does). A CRLF target gets CRLF line ends
/// on the added lines too.
pub fn merge_memory_md(target: &str, source: &str) -> String {
    let existing: HashSet<&str> = target.lines().collect();
    let mut added: Vec<&str> = Vec::new();
    let mut blank_pending = false;
    for line in source.lines() {
        if line.trim().is_empty() {
            blank_pending = !added.is_empty();
            continue;
        }
        if existing.contains(line) {
            continue;
        }
        if blank_pending {
            added.push("");
            blank_pending = false;
        }
        added.push(line);
    }
    if added.is_empty() {
        return target.to_owned();
    }

    let trailing_newline = if target.is_empty() {
        source.ends_with('\n')
    } else {
        target.ends_with('\n')
    };
    let eol = if target.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let mut out = target.to_owned();
    if !out.is_empty() && !out.ends_with('\n') {
        out.push_str(eol);
    }
    out.push_str(&added.join(eol));
    if trailing_newline {
        out.push_str(eol);
    }
    out
}
