use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use unicode_normalization::UnicodeNormalization;
use vt_claude::{
    CLAUDE_REWRITE_JSONL_CWD, claude_json_untransferred_keys, memory_slug_for, merge_claude_json,
    merge_memory_md, rewrite_json_paths, rewrite_jsonl_cwd, target_slug_dir,
};
use vt_core::pathcodec::claude_slug;

const SRC: &str = "/Users/src";
const DST: &str = "/Users/dst";

fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, PathBuf> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), PathBuf::from(v)))
        .collect()
}

#[test]
fn jsonl_cwd_rewrite_is_off() {
    // Ruling R1: the spike was not run, so session lines keep their cwd.
    const { assert!(!CLAUDE_REWRITE_JSONL_CWD) };
}

#[test]
fn slug_rename_merges_nfc_nfd_variants() {
    let nfc: String = "/Users/src/Kişisel".nfc().collect();
    let nfd: String = "/Users/src/Kişisel".nfd().collect();
    assert_ne!(nfc, nfd);
    assert_ne!(claude_slug(&nfc), claude_slug(&nfd));

    let mapping = map(&[(&nfc, "/Users/dst/Kişisel")]);
    let a = target_slug_dir(std::slice::from_ref(&nfc), &mapping);
    let b = target_slug_dir(std::slice::from_ref(&nfd), &mapping);
    let expected = claude_slug(&"/Users/dst/Kişisel".nfc().collect::<String>());
    assert_eq!(a.as_deref(), Some(expected.as_str()));
    assert_eq!(a, b);
}

#[test]
fn slug_maps_subdirectory_cwds_by_prefix() {
    let mapping = map(&[("/Users/src/app", "/Users/dst/work/app")]);
    let cwds = vec!["/Users/src/app/web".to_string()];
    assert_eq!(
        target_slug_dir(&cwds, &mapping),
        Some(claude_slug("/Users/dst/work/app/web"))
    );
    // `/Users/src/app2` is not below `/Users/src/app`.
    assert_eq!(
        target_slug_dir(&["/Users/src/app2".to_string()], &mapping),
        None
    );
}

#[test]
fn slug_dir_with_diverging_or_unmapped_cwds_is_none() {
    let mapping = map(&[
        ("/Users/src/a", "/Users/dst/a"),
        ("/Users/src/b", "/Users/dst/b"),
    ]);
    let diverging = vec!["/Users/src/a".to_string(), "/Users/src/b".to_string()];
    assert_eq!(target_slug_dir(&diverging, &mapping), None);

    let partly = vec!["/Users/src/a".to_string(), "/Users/src/x".to_string()];
    assert_eq!(target_slug_dir(&partly, &mapping), None);

    assert_eq!(target_slug_dir(&[], &mapping), None);

    let same = vec!["/Users/src/a".to_string(), "/Users/src/a".to_string()];
    assert_eq!(
        target_slug_dir(&same, &mapping),
        Some(claude_slug("/Users/dst/a"))
    );
}

#[test]
fn memory_slug_uses_git_root() {
    assert_eq!(
        memory_slug_for(Path::new("/Users/dst/code/acme")),
        "-Users-dst-code-acme"
    );
}

#[test]
fn rewrite_json_paths_settings_hooks() {
    let mut v = json!({
        "hooks": {
            "PreToolUse": [{
                "matcher": "Bash",
                "hooks": [{ "type": "command", "command": "bash /Users/src/.claude/hooks/a.sh" }]
            }]
        },
        "permissions": { "allow": ["Read(/Users/src/code/**)", "Bash(ls)"] },
        "note": "/Users/src2/x and prefix/Users/srcfoo",
        "n": 3
    });
    let out = rewrite_json_paths(&mut v, SRC, DST, &BTreeMap::new());
    assert_eq!(out.count, 2);
    assert!(out.conflicts.is_empty());
    assert_eq!(
        v["hooks"]["PreToolUse"][0]["hooks"][0]["command"],
        "bash /Users/dst/.claude/hooks/a.sh"
    );
    assert_eq!(v["permissions"]["allow"][0], "Read(/Users/dst/code/**)");
    assert_eq!(v["permissions"]["allow"][1], "Bash(ls)");
    assert_eq!(v["note"], "/Users/src2/x and prefix/Users/srcfoo");
}

