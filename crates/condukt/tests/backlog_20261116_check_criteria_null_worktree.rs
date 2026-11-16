// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro (observed 2026-11-16, run run-20261116-143410-12142, ~main
//! fa58e3698): `condukt state check-criteria` on a task whose recorded
//! `worktree` is null ran the mechanical command in the CALLER'S cwd (a tree
//! that is not the task's) and reported on it as if it were the task's.
//!
//! Code path: crates/condukt/src/main.rs `StateAction::CheckCriteria`:
//! `.and_then(|s| s.worktree.as_deref()).map(PathBuf::from)
//!  .unwrap_or_else(|| cwd.to_path_buf())`. A null worktree is "cannot
//! determine which tree", and CLAUDE.md section 3 requires that to resolve to
//! block/ask, not to a silent run elsewhere. The same pattern exists in
//! `CheckOracle`.
//!
//! Pinned (RED): with the task's worktree null, check-criteria must NOT
//! execute the command in cwd (marker file absent in cwd) AND must say so:
//! non-zero exit, or JSON that does not claim a verdict (`passed` not a
//! bool). Either reading (a) refuse with error exit or (b) refuse with an
//! explicit undetermined JSON satisfies it.
//!
//! Control (not ignored): the same task WITH a recorded worktree runs the
//! command in that worktree (marker appears there, not in cwd), proving the
//! fixture's command really is observable via the marker.
//!
//! Does NOT prove: the CheckOracle null-worktree path (separate finding
//! text: "the F->P oracle could not be determined"); what the refusal message
//! says; behaviour for reproduction_tests that are run by other subcommands.
//!
//! Written by an independent test author, not the implementer.

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_condukt")
}

struct Fx {
    base: PathBuf,
    home: PathBuf,
    cwd: PathBuf,
    wt: PathBuf,
}

impl Drop for Fx {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

fn git(dir: &Path, args: &[&str]) {
    let out = harness_core::git_env::scrub(&mut Command::new("git"))
        .args(args)
        .current_dir(dir)
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

impl Fx {
    fn new(tag: &str) -> Fx {
        let base =
            std::env::temp_dir().join(format!("condukt-nullwt-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let cwd = base.join("cwd");
        let home = base.join("home");
        let wt = base.join("wt");
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        git(&cwd, &["init", "-q", "-b", "main"]);
        git(&cwd, &["config", "user.email", "t@t.t"]);
        git(&cwd, &["config", "user.name", "t"]);
        git(&cwd, &["config", "commit.gpgsign", "false"]);
        std::fs::write(cwd.join("base.txt"), "base\n").unwrap();
        git(&cwd, &["add", "."]);
        git(&cwd, &["commit", "-q", "-m", "init"]);
        git(
            &cwd,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "condukt/nw",
                wt.to_str().unwrap(),
            ],
        );
        Fx {
            base,
            home,
            cwd,
            wt,
        }
    }

    fn condukt(&self, args: &[&str]) -> Output {
        common::prime_if_running(args, &self.home, "sess-nullwt");
        harness_core::git_env::scrub(&mut Command::new(bin()))
            .args(args)
            .current_dir(&self.cwd)
            .env("HOME", &self.home)
            .env(
                "CONDUKT_TESTSCOPE_LEDGER",
                common::testscope_ledger(&self.home),
            )
            .env("CLAUDE_CODE_SESSION_ID", "sess-nullwt")
            .env("CONDUKT_AUTONOMOUS", "0")
            .env_remove("CONDUKT_DISABLE")
            .stdin(std::process::Stdio::null())
            .output()
            .expect("spawn condukt")
    }

    fn ok(&self, args: &[&str]) {
        let out = self.condukt(args);
        assert!(out.status.success(), "condukt {args:?} failed: {out:?}");
    }

    /// Run with t1 whose mechanical check touches `ran.marker` in ITS cwd.
    fn run_with(&self, with_worktree: bool) -> String {
        let run = if with_worktree { "rw" } else { "rn" };
        let dec = self.base.join(format!("{run}-dec.json"));
        std::fs::write(
            &dec,
            serde_json::json!({
                "goal": "g",
                "tasks": [{
                    "id": "t1", "title": "t", "touched_files": ["work.txt"],
                    "deps": [], "class": "parallel",
                    "done_criteria": "the check exits 0",
                    "is_behavioral": false,
                    "reproduction_tests": "sh -c 'touch ran.marker'",
                    "mechanical_check": { "cmd": "sh -c 'touch ran.marker'" }
                }]
            })
            .to_string(),
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
        let mut set = vec![
            "state", "set", "--run", run, "--task", "t1", "--status", "running",
        ];
        if with_worktree {
            set.push("--worktree");
            set.push(self.wt.to_str().unwrap());
        }
        self.ok(&set);
        run.to_string()
    }

    fn check(&self, run: &str) -> Output {
        self.condukt(&["state", "check-criteria", "--run", run, "--task", "t1"])
    }
}

#[test]
fn control_recorded_worktree_runs_command_there_not_in_cwd() {
    let fx = Fx::new("ctl");
    let run = fx.run_with(true);
    let _ = fx.check(&run);
    assert!(
        fx.wt.join("ran.marker").exists(),
        "control: command must run in the recorded worktree"
    );
    assert!(
        !fx.cwd.join("ran.marker").exists(),
        "control: command must not run in cwd when a worktree is recorded"
    );
}

#[test]
#[ignore = "RED: check-criteria with a null task worktree runs the command in the caller's cwd and reports a verdict (main.rs CheckCriteria unwrap_or_else(cwd)); expected refusal/undetermined"]
fn null_worktree_check_criteria_does_not_run_in_cwd_and_is_not_a_verdict() {
    let fx = Fx::new("null");
    let run = fx.run_with(false);
    let out = fx.check(&run);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let ran_in_cwd = fx.cwd.join("ran.marker").exists();
    let claims_verdict = stdout
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .any(|v| v.get("passed").and_then(|p| p.as_bool()).is_some());
    assert!(
        !ran_in_cwd,
        "check-criteria executed the task's command in the caller's cwd although the task has no recorded worktree; stdout={stdout} status={:?}",
        out.status.code()
    );
    assert!(
        out.status.code() != Some(0) || !claims_verdict,
        "null worktree: must exit non-zero or report undetermined (no `passed` verdict); stdout={stdout}"
    );
}
