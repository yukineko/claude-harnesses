// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (user ruling 2026-11-15: fix as a bug): `condukt state set --task X
//! --status failed` on a task that is currently `verified` overreaches.
//!
//! OBSERVED (run run-20261114-161340-68728, journal entry
//! `{"kind":"auto_rollback","label":"verified:t1"}`):
//!   (1) the WHOLE run state was restored from the checkpoint — other tasks'
//!       statuses were rolled back too (t2 went back to running with its
//!       worktree already deleted; t3/t4 lost their worktree entries);
//!   (2) `git reset --hard` was run on the failed task's worktree branch back to
//!       the checkpoint commit, discarding a later commit (a merge-of-main).
//!
//! TWO READINGS of "verified -> failed auto-rollback":
//!   R1 (current): the failure restores the whole run + resets branches to the
//!       last checkpoint (`restore_checkpoint` in main.rs).
//!   R2 (expected): marking a verified task failed changes ONLY that task's
//!       status; other tasks' state is untouched and no commit on any worktree
//!       branch is discarded.
//!
//! HOW TO TELL THEM APART: (a) drive t1 to verified at checkpoint time, then
//! change t2's status afterwards; after t1 -> failed, t2 must still have the
//! status it had (R2) rather than the checkpoint's (R1). (b) make a commit on
//! t1's worktree branch after the checkpoint; after t1 -> failed that branch's
//! HEAD must be unchanged (R2), not reset to the checkpoint SHA (R1).
//!
//! DOES NOT PROVE: that t1 itself ends `failed` (not asserted); that the
//! journal records anything in particular; that explicit `state rollback` (a
//! deliberate user action) stays whole-run; nor that the real-run's t3/t4
//! worktree-entry loss is fixed beyond the status/branch-HEAD properties
//! asserted here (worktree entries are not asserted).
//!
//! CONFLICT: this contradicts
//! `checkpoint_cli.rs::verified_task_that_fails_auto_rolls_back_and_journals`,
//! which pins R1 (task back to pending after failing). That test is to be
//! revisited by the fix; it is intentionally not modified here.

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_condukt")
}

struct Fixture {
    repo: PathBuf,
    home: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let mut base = std::env::temp_dir();
        base.push(format!("condukt-failed-scope-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let repo = base.join("repo");
        let home = base.join("home");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(home.join(".condukt").join("state")).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["config", "user.email", "t@t.t"]);
        git(&repo, &["config", "user.name", "t"]);
        std::fs::write(repo.join("README"), "x").unwrap();
        git(&repo, &["add", "README"]);
        git(&repo, &["commit", "-q", "-m", "init"]);
        Self { repo, home }
    }

    fn condukt(&self, args: &[&str]) -> Output {
        common::prime_if_running(args, &self.home, common::TESTSCOPE_SESSION);
        harness_core::git_env::scrub(&mut Command::new(bin()))
            .args(args)
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env(
                "CONDUKT_TESTSCOPE_LEDGER",
                common::testscope_ledger(&self.home),
            )
            .env("CLAUDE_CODE_SESSION_ID", common::TESTSCOPE_SESSION)
            .output()
            .expect("spawn condukt")
    }

    fn ok(&self, args: &[&str]) -> Output {
        let o = self.condukt(args);
        assert!(o.status.success(), "condukt {args:?} failed: {o:?}");
        o
    }

    fn init(&self) -> String {
        let p = self.repo.join("decomp.json");
        std::fs::write(
            &p,
            r#"{"goal":"g","tasks":[
{"id":"t1","title":"x","touched_files":["a.rs"],"deps":[],"class":"serial","done_criteria":"d"},
{"id":"t2","title":"y","touched_files":["b.rs"],"deps":[],"class":"serial","done_criteria":"d"}]}"#,
        )
        .unwrap();
        let out = self.ok(&["state", "init", "--file", p.to_str().unwrap()]);
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .chain(String::from_utf8_lossy(&out.stderr).lines())
            .rev()
            .map(str::trim)
            .find(|l| l.starts_with("run-"))
            .expect("a run- id in init output")
            .to_string()
    }

    fn set(&self, rid: &str, task: &str, status: &str, extra: &[&str]) {
        let mut a = vec![
            "state", "set", "--run", rid, "--task", task, "--status", status,
        ];
        a.extend_from_slice(extra);
        self.ok(&a);
    }

    /// The `status` of `task` from `state show` (JSON), as a lowercase string.
    fn status_of(&self, rid: &str, task: &str) -> String {
        let out = self.ok(&["state", "show", "--run", rid]);
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        let start = text.find('{').expect("json in state show");
        let v: serde_json::Value = serde_json::from_str(&text[start..]).expect("state show json");
        let t = v["tasks"]
            .as_array()
            .expect("tasks array")
            .iter()
            .find(|t| t["id"] == task)
            .unwrap_or_else(|| panic!("task {task} in {text}"));
        t["status"].as_str().expect("status str").to_lowercase()
    }
}

fn git(dir: &Path, args: &[&str]) -> String {
    let o = harness_core::git_env::scrub(&mut Command::new("git"))
        .args(args)
        .current_dir(dir)
        .output()
        .expect("spawn git");
    assert!(o.status.success(), "git {args:?} failed: {o:?}");
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

#[test]
fn a_failing_verified_task_leaves_other_tasks_status_untouched() {
    let fx = Fixture::new("a");
    let rid = fx.init();

    // Checkpoint with everything pending, then t1 -> verified, t2 -> done.
    fx.ok(&["state", "checkpoint", "--run", &rid]);
    fx.set(&rid, "t1", "verified", &[]);
    fx.set(&rid, "t2", "done", &[]);
    assert_eq!(fx.status_of(&rid, "t2"), "done", "precondition");

    fx.set(&rid, "t1", "failed", &[]);

    assert_eq!(
        fx.status_of(&rid, "t2"),
        "done",
        "marking t1 failed rolled back t2's status (whole-run restore)"
    );
}

#[test]
fn b_failing_verified_task_does_not_move_its_worktree_branch_head() {
    let fx = Fixture::new("b");
    let rid = fx.init();

    let wt = fx.repo.parent().unwrap().join("wt-t1");
    let wt_s = wt.to_str().unwrap().to_string();
    git(&fx.repo, &["worktree", "add", "-q", "-b", "task/t1", &wt_s]);
    git(&wt, &["config", "user.email", "t@t.t"]);
    git(&wt, &["config", "user.name", "t"]);
    fx.set(
        &rid,
        "t1",
        "running",
        &["--worktree", &wt_s, "--branch", "task/t1"],
    );

    // Checkpoint records the branch SHA; then a later commit lands on the branch.
    fx.ok(&["state", "checkpoint", "--run", &rid]);
    std::fs::write(wt.join("later.txt"), "later").unwrap();
    git(&wt, &["add", "later.txt"]);
    git(&wt, &["commit", "-q", "-m", "later commit after checkpoint"]);
    let head_before = git(&wt, &["rev-parse", "HEAD"]);

    fx.set(&rid, "t1", "verified", &[]);
    fx.set(&rid, "t1", "failed", &[]);

    let head_after = git(&wt, &["rev-parse", "HEAD"]);
    assert_eq!(
        head_after, head_before,
        "marking t1 failed reset its branch, discarding the later commit"
    );
}