#[test]
fn rewrite_json_paths_projects_keys_and_repo_paths() {
    let mut v = json!({
        "projects": {
            "/Users/src/code/acme": { "lastSessionId": "s1", "x": "/Users/src/code/acme/a", "cmd": "run /Users/src/code/acme/a" },
            "/Users/src/code/acme/sub": {},
            "/Users/src/other": {},
            "/opt/elsewhere": {}
        },
        "githubRepoPaths": { "acme/app": ["/Users/src/code/acme", "/opt/elsewhere", "/Users/src/code/acme"] },
        "mcpServers": { "m": { "command": "/Users/src/bin/m" } }
    });
    let pm = map(&[("/Users/src/code/acme", "/Users/dst/work/acme")]);
    let out = rewrite_json_paths(&mut v, SRC, DST, &pm);
    assert!(out.conflicts.is_empty());

    let keys: Vec<&str> = v["projects"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        [
            "/Users/dst/other",
            "/Users/dst/work/acme",
            "/Users/dst/work/acme/sub",
            "/opt/elsewhere"
        ]
    );
    assert_eq!(v["projects"]["/Users/dst/work/acme"]["lastSessionId"], "s1");
    // A whole-path value goes through the project mapping; text with an
    // embedded path gets the home rewrite only.
    assert_eq!(
        v["projects"]["/Users/dst/work/acme"]["x"],
        "/Users/dst/work/acme/a"
    );
    assert_eq!(
        v["projects"]["/Users/dst/work/acme"]["cmd"],
        "run /Users/dst/code/acme/a"
    );
    assert_eq!(
        v["githubRepoPaths"]["acme/app"],
        json!(["/Users/dst/work/acme", "/opt/elsewhere"])
    );
    assert_eq!(v["mcpServers"]["m"]["command"], "/Users/dst/bin/m");
    // 3 project keys + 2 values inside + 2 repo paths (one then deduplicated)
    // + 1 mcp command.
    assert_eq!(out.count, 8);
}

#[test]
fn rewrite_json_paths_whole_path_values_use_project_map() {
    // installed_plugins.json style.
    let mut v = json!({
        "plugins": { "p@m": [{
            "installPath": "/Users/src/.claude/plugins/cache/p",
            "projectPath": "/Users/src/code/acme/web",
            "note": "/Users/src/code/acme cd /Users/src/code/acme"
        }] }
    });
    let pm = map(&[("/Users/src/code/acme", "/Users/dst/work/acme")]);
    let out = rewrite_json_paths(&mut v, SRC, DST, &pm);
    let entry = &v["plugins"]["p@m"][0];
    assert_eq!(entry["installPath"], "/Users/dst/.claude/plugins/cache/p");
    assert_eq!(entry["projectPath"], "/Users/dst/work/acme/web");
    assert_eq!(
        entry["note"],
        "/Users/dst/code/acme cd /Users/dst/code/acme"
    );
    assert_eq!(out.count, 3);
}

#[test]
fn rewrite_json_paths_collapses_equal_nfc_nfd_project_keys() {
    let nfc: String = "/Users/src/Kişisel".nfc().collect();
    let nfd: String = "/Users/src/Kişisel".nfd().collect();
    let mut v = json!({ "projects": {
        nfc.clone(): { "hasTrustDialogAccepted": true },
        nfd.clone(): { "hasTrustDialogAccepted": true }
    } });
    let pm = map(&[(&nfc, "/Users/dst/Kişisel")]);
    let out = rewrite_json_paths(&mut v, SRC, DST, &pm);
    assert!(out.conflicts.is_empty());
    assert_eq!(
        v["projects"],
        json!({ "/Users/dst/Kişisel": { "hasTrustDialogAccepted": true } })
    );
    assert_eq!(out.count, 2);
}

