#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED tests for the `condukt testscope` PreToolUse hook.
//!
//! Rules: (1) no full-suite `cargo test`/`nextest run`, (2) no mid-run merge of
//! main inside a condukt worktree, (3) no same-tree rerun of a test command,
//! plus fail-closed when the tree identity cannot be determined.

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
    let st = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args)
        .current_dir(dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(st.success(), "git {args:?} failed");
}

/// A git repo with one commit and a tracked file `a.txt`.
fn repo() -> TempDir {
    let d = tempfile::tempdir().unwrap();
    git(d.path(), &["init", "-q"]);
    std::fs::write(d.path().join("a.txt"), "one\n").unwrap();
    git(d.path(), &["add", "a.txt"]);
    git(d.path(), &["commit", "-q", "-m", "init"]);
    d
}

/// Returns (exit code, stdout).
fn run(env: &Env, cmd: &str, cwd: &Path) -> (Option<i32>, String) {
    run_tool(env, "Bash", cmd, cwd)
}

fn run_tool(env: &Env, tool: &str, cmd: &str, cwd: &Path) -> (Option<i32>, String) {
    let payload = serde_json::json!({
        "session_id": "s",
        "tool_name": tool,
        "tool_input": {"command": cmd},
        "cwd": cwd.to_str().unwrap(),
    });
    let mut child = Command::new(env!("CARGO_BIN_EXE_condukt"))
        .arg("testscope")
        .env("HOME", env.home.path())
        .env("CONDUKT_TESTSCOPE_LEDGER", env.ledger())
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

/// Asserts DENY and returns the reason.
fn assert_deny(res: &(Option<i32>, String), what: &str) -> String {
    assert_eq!(
        res.0,
        Some(0),
        "{what}: exit must be 0 (stdout={:?})",
        res.1
    );
    let v: serde_json::Value = serde_json::from_str(&res.1)
        .unwrap_or_else(|e| panic!("{what}: stdout not JSON ({e}): {:?}", res.1));
    let h = &v["hookSpecificOutput"];
    assert_eq!(h["permissionDecision"], "deny", "{what}: {v}");
    let reason = h["permissionDecisionReason"].as_str().unwrap_or("");
    assert!(!reason.is_empty(), "{what}: empty reason");
    reason.to_string()
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
        "{what}: expected empty stdout, got {:?}",
        res.1
    );
}

fn deny_cases_in_repo(cmds: &[&str]) {
    for c in cmds {
        let env = Env::new();
        let r = repo();
        let res = run(&env, c, r.path());
        assert_deny(&res, c);
    }
}

fn allow_cases_in_repo(cmds: &[&str]) {
    for c in cmds {
        let env = Env::new();
        let r = repo();
        let res = run(&env, c, r.path());
        assert_allow(&res, c);
    }
}

#[test]
fn rule1_full_suite_is_denied() {
    deny_cases_in_repo(&[
        "cargo test -p blastguard --no-fail-fast",
        "cargo test",
        "cargo test -p x -- foo",
        "cd /x && cargo test -p x",
        "cargo test --workspace --lib",
        "cargo test --all --test y",
        "bash -c 'cargo test -p x'",
        "sh -c 'cargo test -p x'",
        "FOO=1 cargo test -p x",
        "env FOO=1 cargo test -p x",
        "timeout 60 cargo test -p x",
        "cargo +nightly test -p x",
        "cargo test -p x --tests",
        "cargo nextest run -p x",
        "cargo test -p x -- --test y",
    ]);
}

#[test]
fn rule1_targeted_and_non_test_commands_are_allowed() {
    allow_cases_in_repo(&[
        "cargo test -p x --test y",
        "cargo test -p x --lib foo",
        "cargo test -p x --bin b",
        "cargo test -p x --doc",
        "cargo test -p x --example e",
        "cargo test -p x --bench b",
        "cargo test -p x --test y -- --ignored",
        "cargo build -p x",
        "cargo check",
        "git commit -m \"cargo test -p x\"",
        "echo 'cargo test'",
        "cat <<'EOF' > f.txt\ncargo test -p x\nEOF",
    ]);
}

#[test]
fn rule1_unparseable_with_cargo_test_is_denied() {
    deny_cases_in_repo(&["echo 'unterminated && cargo test -p x --test y"]);
}

#[test]
fn non_bash_tool_is_allowed() {
    let env = Env::new();
    let r = repo();
    let res = run_tool(&env, "Edit", "cargo test", r.path());
    assert_allow(&res, "non-Bash tool");
}

#[test]
fn rule2_main_merge_inside_condukt_worktree_is_denied() {
    for c in [
        "git merge main",
        "git merge origin/main",
        "git pull",
        "git pull origin main",
        "git rebase origin/main",
    ] {
        let env = Env::new();
        let wt = env.home.path().join(".condukt/worktrees/wt1");
        std::fs::create_dir_all(&wt).unwrap();
        git(&wt, &["init", "-q"]);
        let res = run(&env, c, &wt);
        assert_deny(&res, c);
    }
}

#[test]
fn rule2_same_commands_outside_worktree_are_allowed() {
    allow_cases_in_repo(&[
        "git merge main",
        "git merge origin/main",
        "git pull",
        "git pull origin main",
        "git rebase origin/main",
    ]);
}

#[test]
fn rule2_non_main_merge_inside_worktree_is_allowed() {
    let env = Env::new();
    let wt = env.home.path().join(".condukt/worktrees/wt1");
    std::fs::create_dir_all(&wt).unwrap();
    git(&wt, &["init", "-q"]);
    let res = run(&env, "git merge feature-x", &wt);
    assert_allow(&res, "git merge feature-x in worktree");
}

#[test]
fn rule3_same_tree_rerun_is_denied_until_tree_changes() {
    let env = Env::new();
    let r = repo();
    let cmd = "cargo test -p x --test y";

    assert_allow(&run(&env, cmd, r.path()), "first run");

    let second = run(&env, cmd, r.path());
    let reason = assert_deny(&second, "second run, unchanged tree");
    assert!(
        reason.to_lowercase().contains("rerun"),
        "reason should mention rerun: {reason}"
    );

    std::fs::write(r.path().join("a.txt"), "two\n").unwrap();
    assert_allow(&run(&env, cmd, r.path()), "after tracked file modified");
}

#[test]
fn fail_closed_when_tree_identity_unknown() {
    let env = Env::new();
    let not_repo = tempfile::tempdir().unwrap();
    let res = run(&env, "cargo test -p x --test y", not_repo.path());
    assert_deny(&res, "test command outside a git repo");
}
