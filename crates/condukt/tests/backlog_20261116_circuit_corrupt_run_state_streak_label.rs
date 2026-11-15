// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro (backlog eeeb2ed3): a corrupt (present, unparseable) run state
//! must trip `streak_unmeasured` while the streak axis is on, even when the
//! idle axis is also on (and so is also undetermined for the same cause).
//!
//! Documented contract: crates/condukt/src/circuit.rs (~528-531, ~596-597) and
//! crates/condukt/README.md:65 — an unreadable/corrupt run state trips
//! `streak_unmeasured` while the streak axis is on. Target precedence: measured
//! BudgetOverCap / FailureStreak / Stall keep their labels; then
//! BudgetUnmeasured; then StreakUnmeasured; then the core's IdleUnmeasured and
//! SessionCompacted.
//!
//! Pinned here:
//! - corrupt run state + `--streak-cap 3` + `--idle-ttl-secs 1800`, no budget
//!   cap → exit 1 with reason `streak_unmeasured`.
//! - CONTROL (not ignored): same corrupt run state with `--streak-cap 0`
//!   (streak axis off) → reason `idle_unmeasured` (proves the fixture really
//!   makes the idle axis undetermined, so the RED is about label precedence).
//!
//! Measurement point: rev 361523097, 2026-11-16. Expected on that rev: test 1
//! FAILS (reason `idle_unmeasured`, per code reading), control passes.
//!
//! What this does NOT prove: ordering against a measured stall / failure
//! streak / over-cap budget; ordering vs `budget_unmeasured` or
//! `session_compacted`; the `--session` / claim-registry routes; the journal
//! contents — only stdout JSON `reason` and the exit code.
//!
//! Written by an independent test author, not the implementer.
//!
//! Runs the real binary against a per-test `$HOME` in a throwaway git repo with
//! `GIT_CONFIG_GLOBAL=/dev/null`; nothing under the real `~/.condukt` is touched.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const RUN: &str = "run-corrupt-state";

struct Fixture {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    home: PathBuf,
}

fn run_git(dir: &Path, args: &[&str]) {
    let out = harness_core::git_env::scrub(&mut Command::new("git"))
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn find_named(dir: &Path, name: &str, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            find_named(&p, name, out);
        } else if p.file_name().and_then(|n| n.to_str()) == Some(name) {
            out.push(p);
        }
    }
}

impl Fixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let repo = base.join("repo");
        let home = base.join("home");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        run_git(&repo, &["init", "-q", "-b", "main"]);
        run_git(&repo, &["config", "user.email", "t@t.t"]);
        run_git(&repo, &["config", "user.name", "t"]);
        run_git(&repo, &["config", "commit.gpgsign", "false"]);
        std::fs::write(repo.join("base.txt"), "base\n").unwrap();
        run_git(&repo, &["add", "base.txt"]);
        run_git(&repo, &["commit", "-q", "-m", "init"]);
        Self {
            _tmp: tmp,
            repo,
            home,
        }
    }

    fn condukt(&self, args: &[&str]) -> Output {
        harness_core::git_env::scrub(&mut Command::new(env!("CARGO_BIN_EXE_condukt")))
            .args(args)
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env("CONDUKT_WORKTREE_BASE", self.home.join("worktrees"))
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .env_remove("CONDUKT_DISABLE")
            .output()
            .expect("spawn condukt")
    }

    /// Create a real run state via `state init`, then overwrite the file with
    /// unparseable bytes (present but corrupt, NOT absent).
    fn corrupt_run_state(&self) {
        let dec = self.repo.join("dec.json");
        std::fs::write(
            &dec,
            r#"{"goal":"g","tasks":[{"id":"t1","title":"edit","touched_files":["src/x.rs"],"deps":[],"class":"parallel","done_criteria":"d"}]}"#,
        )
        .unwrap();
        let o = self.condukt(&[
            "state",
            "init",
            "--run",
            RUN,
            "--file",
            dec.to_str().unwrap(),
        ]);
        assert!(o.status.success(), "apparatus: state init failed: {o:?}");
        let mut found = Vec::new();
        find_named(&self.home, &format!("{RUN}.json"), &mut found);
        assert_eq!(
            found.len(),
            1,
            "apparatus: expected one run state: {found:?}"
        );
        std::fs::write(&found[0], "{not json").unwrap();
    }

    fn circuit(&self, extra: &[&str]) -> (i32, serde_json::Value, String) {
        let mut args = vec!["circuit", "check", "--run", RUN, "--idle-ttl-secs", "1800"];
        args.extend_from_slice(extra);
        let out = self.condukt(&args);
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        let v: serde_json::Value = serde_json::from_str(&stdout).unwrap_or_else(|e| {
            panic!("circuit check stdout is not JSON ({e}); stdout={stdout} stderr={stderr}")
        });
        (out.status.code().expect("exit code"), v, stderr)
    }
}

/// CONTROL: streak axis off → the corrupt run state leaves idle undetermined.
#[test]
fn control_corrupt_run_state_streak_off_reports_idle_unmeasured() {
    let fx = Fixture::new();
    fx.corrupt_run_state();
    let (code, v, stderr) = fx.circuit(&["--streak-cap", "0"]);
    assert_eq!(code, 1, "CONTROL: must trip: {v} {stderr}");
    assert_eq!(v["reason"], "idle_unmeasured", "CONTROL: {v}");
}

/// Corrupt run state + streak axis on + idle axis on → `streak_unmeasured`.
#[test]
fn corrupt_run_state_with_streak_axis_on_reports_streak_unmeasured() {
    let fx = Fixture::new();
    fx.corrupt_run_state();
    let (code, v, stderr) = fx.circuit(&["--streak-cap", "3"]);
    eprintln!("observation: exit={code} stdout={v} stderr={stderr}");
    assert_eq!(code, 1, "must trip, not continue: {v}");
    assert_eq!(
        v["reason"], "streak_unmeasured",
        "a corrupt run state with the streak axis on must report streak_unmeasured \
         (documented), not the core's idle_unmeasured; full={v}"
    );
}
