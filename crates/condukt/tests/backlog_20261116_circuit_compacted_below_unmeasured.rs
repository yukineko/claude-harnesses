// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro (backlog eeeb2ed3, follow-up to 85139858b): `session_compacted`
//! must stay the LOWEST-precedence circuit reason.
//!
//! Documented contract (crates/condukt/src/circuit.rs):
//! - `CircuitReason::SessionCompacted`: "Lowest precedence: every other reason
//!   (cost, failures, stall, unmeasurable idle) is reported under its own label
//!   first."
//! - `run_circuit_check`: compactions are "checked after every other reason".
//!
//! 85139858b made `run_circuit_check` pass EVERY core `Trip` of
//! `decide_session_circuit` through unchanged (so a measured stall keeps its
//! label over an unmeasured budget — intended, pinned by
//! backlog_20261113_c5_c6_circuit_budget_unmeasured.rs). Because the core
//! returns `Trip(SessionCompacted)` when the compaction count reaches
//! `--max-compactions`, that trip now outranks the post-processing that maps
//! an unmeasured budget to `budget_unmeasured` and an unmeasured streak to
//! `streak_unmeasured`. Resolution (A): `session_compacted` stays lowest.
//!
//! Pinned here:
//! - compacted (2 `compact_boundary` lines, default max 2) + corrupt budget
//!   ledger with `--budget-cap-usd 1` → trips with reason `budget_unmeasured`.
//! - compacted + corrupt item-outcome log (stateless run, streak axis on by
//!   default `--streak-cap 3`) → trips with reason `streak_unmeasured`.
//! - Controls (not ignored): compacted alone → `session_compacted` (proves the
//!   transcript is located and the compaction axis is live in this fixture);
//!   corrupt ledger alone → `budget_unmeasured`; corrupt release log alone →
//!   `streak_unmeasured` (proves each unmeasured input is actually produced by
//!   the fixture, so the RED is about precedence, not about the fixture).
//!
//! Measurement point: rev 85139858b, 2026-11-16. Expected on that rev: the two
//! precedence tests FAIL with reason `session_compacted`; controls pass.
//!
//! What these tests do NOT prove: the relative order of `budget_unmeasured`
//! vs `streak_unmeasured` when both are unmeasured together with a compaction
//! (not exercised); the precedence of `session_compacted` vs a MEASURED
//! budget-over-cap / failure-streak / stall (not exercised here); the
//! claim-registry (non `--session`) route to the transcript; the journal
//! (`*.circuit-log.jsonl`) contents — only stdout JSON `reason` / exit code.
//!
//! Written by an independent test author, not the implementer.
//!
//! Runs the real binary against a per-test `$HOME` (condukt state dir,
//! `~/.claude/projects`, `~/.budgetguard` all resolve under it) in a throwaway
//! git repo with `GIT_CONFIG_GLOBAL=/dev/null`; nothing under the real
//! `~/.condukt`, `~/.budgetguard` or `~/.overwatch` is touched.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const SESSION: &str = "sess-compacted-below-unmeasured";

/// Shape copied from circuit_session_compacted_e2e.rs (a real Claude Code
/// transcript's compaction marker: top-level `type`/`subtype`).
const BOUNDARY: &str = r#"{"parentUuid":null,"isSidechain":false,"type":"system","subtype":"compact_boundary","content":"Conversation compacted","level":"info","compactMetadata":{"trigger":"auto","preTokens":272284,"postTokens":12126},"sessionId":"sess-compacted-below-unmeasured"}"#;
const USER: &str = r#"{"type":"user","message":{"role":"user","content":"hello"},"sessionId":"sess-compacted-below-unmeasured"}"#;

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

