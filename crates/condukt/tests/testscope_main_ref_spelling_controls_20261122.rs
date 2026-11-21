#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Independent allow/deny controls for the testscope main-ref spelling rule
//! (backlog 643e641f). Written by a test author separate from the implementer.
//!
//! DOES NOT PROVE: that git would actually merge anything (nothing is
//! executed); spellings not listed here (e.g. a raw sha of main).

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
        "session_id": "s-git-mainref",
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

fn worktree(env: &Env) -> PathBuf {
    let wt = env.home.path().join(".condukt/worktrees/wt1");
    std::fs::create_dir_all(&wt).unwrap();
    git(&wt, &["init", "-q"]);
    wt
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

fn check_allow(cmds: &[&str]) {
    for c in cmds {
        let env = Env::new();
        let wt = worktree(&env);
        assert_allow(&hook(&env, c, &wt), c);
    }
}

fn check_deny(cmds: &[&str]) {
    for c in cmds {
        let env = Env::new();
        let wt = worktree(&env);
        assert_deny(&hook(&env, c, &wt), c);
    }
}

#[test]
fn allow_plain_branch_names() {
    check_allow(&[
        "git merge feature-x",
        "git merge condukt/run-x/t1",
        "git rebase fix/main-ui",
        "git merge mainline",
    ]);
}

#[test]
fn allow_tag_ref_and_message_mentioning_main() {
    check_allow(&[
        "git merge refs/tags/main",
        "git merge -m \"merge main\" feature-x",
    ]);
}

#[test]
fn deny_full_ref_spellings() {
    check_deny(&[
        "git merge refs/heads/main",
        "git rebase refs/remotes/origin/main",
        "git merge heads/main",
        "git merge refs/heads/master",
    ]);
}

#[test]
fn deny_qualified_and_symbolic_spellings() {
    check_deny(&[
        "git merge origin/HEAD",
        "git merge main~0",
        "git merge main^0",
        "git merge @{u}",
    ]);
}

#[test]
fn deny_undeterminable_spelling() {
    check_deny(&["git merge \"$B\""]);
}
