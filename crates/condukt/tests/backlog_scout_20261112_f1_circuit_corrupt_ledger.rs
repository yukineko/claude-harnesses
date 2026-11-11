// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro (scout 2026-11-12 F1): `condukt circuit check --budget-cap-usd`
//! reads a CORRUPT budgetguard ledger as a confident $0 day total.
//!
//! The budget axis loads `~/.budgetguard/state/ledger.json` through
//! `harness_core::ledger::Ledger::load` (crates/harness-core/src/ledger.rs:85),
//! which folds `LoadError::Corrupt` / `LoadError::Unreadable` into
//! `Ledger::default()`; `crates/condukt/src/circuit.rs:632-635` then compares
//! that `$0` against the cap and reports `budget_over_cap: false`. A ledger
//! that cannot be read is cannot-determine (CLAUDE.md §3) and must not be a
//! confident "under cap": the breaker must trip.
//!
//! RED observed at rev 656b1c38 (written without a fix, by a disinterested
//! author — CLAUDE.md §2(a)). The control (no ledger at all -> continue) runs
//! first inside the RED test, on the same fixture, so the RED assertion can
//! only fail on the corrupt-ledger property and not on fixture plumbing.
//!
//! Everything runs the real binary against a per-test `$HOME` (where condukt's
//! state dir, `~/.claude/projects` and `~/.budgetguard` all resolve) inside a
//! throwaway git repo with `GIT_CONFIG_GLOBAL=/dev/null`; nothing under the
//! real `~/.condukt`, `~/.budgetguard` or `~/.overwatch` is touched.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const SESSION: &str = "sess-scout-f1-ledger";

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

    /// The healthy run of `circuit_stateless_idle_e2e.rs` case (a): a stateless
    /// claim whose owning session's transcript was just written.
    fn healthy_run(&self, run: &str) {
        let dir = self.home.join(".claude").join("projects").join("-fixture");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("{SESSION}.jsonl")), "{\"seed\":1}\n").unwrap();
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
            "fixture precondition: stateless claim-task must succeed: {out:?}"
        );
    }

    fn ledger_path(&self) -> PathBuf {
        self.home
            .join(".budgetguard")
            .join("state")
            .join("ledger.json")
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
}

#[test]
#[ignore = "scout 2026-11-12 F1: RED, open defect. Remove the ignore when fixed"]
fn corrupt_budget_ledger_trips_instead_of_reading_as_zero() {
    let fx = Fixture::new();
    let run = "flow-scout-f1";
    fx.healthy_run(run);

    // CONTROL: no ledger.json at all -> the budget axis has nothing over cap
    // and the healthy run continues (exit 0).
    assert!(
        !fx.ledger_path().exists(),
        "fixture precondition: no ledger yet"
    );
    let (code, v, stderr) = fx.circuit(run);
    assert_eq!(
        code, 0,
        "CONTROL: healthy run with no ledger must continue; got {v} stderr={stderr}"
    );
    assert_eq!(v["verdict"], "continue", "CONTROL: {v}");

    // RED: the ledger exists but is not JSON.
    std::fs::create_dir_all(fx.ledger_path().parent().unwrap()).unwrap();
    std::fs::write(fx.ledger_path(), "{not json").unwrap();
    let (code, v, stderr) = fx.circuit(run);
    eprintln!("observation (corrupt ledger): exit={code} stdout={v} stderr={stderr}");
    let confident_under_cap = v["budget_over_cap"] == serde_json::json!(false)
        && v["budget_unknown_reason"].is_null()
        && !v["reason"].as_str().unwrap_or("").contains("budget");
    assert!(
        code == 1 && v["verdict"] == "trip" && !confident_under_cap,
        "a corrupt budget ledger (`{{not json`) is cannot-determine and must trip the \
         breaker, not read as a confident $0 under the $1 cap; got exit={code} stdout={v} \
         stderr={stderr}"
    );
}
