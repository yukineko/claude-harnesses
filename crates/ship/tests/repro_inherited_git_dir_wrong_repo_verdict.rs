#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro: production `git` spawns in `src/git.rs` (and `src/main.rs`)
//! honour an inherited `GIT_DIR`, so `ship check` run inside directory A
//! judges the checklist from unrelated repo B.
//!
//! A is a plain directory (not a repository). Scrubbed, `ship check` cannot
//! determine anything about it (`[?] uncommitted-changes check could not
//! run`). With `GIT_DIR=<B>/.git` exported (as a git hook or a shell with
//! GIT_DIR set would), the same call answers about B: B has an untracked
//! file, so it reports "uncommitted changes present". The verdict is about
//! the wrong repo. The env is set on the spawned child only (no
//! `std::env::set_var`), so the parallel test runner is not raced.

use std::path::Path;
use std::process::Command;

fn git(dir: &Path, args: &[&str]) {
    let o = harness_core::git_env::scrub(&mut Command::new("git"))
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
}

fn ship_check(dir: &Path, git_dir: Option<&Path>) -> String {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_ship"));
    harness_core::git_env::scrub(&mut cmd);
    cmd.arg("check").current_dir(dir);
    if let Some(g) = git_dir {
        cmd.env("GIT_DIR", g);
    }
    let out = cmd.output().unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn inherited_git_dir_does_not_change_the_verdict_for_a_plain_directory() {
    // Repo B: one commit plus an untracked file => "uncommitted changes".
    let b = tempfile::TempDir::new().unwrap();
    git(b.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(b.path().join("tracked"), "t").unwrap();
    git(b.path(), &["add", "tracked"]);
    git(b.path(), &["commit", "-q", "-m", "c1"]);
    std::fs::write(b.path().join("dirty"), "d").unwrap();

    // A: plain directory, not a repository.
    let a = tempfile::TempDir::new().unwrap();

    let control = ship_check(a.path(), None);
    let with_env = ship_check(a.path(), Some(&b.path().join(".git")));

    assert_eq!(
        with_env, control,
        "ship check in a non-repo directory gave a different verdict when GIT_DIR \
         pointed at unrelated repo B (judged B, not A).\n--- control (scrubbed) ---\n{control}\n\
         --- with GIT_DIR=B/.git ---\n{with_env}"
    );
}
