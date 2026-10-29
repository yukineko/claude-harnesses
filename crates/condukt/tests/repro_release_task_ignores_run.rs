#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Regression (backlog 40dde238): `condukt state release-task --run X` used to
//! ignore --run, so run B could release a task claim held by run A.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Fx {
    base: PathBuf,
    repo: PathBuf,
    home: PathBuf,
}

impl Drop for Fx {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

fn git(dir: &Path, args: &[&str]) {
    let o = harness_core::git_env::scrub(&mut Command::new("git"))
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(o.status.success(), "git {args:?}: {o:?}");
}

impl Fx {
    fn new(tag: &str) -> Self {
        let base =
            std::env::temp_dir().join(format!("condukt-relrun-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let repo = base.join("repo");
        let home = base.join("home");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["config", "user.email", "t@t.t"]);
        git(&repo, &["config", "user.name", "t"]);
        std::fs::write(repo.join("a.txt"), "a\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-q", "-m", "init"]);
        Self { base, repo, home }
    }
    fn c(&self, args: &[&str]) -> Output {
        harness_core::git_env::scrub(&mut Command::new(env!("CARGO_BIN_EXE_condukt")))
            .args(args)
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env("CONDUKT_WORKTREE_BASE", self.base.join("wtb"))
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .output()
            .unwrap()
    }
}

#[test]
fn release_task_by_another_run_does_not_release_the_holders_claim() {
    let f = Fx::new("other-run");
    let o = f.c(&[
        "state",
        "claim-task",
        "--run",
        "run-A",
        "--hashkey",
        "hk-held",
    ]);
    assert!(o.status.success(), "apparatus: claim failed: {o:?}");
    assert_eq!(
        f.c(&["state", "is-claimed", "--hashkey", "hk-held"])
            .status
            .code(),
        Some(0)
    );

    let _ = f.c(&[
        "state",
        "release-task",
        "--run",
        "run-B",
        "--hashkey",
        "hk-held",
    ]);

    let still = f.c(&["state", "is-claimed", "--hashkey", "hk-held"]);
    assert_eq!(
        still.status.code(),
        Some(0),
        "run-B released run-A's claim via release-task --run run-B: {still:?}"
    );
}

// ---- fixture isolation: inherited GIT_* must not reach the repo under test ------

/// Scrubbed git call (independent of the fixture `git` helper under test);
/// returns stdout.
fn clean_git(dir: &Path, args: &[&str]) -> String {
    let mut c = Command::new("git");
    c.args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t.t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t.t");
    harness_core::git_env::scrub(&mut c);
    let out = c.output().expect("git runs");
    assert!(
        out.status.success(),
        "clean git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn repo_state(outer: &Path) -> String {
    format!(
        "HEAD={}\nbranches:\n{}\nworktrees:\n{}",
        clean_git(outer, &["rev-parse", "HEAD"]).trim(),
        clean_git(outer, &["branch", "--list"]),
        clean_git(outer, &["worktree", "list", "--porcelain"]),
    )
}

/// The fixture's `git` helper must not follow an inherited `GIT_DIR` (as set
/// when the suite runs under a git hook). The child re-runs one existing
/// `Fx::new` test with `GIT_DIR` aimed at a scratch outer repo; that outer
/// repo must come back unchanged.
#[test]
fn fixture_git_ignores_inherited_git_dir() {
    let outer = std::env::temp_dir().join(format!(
        "condukt-relrun-gitenv-outer-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&outer).unwrap();
    clean_git(&outer, &["init", "-q", "-b", "main"]);
    std::fs::write(outer.join("f.txt"), "o\n").unwrap();
    clean_git(&outer, &["add", "f.txt"]);
    clean_git(&outer, &["commit", "-q", "-m", "outer"]);
    let before = repo_state(&outer);

    let child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "release_task_by_another_run_does_not_release_the_holders_claim",
            "--test-threads=1",
        ])
        .env("GIT_DIR", outer.join(".git"))
        .output()
        .expect("child test binary runs");

    let after = repo_state(&outer);
    let _ = std::fs::remove_dir_all(&outer);
    assert_eq!(
        before,
        after,
        "outer repo mutated by fixture git under inherited GIT_DIR\nchild stderr: {}",
        String::from_utf8_lossy(&child.stderr)
    );
}