#[test]
fn rewrite_json_paths_reports_differing_colliding_project_keys() {
    let nfc: String = "/Users/src/Kişisel".nfc().collect();
    let nfd: String = "/Users/src/Kişisel".nfd().collect();
    let mut v = json!({ "projects": {
        nfc.clone(): { "lastSessionId": "a" },
        nfd.clone(): { "lastSessionId": "b" }
    } });
    let pm = map(&[(&nfc, "/Users/dst/Kişisel")]);
    let out = rewrite_json_paths(&mut v, SRC, DST, &pm);

    // The first by old key is kept; no source-path key is left behind.
    let (first, second) = if nfc < nfd { ("a", "b") } else { ("b", "a") };
    assert_eq!(
        v["projects"],
        json!({ "/Users/dst/Kişisel": { "lastSessionId": first } })
    );
    assert_eq!(out.conflicts.len(), 1);
    assert_eq!(out.conflicts[0].path, "projects./Users/dst/Kişisel");
    assert_eq!(out.conflicts[0].source, json!({ "lastSessionId": second }));
    assert_eq!(out.conflicts[0].target, json!({ "lastSessionId": first }));
}

#[test]
fn rewrite_json_paths_root_home_is_noop() {
    let mut v = json!({ "a": "/Users/src/x" });
    assert_eq!(
        rewrite_json_paths(&mut v, "/", DST, &BTreeMap::new()).count,
        0
    );
    assert_eq!(v["a"], "/Users/src/x");
}

#[test]
fn merge_claude_json_keeps_target_identity() {
    let mut target = json!({
        "userID": "t-user",
        "machineID": "t-machine",
        "oauthAccount": { "accountUuid": "t" },
        "numStartups": 4,
        "projects": {
            "/Users/dst/a": { "k": 1 },
            "/Users/dst/c": { "k": "target" }
        },
        "githubRepoPaths": { "acme/a": ["/Users/dst/a"] },
        "mcpServers": { "same": { "command": "x" } }
    });
    let source = json!({
        "userID": "s-user",
        "machineID": "s-machine",
        "oauthAccount": { "accountUuid": "s" },
        "numStartups": 99,
        "projects": {
            "/Users/dst/a": { "k": 1 },
            "/Users/dst/b": { "k": 2 },
            "/Users/dst/c": { "k": "source" }
        },
        "githubRepoPaths": {
            "acme/a": ["/Users/dst/a2", "/Users/dst/a"],
            "acme/b": ["/Users/dst/b"]
        },
        "mcpServers": { "same": { "command": "x" }, "new": { "command": "y" } }
    });
    let conflicts = merge_claude_json(&mut target, &source, false);

    assert_eq!(target["userID"], "t-user");
    assert_eq!(target["machineID"], "t-machine");
    assert_eq!(target["numStartups"], 4);
    assert_eq!(target["oauthAccount"]["accountUuid"], "t");
    let keys: Vec<&String> = target["projects"].as_object().unwrap().keys().collect();
    assert_eq!(keys, ["/Users/dst/a", "/Users/dst/b", "/Users/dst/c"]);
    assert_eq!(target["projects"]["/Users/dst/c"]["k"], "target");
    assert_eq!(
        target["githubRepoPaths"],
        json!({ "acme/a": ["/Users/dst/a", "/Users/dst/a2"], "acme/b": ["/Users/dst/b"] })
    );
    assert_eq!(target["mcpServers"]["new"]["command"], "y");

    assert_eq!(conflicts.len(), 1);
    assert_eq!(conflicts[0].path, "projects./Users/dst/c");
    assert_eq!(conflicts[0].source, json!({ "k": "source" }));
    assert_eq!(conflicts[0].target, json!({ "k": "target" }));
}

#[test]
fn merge_claude_json_takes_oauth_account_when_asked() {
    let mut target = json!({ "oauthAccount": { "accountUuid": "t" } });
    let source = json!({ "oauthAccount": { "accountUuid": "s" }, "mcpServers": { "m": {} } });
    let conflicts = merge_claude_json(&mut target, &source, true);
    assert!(conflicts.is_empty());
    assert_eq!(target["oauthAccount"]["accountUuid"], "s");
    assert_eq!(target["mcpServers"], json!({ "m": {} }));

    let mut empty = Value::Null;
    merge_claude_json(&mut empty, &source, false);
    assert_eq!(empty, json!({ "mcpServers": { "m": {} } }));
}

