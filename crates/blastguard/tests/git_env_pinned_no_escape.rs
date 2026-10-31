//! `git_env::pinned` must confine fixture git to the fixture dir (backlog
//! 84a90410). With only `current_dir`, a fixture dir that is not its own repo
//! makes `git commit` walk up into the enclosing checkout and commit ITS staged
//! index. These tests build an outer repo with a staged file and prove the
//! fixture-style init/commit through `pinned` leaves it untouched.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "support/git_env.rs"]
mod git_env;

use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

fn git_out(dir: &Path, args: &[&str]) -> (bool, String) {
    let mut cmd = Command::new("git");
    git_env::scrub(&mut cmd);
    let out = cmd.arg("-C").arg(dir).args(args).output().unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
    )
}

/// Outer repo with one real commit and a second file staged but uncommitted.
fn outer_repo_with_staged() -> TempDir {
    let td = TempDir::new().unwrap();
    let d = td.path();
    for args in [
        vec!["init", "-q"],
        vec!["config", "user.email", "t@example.com"],
        vec!["config", "user.name", "t"],
    ] {
        assert!(git_out(d, &args).0, "outer setup {args:?}");
    }
    std::fs::write(d.join("base.txt"), "base").unwrap();
    assert!(git_out(d, &["add", "base.txt"]).0);
    assert!(git_out(d, &["commit", "-q", "-m", "base"]).0);
    std::fs::write(d.join("staged.txt"), "staged").unwrap();
    assert!(git_out(d, &["add", "staged.txt"]).0);
    td
}

struct Snapshot {
    head: String,
    count: String,
    staged: String,
}

fn snap(d: &Path) -> Snapshot {
    Snapshot {
        head: git_out(d, &["rev-parse", "HEAD"]).1,
        count: git_out(d, &["rev-list", "--count", "HEAD"]).1,
        staged: git_out(d, &["diff", "--cached", "--name-only"]).1,
    }
}

/// Fixture-style helper: init + commit through `pinned`, tolerating failure
/// (a fixture whose `.git` vanished must fail, not escape).
fn fixture_init_commit(dir: &Path, with_init: bool) {
    if with_init {
        let _ = git_env::pinned(dir).args(["init", "-q"]).output().unwrap();
    }
    let _ = git_env::pinned(dir)
        .args([
            "-c",
            "user.email=t@example.com",
            "-c",
            "user.name=t",
            "commit",
            "--allow-empty",
            "-q",
            "-m",
            "init",
        ])
        .output()
        .unwrap();
}

fn assert_outer_untouched(outer: &Path, before: &Snapshot) {
    let after = snap(outer);
    assert_eq!(after.head, before.head, "outer HEAD moved");
    assert_eq!(after.count, before.count, "outer commit count changed");
    assert_eq!(
        after.staged, "staged.txt",
        "outer staged index was consumed or altered"
    );
}

#[test]
fn pinned_commit_in_untracked_subdir_does_not_reach_outer_repo() {
    let outer = outer_repo_with_staged();
    let before = snap(outer.path());
    assert_eq!(before.staged, "staged.txt", "precondition: staged file");
    let sub = outer.path().join("fixture");
    std::fs::create_dir(&sub).unwrap();
    // Fixture whose `.git` is missing: commit only, no init.
    fixture_init_commit(&sub, false);
    assert_outer_untouched(outer.path(), &before);
}

#[test]
fn pinned_init_and_commit_in_subdir_stays_inside_subdir() {
    let outer = outer_repo_with_staged();
    let before = snap(outer.path());
    let sub = outer.path().join("fixture2");
    std::fs::create_dir(&sub).unwrap();
    fixture_init_commit(&sub, true);
    assert_outer_untouched(outer.path(), &before);
    // The fixture's own repo got the commit.
    assert!(sub.join(".git").exists(), "fixture repo was not created");
    assert_eq!(git_out(&sub, &["rev-list", "--count", "HEAD"]).1, "1");
}

#[test]
fn pinned_on_missing_dir_panics() {
    let outer = outer_repo_with_staged();
    let missing = outer.path().join("does-not-exist");
    let r = std::panic::catch_unwind(|| {
        let _ = git_env::pinned(&missing);
    });
    assert!(r.is_err(), "pinned() on a missing dir must panic");
}
