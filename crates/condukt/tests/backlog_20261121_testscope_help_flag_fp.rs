#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Backlog RED (2026-11-21, condukt run run-20261121-114054-75519): testscope
//! false positive on `cargo test --help`.
//!
//! Observed in task worktree run-20261121-114054-75519-ts6f65: `cargo test
//! --help` was denied with "`cargo test --help` names no test target, so it
//! builds and runs the whole suite: full suite is nightly-only…". `--help`
//! (and `-h`) prints cargo's usage text and builds/runs nothing, so the
//! full-suite rule (crates/condukt/src/testscope.rs `judge_cargo`) must not
//! deny it.
//!
//! Control: a bare `cargo test` in the same fixture is still denied, so the
//! allow cases cannot pass merely because the hook stopped judging.
//!
//! Driving follows tests/testscope_enforcement_20261114.rs: a temp `$HOME`, a
//! temp `CONDUKT_TESTSCOPE_LEDGER`, a temp git repo; the hook is fed a
//! PreToolUse(Bash) payload on stdin and the command is only judged, never run.
//!
//! DOES NOT PROVE: behaviour inside a condukt run task worktree (rule C); the
//! observation came from one, but the deny it quoted is the rule-1 full-suite
//! reason, which this fixture reaches without a run. `cargo nextest run
//! --help`, `cargo help test`, and `--help` combined with other flags.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use tempfile::TempDir;

struct Env {
    home: TempDir,
    ledger_dir: TempDir,
}

impl Env {
    fn new() -> Self {
        Env {
            home: tempfile::tempdir().unwrap(),
            ledger_dir: tempfile::tempdir().unwrap(),
        }
    }
    fn ledger(&self) -> PathBuf {
        self.ledger_dir.path().join("ledger.tsv")
    }
}

fn git(dir: &Path, args: &[&str]) {
    let out = harness_core::git_env::scrub(&mut Command::new("git"))
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn repo() -> TempDir {
    let d = tempfile::tempdir().unwrap();
    git(d.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(d.path().join("a.txt"), "one\n").unwrap();
    git(d.path(), &["add", "-A"]);
    git(d.path(), &["commit", "-q", "-m", "init"]);
    d
}

/// `condukt testscope` with a PreToolUse(Bash) payload. Returns (exit, stdout).
fn hook(env: &Env, cmd: &str, cwd: &Path) -> (Option<i32>, String) {
    let payload = serde_json::json!({
        "session_id": "s-help",
        "tool_name": "Bash",
        "tool_input": {"command": cmd},
        "cwd": cwd.to_str().unwrap(),
    });
    let mut child = harness_core::git_env::scrub(&mut Command::new(env!("CARGO_BIN_EXE_condukt")))
        .arg("testscope")
        .env("HOME", env.home.path())
        .env("CONDUKT_TESTSCOPE_LEDGER", env.ledger())
        .env("CONDUKT_DEFAULT_BRANCH", "main")
        .env_remove("CONDUKT_DISABLE")
        .env_remove("CONDUKT_WORKTREE_BASE")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.to_string().as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

fn assert_deny(res: &(Option<i32>, String), what: &str) {
    assert_eq!(
        res.0,
        Some(0),
        "{what}: exit must be 0 (stdout={:?})",
        res.1
    );
    let v: serde_json::Value = serde_json::from_str(&res.1).unwrap_or_else(|e| {
        panic!(
            "{what}: expected a deny, stdout not JSON ({e}): {:?}",
            res.1
        )
    });
    assert_eq!(
        v["hookSpecificOutput"]["permissionDecision"], "deny",
        "{what}: {v}"
    );
}

fn assert_allow(res: &(Option<i32>, String), what: &str) {
    assert_eq!(
        res.0,
        Some(0),
        "{what}: exit must be 0 (stdout={:?})",
        res.1
    );
    assert!(
        res.1.trim().is_empty(),
        "{what}: expected allow (empty stdout), got {:?}",
        res.1
    );
}

#[test]
fn cargo_test_help_is_not_denied() {
    for cmd in ["cargo test --help", "cargo test -h"] {
        let env = Env::new();
        let r = repo();
        let res = hook(&env, cmd, r.path());
        assert_allow(&res, cmd);
    }
}

#[test]
fn control_bare_cargo_test_is_still_denied() {
    let env = Env::new();
    let r = repo();
    let res = hook(&env, "cargo test", r.path());
    assert_deny(&res, "cargo test");
}
