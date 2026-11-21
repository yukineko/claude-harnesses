#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Independent spelling pins for backlog 669b28ea (written by an independent
//! test author who did NOT implement the fix in crates/condukt/src/testscope.rs
//! `judge_cargo`/`cargo_global`).
//!
//! PROVES (via the `condukt testscope` PreToolUse(Bash) hook, command only
//! judged, never run): with no target selector, these spellings of cargo
//! global options before `test` are denied as a full-suite run: `-C dir`,
//! `-Zunstable-options`, `-Z unstable-options`, `+nightly --color always`,
//! `-q --config a=b`, `--color always t`, `-vC dir`, `--explain E0001`;
//! unknown global options (`--bogus`, `--manifest-path x`, `--locked=x`)
//! before the subcommand resolve restrictively (deny); and non-test
//! invocations (`--version`, `-V`, `--list`, `build` with the same global
//! option shapes) are allowed (empty stdout).
//!
//! DOES NOT PROVE: that cargo itself accepts each spelling; options placed
//! after the subcommand; `nextest`; behaviour inside a run task worktree;
//! any spelling not listed here.

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
fn global_option_spellings_before_test_are_denied() {
    for cmd in [
        "cargo -C dir test",
        "cargo -Zunstable-options test",
        "cargo -Z unstable-options test",
        "cargo +nightly --color always test",
        "cargo -q --config a=b test",
        "cargo --color always t",
        "cargo -vC dir test",
        "cargo --explain E0001 test",
    ] {
        let env = Env::new();
        let r = repo();
        assert_deny(&hook(&env, cmd, r.path()), cmd);
    }
}

#[test]
fn unknown_global_option_before_subcommand_is_denied() {
    for cmd in [
        "cargo --bogus test",
        "cargo --manifest-path x test",
        "cargo --locked=x test",
    ] {
        let env = Env::new();
        let r = repo();
        assert_deny(&hook(&env, cmd, r.path()), cmd);
    }
}

#[test]
fn non_test_invocations_are_not_denied() {
    for cmd in [
        "cargo --version",
        "cargo -V",
        "cargo --list",
        "cargo build",
        "cargo +nightly build",
        "cargo --color always build",
        "cargo -vv build",
    ] {
        let env = Env::new();
        let r = repo();
        assert_allow(&hook(&env, cmd, r.path()), cmd);
    }
}
