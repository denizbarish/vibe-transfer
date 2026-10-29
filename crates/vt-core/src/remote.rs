//! Git remote helpers that never spawn the `git` binary.

use std::fs;
use std::path::{Path, PathBuf};

/// Normalizes a git remote URL to `host/owner/repo`.
///
/// Accepts scp-like (`git@host:owner/repo.git`) and URL forms
/// (`https://`, `ssh://`, `git://`, with optional user info and port).
/// The host is lowercased; path case is kept. A trailing `/` and `.git`
/// are removed. Local paths, `file://` URLs and empty input give `None`.
pub fn normalize_remote(url: &str) -> Option<String> {
    let url = url.trim();
    if url.is_empty() {
        return None;
    }

    let (host, path) = if let Some((scheme, rest)) = url.split_once("://") {
        if scheme.eq_ignore_ascii_case("file") {
            return None;
        }
        let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
        let host_port = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
        let host = host_port.split_once(':').map_or(host_port, |(h, _)| h);
        (host, path)
    } else {
        // scp-like syntax: the colon must come before any slash.
        let colon = url.find(':')?;
        // A slash before the colon, or a Windows drive (`C:\repo`), is a local path.
        if url[..colon].contains('/') || colon == 1 || url.contains('\\') {
            return None;
        }
        let authority = &url[..colon];
        let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
        (host, &url[colon + 1..])
    };

    let mut path = path.trim_matches('/');
    path = path.strip_suffix(".git").unwrap_or(path);
    let path = path.trim_end_matches('/');
    if host.is_empty() || path.is_empty() {
        return None;
    }
    Some(format!("{}/{}", host.to_ascii_lowercase(), path))
}

/// Reads `[remote "origin"] url` from the repository at `repo_root`.
///
/// Follows a `.git` file (`gitdir: ...`) for worktrees and submodules, and
/// a `commondir` file to reach the shared config of a linked worktree.
pub fn read_origin(repo_root: &Path) -> Option<String> {
    let git_dir = resolve_git_dir(repo_root)?;
    let config_dir = match fs::read_to_string(git_dir.join("commondir")) {
        Ok(common) => git_dir.join(common.trim()),
        Err(_) => git_dir,
    };
    let config = fs::read_to_string(config_dir.join("config")).ok()?;
    origin_url(&config)
}

fn resolve_git_dir(repo_root: &Path) -> Option<PathBuf> {
    let dot_git = repo_root.join(".git");
    let meta = fs::metadata(&dot_git).ok()?;
    if meta.is_dir() {
        return Some(dot_git);
    }
    let contents = fs::read_to_string(&dot_git).ok()?;
    let target = contents
        .lines()
        .find_map(|line| line.trim().strip_prefix("gitdir:"))?
        .trim();
    if target.is_empty() {
        return None;
    }
    // `join` keeps an absolute target as is.
    Some(repo_root.join(target))
}

fn origin_url(config: &str) -> Option<String> {
    let mut in_origin = false;
    for raw in config.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(header) = line.strip_prefix('[') {
            let header = header.split(']').next().unwrap_or("").trim();
            in_origin = match header.split_once(char::is_whitespace) {
                Some((section, sub)) => {
                    section.eq_ignore_ascii_case("remote") && sub.trim() == "\"origin\""
                }
                None => false,
            };
            continue;
        }
        if !in_origin {
            continue;
        }
        if let Some((key, value)) = line.split_once('=')
            && key.trim().eq_ignore_ascii_case("url")
        {
            let value = value.trim();
            let value = value
                .strip_prefix('"')
                .and_then(|v| v.strip_suffix('"'))
                .unwrap_or(value);
            if !value.is_empty() {
                return Some(value.to_owned());
            }
        }
    }
    None
}
