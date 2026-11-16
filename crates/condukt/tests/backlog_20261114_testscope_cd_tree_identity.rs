#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro (observed 2026-11-14 at main 1bd903112 by the worker of condukt
//! run run-20261114-110017-12373, task worktree
//! run-20261114-110017-12373-9352d929): the testscope rerun guard keys the
//! tested tree on the hook payload `cwd`, not on the directory a leading
//! `cd <dir> &&` moves into.
//!
//! Mechanism (crates/condukt/src/testscope.rs `check_rerun` -> `tree_identity(cwd)`):
//! `cd` is an ordinary segment (`Judged::Other`) and never changes the `cwd`
//! passed to `tree_identity`. Observed in the wild: every ledger key for the
//! worker's worktree was `/Users/yuki/src/harness\tb0b4ac6e...:1dcbe1ce95dcba8e`
//! (the session root), unchanged after code edits, so a test command a previous
//! worker ran in another worktree was refused with "rerun refused: ... already
//! ran on this exact tree" although the tested tree differed.
//!
//! Two readings: (a) real defect — the guard promises "same *tree*" and the
//! tree under test is the `cd` target; (b) intended — the guard is defined on
//! the payload cwd only and workers must not `cd` (then the refusal text
//! "this exact tree" is misleading). Tell them apart by the rule's docstring in
//! testscope.rs: it speaks of the tree a test runs against.
//!
//! Controls (GREEN at HEAD, not ignored): an identical command on an unchanged
//! `cd` target is still refused; with no `cd`, the tree the test runs in IS the
//! payload cwd, so editing it allows a rerun.
//!
//! User ruling (binding): the rerun fingerprint is taken ONLY from the tree the
//! test actually runs in. The payload cwd's tree must not contribute when a
//! `cd` moved the test elsewhere, so editing only the payload cwd must NOT
//! allow a rerun of the same `cd <target> && ...` command (RED test below).
//!
//! DOES NOT PROVE: how `cd` should be parsed (chains, `pushd`, `git -C`,
//! subshells), only that a leading `cd <dir> &&` target edit must change the
//! tree identity.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use tempfile::TempDir;

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

fn repo() -> TempDir {
    let d = tempfile::tempdir().unwrap();
    git(d.path(), &["init", "-q"]);
    std::fs::write(d.path().join("a.txt"), "one\n").unwrap();
    git(d.path(), &["add", "a.txt"]);
    git(d.path(), &["commit", "-q", "-m", "init"]);
    d
}

fn run(home: &Path, ledger: &Path, cmd: &str, cwd: &Path) -> (Option<i32>, String) {
    run_as("testscope", None, home, ledger, cmd, cwd)
}

/// `run`, then (when the hook allowed it, i.e. the command ran) the
/// PostToolUse that confirms the run (spec testscope-confirmed-run R2: input
/// only, the asserts stay as they were).
fn run_confirmed(home: &Path, ledger: &Path, cmd: &str, cwd: &Path) -> (Option<i32>, String) {
    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let id = format!("tu-{}", N.fetch_add(1, std::sync::atomic::Ordering::SeqCst));
    let res = run_as("testscope", Some(&id), home, ledger, cmd, cwd);
    if res.1.trim().is_empty() {
        run_as("testscope-confirm", Some(&id), home, ledger, cmd, cwd);
    }
    res
}

fn run_as(
    sub: &str,
    tuid: Option<&str>,
    home: &Path,
    ledger: &Path,
    cmd: &str,
    cwd: &Path,
) -> (Option<i32>, String) {
    let mut payload = serde_json::json!({
        "session_id": "s",
        "tool_name": "Bash",
        "tool_input": {"command": cmd},
        "cwd": cwd.to_str().unwrap(),
    });
    if let Some(id) = tuid {
        payload["tool_use_id"] = serde_json::json!(id);
    }
    let mut child = Command::new(env!("CARGO_BIN_EXE_condukt"))
        .arg(sub)
        .env("HOME", home)
        .env("CONDUKT_TESTSCOPE_LEDGER", ledger)
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

fn denied(res: &(Option<i32>, String)) -> bool {
    res.1.contains("\"deny\"")
}

struct Fx {
    home: TempDir,
    ledger_dir: TempDir,
    session_root: TempDir,
    target: TempDir,
}

impl Fx {
    fn new() -> Self {
        Fx {
            home: tempfile::tempdir().unwrap(),
            ledger_dir: tempfile::tempdir().unwrap(),
            session_root: repo(),
            target: repo(),
        }
    }
    fn ledger(&self) -> PathBuf {
        self.ledger_dir.path().join("ledger.tsv")
    }
    fn cmd(&self) -> String {
        format!(
            "cd {} && cargo test -p foo --test bar",
            self.target.path().display()
        )
    }
    fn go(&self, cmd: &str) -> (Option<i32>, String) {
        run(
            self.home.path(),
            &self.ledger(),
            cmd,
            self.session_root.path(),
        )
    }
    fn go_confirmed(&self, cmd: &str) -> (Option<i32>, String) {
        run_confirmed(
            self.home.path(),
            &self.ledger(),
            cmd,
            self.session_root.path(),
        )
    }
}

// test-weakening-justified: new RED for an unfixed defect (backlog pending,
// 2026-11-14); the ignore is removed in the fixing commit.
#[test]
fn rerun_after_editing_the_cd_target_tree_is_allowed() {
    let fx = Fx::new();
    let cmd = fx.cmd();
    let first = fx.go(&cmd);
    assert!(!denied(&first), "first run must be allowed: {first:?}");
    // The tree the test runs against (the cd target) changes.
    std::fs::write(fx.target.path().join("a.txt"), "two\n").unwrap();
    let second = fx.go(&cmd);
    assert!(
        !denied(&second),
        "cd target tree changed but the rerun was refused (keyed on payload cwd): {second:?}"
    );
}

// Control: unchanged cd target, identical command: still refused.
#[test]
fn control_identical_command_on_unchanged_cd_target_is_refused() {
    let fx = Fx::new();
    let cmd = fx.cmd();
    assert!(!denied(&fx.go_confirmed(&cmd)));
    let second = fx.go(&cmd);
    assert!(denied(&second), "same command, same trees: {second:?}");
}

// test-weakening-justified: new RED for an unfixed defect (user ruling
// 2026-11-15; replaces the removed control that pinned the overruled
// behaviour); the ignore is removed in the fixing commit.
#[test]
fn rerun_after_editing_only_the_payload_cwd_tree_is_refused_when_test_runs_in_cd_target() {
    let fx = Fx::new();
    let cmd = fx.cmd();
    let first = fx.go_confirmed(&cmd);
    assert!(!denied(&first), "first run must be allowed: {first:?}");
    // Only the payload cwd (session root) changes; the run tree (cd target) is identical.
    std::fs::write(fx.session_root.path().join("a.txt"), "two\n").unwrap();
    let second = fx.go(&cmd);
    assert!(
        denied(&second),
        "run tree unchanged, only payload cwd edited, yet the rerun was allowed: {second:?}"
    );
}

// Control: no cd, so the run tree equals the payload cwd; editing it allows a rerun.
#[test]
fn control_editing_run_tree_equal_to_payload_cwd_allows_rerun_without_cd() {
    let fx = Fx::new();
    let cmd = "cargo test -p foo --test bar";
    let first = fx.go(cmd);
    assert!(!denied(&first), "first run must be allowed: {first:?}");
    std::fs::write(fx.session_root.path().join("a.txt"), "two\n").unwrap();
    let second = fx.go(cmd);
    assert!(!denied(&second), "{second:?}");
}
