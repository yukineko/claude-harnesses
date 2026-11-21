#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Backlog RED (2026-11-21, measured on main 0209adc6d; named by the
//! independent t1 verifier of condukt run run-20261121-225214-62742): the
//! testscope full-suite rule is skipped when a cargo global option that takes
//! a separate value comes before the subcommand.
//!
//! `judge_cargo` (crates/condukt/src/testscope.rs) skips leading global flags
//! with `while args.get(i).is_some_and(|a| a.starts_with('+') || a.starts_with('-'))`,
//! one token per flag. The value of `--color always` / `--config k=v` is then
//! taken as the subcommand, which is neither `test` nor `nextest`, so the
//! command is judged `Other` and allowed. cargo 1.97.1 accepts both spellings
//! as global options (`cargo --color always --version` and
//! `cargo --config net.offline=true --version` both print the version), so
//! `cargo --color always test` builds and runs the whole suite.
//!
//! Readings: (a) fail-open: the full-suite deny is bypassed; (b) intended:
//! none found (the rule's reason is that a bare `cargo test` runs the whole
//! suite, which the global option does not change). Telling apart: these
//! asserts; the control shows the `=` spelling is denied in the same fixture.
//!
//! DOES NOT PROVE: `-C <dir>` / `-Z <flag>` / `--manifest-path` spellings;
//! that cargo would actually run tests (nothing is executed here).

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
        "session_id": "s-global-flag",
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

#[test]
#[ignore = "backlog RED: cargo global option with a separate value before `test` skips the full-suite deny"]
fn global_option_value_before_test_is_still_denied() {
    for cmd in [
        "cargo --color always test",
        "cargo --config net.offline=true test",
    ] {
        let env = Env::new();
        let r = repo();
        let res = hook(&env, cmd, r.path());
        assert_deny(&res, cmd);
    }
}

#[test]
#[ignore = "backlog RED: control for the global-option-value testscope bypass"]
fn control_equals_spelling_is_denied() {
    let env = Env::new();
    let r = repo();
    let res = hook(&env, "cargo --color=always test", r.path());
    assert_deny(&res, "cargo --color=always test");
}
