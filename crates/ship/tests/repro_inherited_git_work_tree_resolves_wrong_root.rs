#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Pins the scrub on `resolve_repo_root` (`src/main.rs`, the
//! `git rev-parse --show-toplevel` spawn) independently of the scrub in
//! `src/git.rs`.
//!
//! With `GIT_DIR=<B>/.git` AND `GIT_WORK_TREE=<B>` inherited, an unscrubbed
//! `rev-parse --show-toplevel` run inside clean repo A answers B's toplevel.
//! Every later detection (even the scrubbed ones in `git.rs`) then runs in B,
//! so `ship check` in A reports B's untracked file as "uncommitted changes
//! present". The verdict for A must stay A's: "[x] no uncommitted changes".
//! `GIT_DIR` alone is not enough to observe this: git then takes the cwd as
//! the work tree, so `--show-toplevel` would still answer A.
//! The env is set on the spawned child only (no `std::env::set_var`).

use std::path::Path;
use std::process::Command;

fn git(dir: &Path, args: &[&str]) {
    let o = harness_core::git_env::scrub(&mut Command::new("git"))
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
}

fn ship_check(dir: &Path, other: Option<&Path>) -> String {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_ship"));
    harness_core::git_env::scrub(&mut cmd);
    cmd.arg("check")
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1");
    if let Some(b) = other {
        cmd.env("GIT_DIR", b.join(".git"));
        cmd.env("GIT_WORK_TREE", b);
    }
    let out = cmd.output().unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn committed_repo() -> tempfile::TempDir {
    let d = tempfile::TempDir::new().unwrap();
    git(d.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(d.path().join("tracked"), "t").unwrap();
    git(d.path(), &["add", "tracked"]);
    git(d.path(), &["commit", "-q", "-m", "c1"]);
    d
}

#[test]
fn inherited_git_work_tree_does_not_redirect_repo_root_resolution() {
    // A: clean repo (the target). B: repo with an untracked file.
    let a = committed_repo();
    let b = committed_repo();
    std::fs::write(b.path().join("dirty"), "d").unwrap();

    let control = ship_check(a.path(), None);
    assert!(
        control.contains("[x] no uncommitted changes"),
        "control: clean repo A must report no uncommitted changes:\n{control}"
    );

    let with_env = ship_check(a.path(), Some(b.path()));
    assert!(
        with_env.contains("[x] no uncommitted changes")
            && !with_env.contains("uncommitted changes present"),
        "ship check in clean repo A judged unrelated repo B (inherited \
         GIT_DIR/GIT_WORK_TREE redirected the repo root).\n\
         --- control ---\n{control}\n--- with GIT_DIR/GIT_WORK_TREE=B ---\n{with_env}"
    );
    assert_eq!(with_env, control);
}