#[test]
fn merge_claude_json_unmergeable_document_is_a_conflict() {
    let source = json!({ "projects": { "/Users/dst/a": {} } });
    let mut target = json!([1, 2]);
    let conflicts = merge_claude_json(&mut target, &source, false);
    assert_eq!(target, json!([1, 2]));
    assert_eq!(conflicts.len(), 1);
    assert_eq!(conflicts[0].path, "");
    assert_eq!(conflicts[0].source, source);
    assert_eq!(conflicts[0].target, json!([1, 2]));

    let mut target = json!({ "userID": "t" });
    let conflicts = merge_claude_json(&mut target, &json!("broken"), false);
    assert_eq!(target, json!({ "userID": "t" }));
    assert_eq!(conflicts.len(), 1);
    assert_eq!(conflicts[0].path, "");

    // A null source has nothing to merge.
    assert!(merge_claude_json(&mut target, &Value::Null, false).is_empty());
}

#[test]
fn merge_memory_md_dedup() {
    let target = "# Memory\n- [A](a.md): one\n\n- [B](b.md): two\n";
    let source = "# Memory\n- [A](a.md): one\n- [C](c.md): three\n\n\n\n- [D](d.md): four\n\n\n";
    assert_eq!(
        merge_memory_md(target, source),
        "# Memory\n- [A](a.md): one\n\n- [B](b.md): two\n- [C](c.md): three\n\n- [D](d.md): four\n"
    );
}

#[test]
fn merge_memory_md_keeps_target_newline_convention() {
    assert_eq!(merge_memory_md("a\nb", "b\nc\n"), "a\nb\nc");
    assert_eq!(merge_memory_md("a\n", "a\n"), "a\n");
    assert_eq!(merge_memory_md("a\n", "\n\nb\n\n"), "a\nb\n");
    assert_eq!(merge_memory_md("", "x\ny\n"), "x\ny\n");
    assert_eq!(
        merge_memory_md("a\r\nb\r\n", "b\nc\n\nd\n"),
        "a\r\nb\r\nc\r\n\r\nd\r\n"
    );
    assert_eq!(merge_memory_md("a\r\nb", "c\n"), "a\r\nb\r\nc");
}

#[test]
fn rewrite_jsonl_cwd_only_cwd_field() {
    let line = r#"{"type":"user", "message":{"cwd":"/Users/src/nested","text":"aé\/b"},"cwd" : "/Users/src/code/acme","n":1.50}"#;
    let mapper = |old: &str| {
        old.strip_prefix("/Users/src/code/acme")
            .map(|rest| format!("/Users/dst/work/acme{rest}"))
    };
    let out = rewrite_jsonl_cwd(line, &mapper).unwrap();
    assert_eq!(
        out,
        r#"{"type":"user", "message":{"cwd":"/Users/src/nested","text":"aé\/b"},"cwd" : "/Users/dst/work/acme","n":1.50}"#
    );

    // Escaped cwd value: decoded before mapping.
    let escaped = r#"{"cwd":"\/Users\/src\/code\/acme"}"#;
    assert_eq!(
        rewrite_jsonl_cwd(escaped, &mapper).as_deref(),
        Some(r#"{"cwd":"/Users/dst/work/acme"}"#)
    );

    let unmapped = r#"{"cwd":"/opt/x"}"#;
    assert_eq!(rewrite_jsonl_cwd(unmapped, &mapper), None);
    assert_eq!(rewrite_jsonl_cwd(r#"{"type":"summary"}"#, &mapper), None);
    assert_eq!(rewrite_jsonl_cwd(r#"{"cwd":3}"#, &mapper), None);
    assert_eq!(rewrite_jsonl_cwd("not json", &mapper), None);
}

#[test]
fn claude_json_untransferred_keys_follow_the_merge_rules() {
    let source = serde_json::json!({
        "projects": {}, "mcpServers": {}, "githubRepoPaths": {},
        "oauthAccount": {}, "userID": "x", "cachedThing": 1,
    });
    assert_eq!(
        claude_json_untransferred_keys(&source, false),
        vec!["cachedThing", "oauthAccount", "userID"]
    );
    assert_eq!(
        claude_json_untransferred_keys(&source, true),
        vec!["cachedThing", "userID"]
    );
    assert!(claude_json_untransferred_keys(&serde_json::Value::Null, false).is_empty());
}
