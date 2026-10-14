// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro for backlog 41c837ab: `condukt state reconcile` promotes a task
//! to `verified` when the `branch_sha` recorded by `state set --branch` is the
//! fork point (main tip at worktree creation), because "the recorded sha is an
//! ancestor of main" is read as "the task's work is merged".
//!
//! Driven entirely through the real binary and real git: the branch and its
//! worktree are created at main's tip, the branch is recorded with
//! `state set --branch` (which pins `branch_sha` = main tip), the worker then
//! commits on the branch, the task is set `failed`, and NOTHING is merged.
//! Reconcile must not report that failed, unmerged task as verified.
//!
//! Control: a task whose branch commit really is merged into main is still
//! promoted (so the RED test cannot be satisfied by never promoting anything).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_condukt")
}

struct Fixture {
    base: PathBuf,
    repo: PathBuf,
    home: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn find_file(dir: &Path, name: &str) -> Option<PathBuf> {
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let p = entry.path();
        if p.is_dir() {
            if let Some(hit) = find_file(&p, name) {
                return Some(hit);
            }
        } else if p.file_name().and_then(|n| n.to_str()) == Some(name) {
            return Some(p);
        }
    }
    None
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let mut base = std::env::temp_dir();
        base.push(format!("condukt-41c837ab-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let repo = base.join("repo");
        let home = base.join("home");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["config", "user.email", "t@t.t"]);
        git(&repo, &["config", "user.name", "t"]);
        git(&repo, &["config", "commit.gpgsign", "false"]);
        std::fs::write(repo.join("base.txt"), "base\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-q", "-m", "init"]);
        Self { base, repo, home }
    }

    fn condukt(&self, args: &[&str]) -> Output {
        Command::new(bin())
            .args(args)
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env("CLAUDE_CODE_SESSION_ID", "sess-41c837ab")
            .env_remove("CONDUKT_DISABLE")
            .output()
            .expect("spawn condukt")
    }

    fn ok(&self, args: &[&str]) {
        let out = self.condukt(args);
        assert!(out.status.success(), "condukt {args:?} failed: {out:?}");
    }

    fn init_run(&self, run: &str) {
        let dec = self.base.join(format!("{run}-dec.json"));
        std::fs::write(
            &dec,
            r#"{"goal":"g","tasks":[{"id":"t1","title":"edit work.txt","touched_files":["work.txt"],"deps":[],"class":"parallel","done_criteria":"d"}]}"#,
        )
        .unwrap();
        self.ok(&[
            "state",
            "init",
            "--run",
            run,
            "--file",
            dec.to_str().unwrap(),
        ]);
    }

    fn status(&self, run: &str) -> String {
        let p = find_file(&self.home.join(".condukt"), &format!("{run}.json"))
            .unwrap_or_else(|| panic!("run state {run}.json not found"));
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap();
        v["tasks"][0]["status"].as_str().unwrap().to_string()
    }

    /// Create `branch` + its worktree at main's tip and record both on t1 the
    /// way the orchestrator does at dispatch (`state set --status running
    /// --worktree W --branch B`). Returns the worktree path.
    fn dispatch(&self, run: &str, branch: &str) -> PathBuf {
        let wt = self.base.join(format!("wt-{run}"));
        git(
            &self.repo,
            &["worktree", "add", "-q", "-b", branch, wt.to_str().unwrap()],
        );
        self.ok(&[
            "state",
            "set",
            "--run",
            run,
            "--task",
            "t1",
            "--status",
            "running",
            "--worktree",
            wt.to_str().unwrap(),
            "--branch",
            branch,
        ]);
        wt
    }
}

#[test]
#[ignore = "backlog 41c837ab: RED repro"]
fn failed_unmerged_task_is_not_promoted_to_verified_by_reconcile() {
    let fx = Fixture::new("red");
    fx.init_run("r1");
    let wt = fx.dispatch("r1", "condukt/r1/t1");

    // The worker commits real work on the branch; it is never merged.
    std::fs::write(wt.join("work.txt"), "unmerged work\n").unwrap();
    git(&wt, &["add", "work.txt"]);
    git(&wt, &["commit", "-q", "-m", "task work"]);
    let branch_tip = git(&fx.repo, &["rev-parse", "condukt/r1/t1"]);
    let merged = Command::new("git")
        .args(["merge-base", "--is-ancestor", &branch_tip, "main"])
        .current_dir(&fx.repo)
        .status()
        .unwrap()
        .success();
    assert!(!merged, "fixture: the branch's work must NOT be in main");

    fx.ok(&[
        "state", "set", "--run", "r1", "--task", "t1", "--status", "failed",
    ]);
    assert_eq!(fx.status("r1"), "failed");

    let out = fx.condukt(&["state", "reconcile", "--run", "r1"]);
    assert_eq!(
        fx.status("r1"),
        "failed",
        "reconcile promoted a FAILED task whose branch commit is not in main: \
         stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn control_really_merged_task_is_still_promoted() {
    let fx = Fixture::new("control");
    fx.init_run("r2");
    let wt = fx.dispatch("r2", "condukt/r2/t1");
    std::fs::write(wt.join("work.txt"), "merged work\n").unwrap();
    git(&wt, &["add", "work.txt"]);
    git(&wt, &["commit", "-q", "-m", "task work"]);
    fx.ok(&[
        "state", "set", "--run", "r2", "--task", "t1", "--status", "done",
    ]);
    git(
        &fx.repo,
        &["merge", "-q", "--no-ff", "condukt/r2/t1", "-m", "merge"],
    );
    fx.ok(&["state", "reconcile", "--run", "r2"]);
    assert_eq!(fx.status("r2"), "verified");
}
