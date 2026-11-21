#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (backlog filing, 2026-11-22, main 5e99d94d4): in a condukt task
//! worktree the testscope main-merge deny is skipped by operands that start
//! with `-`, because `judge_git` (crates/condukt/src/testscope/git.rs)
//! collects operands with `} else if !a.starts_with('-') { operands.push(a); }`:
//!
//! - `git rebase --onto=main x`: the attached `--onto=<main>` value is never
//!   judged (the separated `--onto main` form is judged and denied).
//! - `git merge -`: `-` is git's shorthand for `@{-1}` (the previous branch);
//!   `@{-1}` is denied as undetermined, so `-` should be too.
//!
//! DOES NOT PROVE: that git would actually merge main (nothing is executed);
//! other `--opt=<value>` options of merge/rebase.

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
        self.ledger_dir.path().join("ledger.json")
    }
}

fn git(dir: &Path, args: &[&str]) {
    let st = harness_core::git_env::scrub(&mut Command::new("git"))
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(st.success(), "git {args:?} failed");
}

fn hook(env: &Env, cmd: &str, cwd: &Path) -> (Option<i32>, String) {
    let payload = serde_json::json!({
        "session_id": "s-git-mainref-dash",
        "tool_name": "Bash",
        "tool_input": {"command": cmd},
        "cwd": cwd.to_str().unwrap(),
    });
    let mut child = harness_core::git_env::scrub(&mut Command::new(env!("CARGO_BIN_EXE_condukt")))
        .arg("testscope")
        .env("HOME", env.home.path())
        .env("CONDUKT_TESTSCOPE_LEDGER", env.ledger())
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

fn worktree(env: &Env) -> PathBuf {
    let wt = env.home.path().join(".condukt/worktrees/wt1");
    std::fs::create_dir_all(&wt).unwrap();
    git(&wt, &["init", "-q"]);
    wt
}

fn decision(cmd: &str) -> String {
    let env = Env::new();
    let wt = worktree(&env);
    let (code, out) = hook(&env, cmd, &wt);
    assert_eq!(code, Some(0), "{cmd}: exit must be 0 (stdout={out:?})");
    if out.trim().is_empty() {
        return "allow".into();
    }
    let v: serde_json::Value = serde_json::from_str(&out)
        .unwrap_or_else(|e| panic!("{cmd}: stdout not JSON ({e}): {out:?}"));
    v["hookSpecificOutput"]["permissionDecision"]
        .as_str()
        .unwrap_or("?")
        .to_string()
}

/// Controls: the fixture does deny the separated `--onto main` and `@{-1}`
/// forms and allows a plain branch, so a RED below is about the spelling.
#[test]
fn controls_separated_onto_and_at_minus_one_are_denied() {
    assert_eq!(decision("git rebase --onto main x"), "deny");
    assert_eq!(decision("git merge @{-1}"), "deny");
    assert_eq!(decision("git merge feature-x"), "allow");
}

#[test]
fn deny_rebase_onto_attached_main() {
    assert_eq!(decision("git rebase --onto=main x"), "deny");
}

#[test]
fn deny_merge_dash_previous_branch() {
    assert_eq!(decision("git merge -"), "deny");
}
