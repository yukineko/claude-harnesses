// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repros (audit 2026-11-13 C5 + C6): the circuit breaker's handling of an
//! UNMEASURED budget (corrupt budgetguard ledger).
//!
//! - C5: `CircuitRecord::budget_over_cap` (crates/condukt/src/gatelog.rs) is a
//!   plain `bool`; crates/condukt/src/circuit.rs builds the journal record with
//!   `budget_over_cap` = `false` when the ledger could not be read, so the
//!   `<run>.circuit-log.jsonl` line says `"budget_over_cap":false` ("under
//!   cap") for a budget that was never measured. (stdout already prints
//!   `null`.) A journal consumer reads that as a measurement.
//! - C6: crates/condukt/src/circuit.rs (~line 705) matches
//!   `_ if budget_unknown_reason.is_some() => Trip(BudgetUnmeasured)` before
//!   the stall / failure-streak outcome of `decide_session_circuit`, so a run
//!   that is ALSO stalled (measured) is relabelled `budget_unmeasured` and the
//!   stall is not reported.
//!
//! Measurement point: rev 09dd0c8c, 2026-11-13.
//!
//! What these tests do NOT prove: C6 uses the stall axis only, not the
//! failure-streak axis (same match arm, not separately exercised); C5 only
//! checks the journal text does not claim `false`, not what replacement
//! (null / separate field) is right. Controls (not ignored): a stall with an
//! absent ledger names `stall`; an absent ledger journals
//! `"budget_over_cap":false` (proves the journal file is located correctly).
//!
//! Runs the real binary against a per-test `$HOME` (condukt state dir,
//! `~/.claude/projects`, `~/.budgetguard` all resolve under it) in a throwaway
//! git repo with `GIT_CONFIG_GLOBAL=/dev/null`; nothing under the real
//! `~/.condukt`, `~/.budgetguard` or `~/.overwatch` is touched.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, SystemTime};

const SESSION: &str = "sess-reds-c5c6";

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

    /// A stateless claim whose session transcript is `age_secs` old.
    fn claim(&self, run: &str, age_secs: u64) {
        let dir = self.home.join(".claude").join("projects").join("-fixture");
        std::fs::create_dir_all(&dir).unwrap();
        let t = dir.join(format!("{SESSION}.jsonl"));
        std::fs::write(&t, "{\"seed\":1}\n").unwrap();
        let f = std::fs::OpenOptions::new().write(true).open(&t).unwrap();
        f.set_modified(SystemTime::now() - Duration::from_secs(age_secs))
            .unwrap();
        let out = self.condukt(&[
            "state",
            "claim-task",
            "--run",
            run,
            "--stateless",
            "--title",
            "t",
            "--hashkey",
            "hk-1",
            "--session",
            SESSION,
        ]);
        assert!(
            out.status.success(),
            "fixture precondition: claim-task: {out:?}"
        );
    }

    fn corrupt_ledger(&self) {
        let p = self
            .home
            .join(".budgetguard")
            .join("state")
            .join("ledger.json");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, "{not json").unwrap();
    }

    fn circuit(&self, run: &str) -> (i32, serde_json::Value, String) {
        let out = self.condukt(&[
            "circuit",
            "check",
            "--run",
            run,
            "--idle-ttl-secs",
            "1800",
            "--budget-cap-usd",
            "1",
        ]);
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        let v: serde_json::Value = serde_json::from_str(&stdout).unwrap_or_else(|e| {
            panic!("circuit check stdout is not JSON ({e}); stdout={stdout} stderr={stderr}")
        });
        (out.status.code().expect("exit code"), v, stderr)
    }

    /// Every `*.circuit-log.jsonl` under HOME (the state dir resolves there),
    /// concatenated. Empty if none found.
    fn journal_text(&self) -> String {
        fn walk(dir: &Path, out: &mut String) {
            let Ok(rd) = std::fs::read_dir(dir) else {
                return;
            };
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, out);
                } else if p
                    .file_name()
                    .is_some_and(|n| n.to_string_lossy().ends_with(".circuit-log.jsonl"))
                {
                    out.push_str(&std::fs::read_to_string(&p).unwrap_or_default());
                }
            }
        }
        let mut s = String::new();
        walk(&self.home, &mut s);
        s
    }
}

/// CONTROL for C5 (not ignored): with NO ledger the journal is found and says
/// `"budget_over_cap":false` (a genuine $0 day).
#[test]
fn control_absent_ledger_journals_budget_over_cap_false() {
    let fx = Fixture::new();
    fx.claim("flow-c5-ctl", 0);
    let (code, v, stderr) = fx.circuit("flow-c5-ctl");
    assert_eq!(code, 0, "CONTROL: {v} {stderr}");
    let j = fx.journal_text();
    assert!(
        j.contains("\"budget_over_cap\":false"),
        "CONTROL: journal must be locatable and record false; journal={j:?}"
    );
}

/// CONTROL for C6 (not ignored): stalled run + absent ledger names `stall`.
#[test]
fn control_stall_with_absent_ledger_names_stall() {
    let fx = Fixture::new();
    fx.claim("flow-c6-ctl", 7200);
    let (code, v, stderr) = fx.circuit("flow-c6-ctl");
    assert_eq!(code, 1, "CONTROL: {v} {stderr}");
    assert_eq!(v["reason"], "stall", "CONTROL: {v}");
}

/// C5: an unmeasured budget must not be journaled as `budget_over_cap:false`.
#[test]
#[ignore = "RED: audit 2026-11-13 C5 — remove the ignore when fixed"]
fn journal_does_not_record_unmeasured_budget_as_not_over_cap() {
    let fx = Fixture::new();
    let run = "flow-c5";
    fx.claim(run, 0);
    fx.corrupt_ledger();
    let (code, v, stderr) = fx.circuit(run);
    eprintln!("observation: exit={code} stdout={v} stderr={stderr}");
    assert_eq!(v["reason"], "budget_unmeasured", "precondition: {v}");
    let j = fx.journal_text();
    assert!(
        !j.is_empty(),
        "precondition: a journal line must have been written"
    );
    assert!(
        !j.contains("\"budget_over_cap\":false"),
        "the circuit journal records `\"budget_over_cap\":false` for a corrupt (unmeasured) \
         budget ledger; journal={j:?}"
    );
}

/// C6: a measured stall must still be reported when the budget is unmeasured.
#[test]
#[ignore = "RED: audit 2026-11-13 C6 — remove the ignore when fixed"]
fn unmeasured_budget_does_not_relabel_a_measured_stall() {
    let fx = Fixture::new();
    let run = "flow-c6";
    fx.claim(run, 7200);
    fx.corrupt_ledger();
    let (code, v, stderr) = fx.circuit(run);
    eprintln!("observation: exit={code} stdout={v} stderr={stderr}");
    assert_eq!(code, 1, "precondition: must trip: {v}");
    let reason = v["reason"].to_string();
    assert!(
        reason.contains("stall"),
        "a 2h-stalled run with a corrupt budget ledger must still report the stall (alone \
         or together with budget_unmeasured); got reason={reason} full={v}"
    );
}
