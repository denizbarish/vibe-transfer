use std::fs;

use vt_core::remote::{normalize_remote, read_origin};

#[test]
fn remote_forms_equal() {
    let a = normalize_remote("git@github.com:Acme/Repo.git");
    assert_eq!(a.as_deref(), Some("github.com/Acme/Repo"));
    assert_eq!(normalize_remote("https://github.com/Acme/Repo"), a);
    assert_eq!(normalize_remote("ssh://git@GitHub.com/Acme/Repo.git/"), a);
    assert_eq!(normalize_remote(""), None);
}

#[test]
fn remote_more_forms() {
    assert_eq!(
        normalize_remote("https://user:pw@Git.Example.com:8443/acme/tools/app.git").as_deref(),
        Some("git.example.com/acme/tools/app")
    );
    assert_eq!(
        normalize_remote("ssh://git@github.com:22/Acme/Repo.git").as_deref(),
        Some("github.com/Acme/Repo")
    );
    assert_eq!(normalize_remote("   "), None);
    assert_eq!(normalize_remote("/srv/git/repo.git"), None);
    assert_eq!(normalize_remote("https://github.com/"), None);
    assert_eq!(normalize_remote("C:\\src\\repo"), None);
    assert_eq!(normalize_remote("file:///srv/git/repo.git"), None);
}

const CONFIG: &str = "[core]\n\tbare = false\n[remote \"upstream\"]\n\turl = https://github.com/other/Repo.git\n[remote \"origin\"]\n\turl = git@github.com:Acme/Repo.git\n\tfetch = +refs/heads/*:refs/remotes/origin/*\n";

#[test]
fn read_origin_from_config_and_worktree_file() {
    let tmp = tempfile::tempdir().unwrap();

    // Regular repository: .git is a directory.
    let repo = tmp.path().join("repo");
    fs::create_dir_all(repo.join(".git")).unwrap();
    fs::write(repo.join(".git/config"), CONFIG).unwrap();
    assert_eq!(
        read_origin(&repo).as_deref(),
        Some("git@github.com:Acme/Repo.git")
    );

    // Linked worktree: .git is a file pointing at <common>/worktrees/<name>.
    let wt_gitdir = repo.join(".git/worktrees/wt");
    fs::create_dir_all(&wt_gitdir).unwrap();
    fs::write(wt_gitdir.join("commondir"), "../..\n").unwrap();
    let wt = tmp.path().join("wt");
    fs::create_dir_all(&wt).unwrap();
    fs::write(
        wt.join(".git"),
        format!("gitdir: {}\n", wt_gitdir.display()),
    )
    .unwrap();
    assert_eq!(
        read_origin(&wt).as_deref(),
        Some("git@github.com:Acme/Repo.git")
    );

    // Submodule style: relative gitdir with its own config.
    let sub_gitdir = tmp.path().join("modules/sub");
    fs::create_dir_all(&sub_gitdir).unwrap();
    fs::write(
        sub_gitdir.join("config"),
        "[remote \"origin\"]\n    url = https://github.com/Acme/Sub\n",
    )
    .unwrap();
    let sub = tmp.path().join("sub");
    fs::create_dir_all(&sub).unwrap();
    fs::write(sub.join(".git"), "gitdir: ../modules/sub\n").unwrap();
    assert_eq!(
        read_origin(&sub).as_deref(),
        Some("https://github.com/Acme/Sub")
    );

    // No origin, or no repository at all.
    let bare = tmp.path().join("noorigin");
    fs::create_dir_all(bare.join(".git")).unwrap();
    fs::write(bare.join(".git/config"), "[core]\n\tbare = false\n").unwrap();
    assert_eq!(read_origin(&bare), None);
    assert_eq!(read_origin(&tmp.path().join("missing")), None);
}
