#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (2026-11-22, measured on main e3baca954; named by the independent t2
//! verifier of condukt run run-20261122-012059-29884): the testscope
//! main-merge rule (`git pull` / `git merge main` inside a condukt worktree is
//! denied) is skipped when a git global option that takes a SEPARATE value
//! comes before the subcommand.
//!
//! `judge_git` (crates/condukt/src/testscope.rs) skips `-C` / `-c` as two
//! tokens and every other `-...` token as ONE token, so the value of
//! `--git-dir <path>` / `--work-tree <path>` / `--namespace <name>` becomes the
//! subcommand, which is neither pull/merge/rebase, and the command is judged
//! `Other` (allowed). git 2.50.1 accepts the separate-value spelling (`git
//! --work-tree . status` exits 0). Same shape as the cargo bug fixed for
//! backlog 669b28ea.
//!
//! Readings: (a) fail-open: the main-merge deny is bypassed; (b) intended:
//! none found (the rule's reason does not depend on how the git dir is named).
//! Control: the `=` spelling `git --work-tree=. pull` is denied in the same
//! fixture.
//!
//! DOES NOT PROVE: other git options (`--exec-path`, `--super-prefix`,
//! `--config-env`), `git merge main` / `git rebase origin/main` spellings,
//! that git would actually pull (nothing is executed here).

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
        "session_id": "s-git-global",
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
fn control_equals_spelling_is_denied() {
    let env = Env::new();
    let wt = worktree(&env);
    assert_deny(
        &hook(&env, "git --work-tree=. pull", &wt),
        "git --work-tree=. pull",
    );
}

#[test]
#[ignore = "RED(d3f5d7a8): git global option with a separate value skips the main-merge deny"]
fn git_global_option_value_before_pull_is_still_denied() {
    for c in [
        "git --work-tree . pull",
        "git --git-dir .git pull",
        "git --namespace x pull",
    ] {
        let env = Env::new();
        let wt = worktree(&env);
        assert_deny(&hook(&env, c, &wt), c);
    }
}