fn find_release_logs(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            find_release_logs(&p, out);
        } else if p
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.ends_with(".release-log.jsonl"))
        {
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

    fn run_id() -> String {
        format!("flow-{SESSION}")
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

    /// `<home>/.claude/projects/-fixture/<SESSION>.jsonl`, written fresh
    /// (mtime = now, so the 1800s idle axis never trips).
    fn write_transcript(&self, compactions: usize) {
        let dir = self.home.join(".claude").join("projects").join("-fixture");
        std::fs::create_dir_all(&dir).unwrap();
        let mut body = String::new();
        body.push_str(USER);
        body.push('\n');
        for _ in 0..compactions {
            body.push_str(BOUNDARY);
            body.push('\n');
            body.push_str(USER);
            body.push('\n');
        }
        std::fs::write(dir.join(format!("{SESSION}.jsonl")), body).unwrap();
    }

    fn corrupt_budget_ledger(&self) {
        let p = self
            .home
            .join(".budgetguard")
            .join("state")
            .join("ledger.json");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, "{not json").unwrap();
    }

    /// Create the run's item-outcome log via the real `state release-task`
    /// (a `done` outcome), then append an unparseable line, so the stateless
    /// run's failure streak is uncountable.
    fn corrupt_release_log(&self) {
        let run = Self::run_id();
        let o = self.condukt(&[
            "state",
            "release-task",
            "--run",
            run.as_str(),
            "--hashkey",
            "hk-1",
            "--outcome",
            "done",
        ]);
        assert!(o.status.success(), "apparatus: release-task failed: {o:?}");
        let mut logs = Vec::new();
        find_release_logs(&self.home, &mut logs);
        find_release_logs(&self.repo, &mut logs);
        assert_eq!(
            logs.len(),
            1,
            "apparatus: expected one release log: {logs:?}"
        );
        let log = logs.pop().unwrap();
        let mut text = std::fs::read_to_string(&log).unwrap();
        text.push_str("{not json\n");
        std::fs::write(&log, text).unwrap();
    }

    /// `circuit check --run flow-S --session S --idle-ttl-secs 1800` plus
    /// `extra`; returns (exit code, stdout JSON, stderr).
    fn circuit(&self, extra: &[&str]) -> (i32, serde_json::Value, String) {
        let run = Self::run_id();
        let mut args = vec![
            "circuit",
            "check",
            "--run",
            run.as_str(),
            "--idle-ttl-secs",
            "1800",
            "--session",
            SESSION,
        ];
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

/// CONTROL: 2 compactions (default max 2), nothing unmeasured →
/// `session_compacted`. Proves the transcript is located via `--session`.
#[test]
fn control_compacted_alone_reports_session_compacted() {
    let fx = Fixture::new();
    fx.write_transcript(2);
    let (code, v, stderr) = fx.circuit(&["--budget-cap-usd", "1"]);
    assert_eq!(code, 1, "CONTROL: must trip: {v} {stderr}");
    assert_eq!(v["reason"], "session_compacted", "CONTROL: {v}");
}

/// CONTROL: no compactions + corrupt ledger (cap set) → `budget_unmeasured`.
#[test]
fn control_corrupt_ledger_alone_reports_budget_unmeasured() {
    let fx = Fixture::new();
    fx.write_transcript(0);
    fx.corrupt_budget_ledger();
    let (code, v, stderr) = fx.circuit(&["--budget-cap-usd", "1"]);
    assert_eq!(code, 1, "CONTROL: must trip: {v} {stderr}");
    assert_eq!(v["reason"], "budget_unmeasured", "CONTROL: {v}");
}

/// CONTROL: no compactions + corrupt release log (streak axis on by default)
/// → `streak_unmeasured`.
#[test]
fn control_corrupt_release_log_alone_reports_streak_unmeasured() {
    let fx = Fixture::new();
    fx.write_transcript(0);
    fx.corrupt_release_log();
    let (code, v, stderr) = fx.circuit(&[]);
    assert_eq!(code, 1, "CONTROL: must trip: {v} {stderr}");
    assert_eq!(v["reason"], "streak_unmeasured", "CONTROL: {v}");
}

/// Compacted + unmeasured budget → `budget_unmeasured` (compaction is lowest).
#[test]
fn compacted_session_with_unmeasured_budget_reports_budget_unmeasured() {
    let fx = Fixture::new();
    fx.write_transcript(2);
    fx.corrupt_budget_ledger();
    let (code, v, stderr) = fx.circuit(&["--budget-cap-usd", "1"]);
    eprintln!("observation: exit={code} stdout={v} stderr={stderr}");
    assert_eq!(code, 1, "must trip, not continue: {v}");
    assert_eq!(
        v["reason"], "budget_unmeasured",
        "session_compacted is documented as lowest precedence; a compacted session \
         with a corrupt budget ledger (cap set) must report budget_unmeasured; full={v}"
    );
}

/// Compacted + unmeasured streak (streak axis on) → `streak_unmeasured`.
#[test]
fn compacted_session_with_unmeasured_streak_reports_streak_unmeasured() {
    let fx = Fixture::new();
    fx.write_transcript(2);
    fx.corrupt_release_log();
    let (code, v, stderr) = fx.circuit(&["--streak-cap", "3"]);
    eprintln!("observation: exit={code} stdout={v} stderr={stderr}");
    assert_eq!(code, 1, "must trip, not continue: {v}");
    assert_eq!(
        v["reason"], "streak_unmeasured",
        "session_compacted is documented as lowest precedence; a compacted session \
         whose failure streak is uncountable (streak axis on) must report \
         streak_unmeasured; full={v}"
    );
}
