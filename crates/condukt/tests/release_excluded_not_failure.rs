#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro (2026-11-03, measured at condukt 0.7.322): a flow driver that
//! claims an item and then EXCLUDES it (overlap with another session found on
//! the post-claim re-check) can only release it as `done`, `failed` or with no
//! outcome. No outcome is logged `unspecified`, which `circuit check` counts
//! as a failure, so three overlap exclusions tripped `failure_streak` (streak
//! 3/3) in run flow-c4aa3733-… before any work started.
//!
//! Contract pinned here (the spelling `excluded` is the test author's; the
//! implementer may raise a different spelling with the test author rather
//! than weaken the test):
//!   * `state release-task --outcome excluded` is accepted;
//!   * an excluded release does NOT count toward the failure streak;
//!   * it is NOT progress either: it does not reset a preceding streak.
//!
//! Written independently of any implementation (CLAUDE.md §2(a)).

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
            std::env::temp_dir().join(format!("condukt-relexcl-{}-{tag}", std::process::id()));
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

    fn release(&self, run: &str, hk: &str, outcome: &str) {
        let o = self.c(&[
            "state",
            "release-task",
            "--run",
            run,
            "--hashkey",
            hk,
            "--outcome",
            outcome,
        ]);
        assert!(
            o.status.success(),
            "release-task --outcome {outcome} must be accepted: {o:?}"
        );
    }

    fn circuit(&self, run: &str) -> (Option<i32>, serde_json::Value, Output) {
        let o = self.c(&["circuit", "check", "--run", run, "--idle-ttl-secs", "0"]);
        let v: serde_json::Value = serde_json::from_slice(&o.stdout)
            .unwrap_or_else(|e| panic!("circuit stdout is not JSON ({e}): {o:?}"));
        (o.status.code(), v, o)
    }
}

#[test]
fn three_overlap_exclusions_do_not_trip_the_failure_streak() {
    let f = Fx::new("three-excl");
    let run = "flow-excl-1";
    for hk in ["hk-1", "hk-2", "hk-3"] {
        f.release(run, hk, "excluded");
    }
    let (code, v, o) = f.circuit(run);
    assert_eq!(
        code,
        Some(0),
        "exclusions are not failures and must not trip: {o:?}"
    );
    assert_eq!(v["verdict"], "continue", "{v}");
    assert_eq!(v["streak"], 0, "{v}");
}

#[test]
fn an_exclusion_does_not_reset_a_preceding_failure_streak() {
    let f = Fx::new("excl-no-reset");
    let run = "flow-excl-2";
    f.release(run, "hk-1", "failed");
    f.release(run, "hk-2", "failed");
    f.release(run, "hk-3", "excluded");
    f.release(run, "hk-4", "failed");
    let (code, v, o) = f.circuit(run);
    assert_eq!(
        code,
        Some(1),
        "an exclusion is not progress; three failures around it must trip: {o:?}"
    );
    assert_eq!(v["reason"], "failure_streak", "{v}");
    assert_eq!(v["streak"], 3, "{v}");
}
