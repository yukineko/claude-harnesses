#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (backlog filing, 2026-11-22, main 950c8cdce): `text_value_opts("merge")`
//! in crates/condukt/src/testscope/git.rs is the exact list
//! `["-m", "--message", "-F", "--file"]`, so the separated value of any other
//! text-valued merge option (`--into-name <branch>`, which only names the
//! branch for the merge message) is collected as a commit operand and judged
//! as a ref. In a condukt task worktree `git merge --into-name main x` is
//! therefore denied as a main merge although `x` is the only commit operand.
//!
//! DOES NOT PROVE: anything about `git merge --mess main` (an abbreviation of
//! `--message`; with no commit operand git merges the upstream, so deny may be
//! correct -- deliberately NOT asserted); nothing is executed by git.

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

/// Controls: the fixture denies a plain main merge and allows a plain branch.
#[test]
fn controls_main_merge_denied_feature_allowed() {
    assert_eq!(decision("git merge main"), "deny");
    assert_eq!(decision("git merge x"), "allow");
}

#[test]
fn allow_merge_into_name_value_is_not_a_commit_operand() {
    assert_eq!(decision("git merge --into-name main x"), "allow");
}
