#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (2026-11-22, measured on main e528afafb; named as out-of-scope by the
//! independent t1 verifier of condukt run run-20261122-025656-99560): the
//! testscope main-merge rule (`git merge main` / `git rebase origin/main`
//! inside a condukt worktree is denied) only recognises the main branch by a
//! fixed string list. `judge_git` (crates/condukt/src/testscope.rs) `is_main`
//! matches exactly `main | master | origin/main | origin/master | @{u} |
//! @{upstream}`, so a full-ref spelling of the same branch (`refs/heads/main`,
//! `refs/remotes/origin/main`) is judged `Other` (allowed).
//!
//! Control: `git merge main` is denied in the same fixture.
//!
//! Readings: (a) fail-open: the main-merge deny is bypassed by naming main with
//! its full ref; (b) intended: the rule only means the short names. Tell apart:
//! the rule's reason (MAIN_MERGE_REASON) concerns integrating the default
//! branch, not how it is spelled; no ruling for (b) found.
//!
//! DOES NOT PROVE: other spellings (`main~0`, `main^0`, `origin/HEAD`, a sha of
//! main, `heads/main`), other history-moving subcommands; that git would merge
//! anything (nothing is executed here).

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

#[test]
fn control_short_main_is_denied() {
    let env = Env::new();
    let wt = worktree(&env);
    assert_deny(&hook(&env, "git merge main", &wt), "git merge main");
}

#[test]
#[ignore = "RED(643e641f): a full-ref spelling of main (refs/heads/main) skips the main-merge deny"]
fn full_ref_spelling_of_main_is_still_denied() {
    for c in [
        "git merge refs/heads/main",
        "git rebase refs/remotes/origin/main",
    ] {
        let env = Env::new();
        let wt = worktree(&env);
        assert_deny(&hook(&env, c, &wt), c);
    }
}
