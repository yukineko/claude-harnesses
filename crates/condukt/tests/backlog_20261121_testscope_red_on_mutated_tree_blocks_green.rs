#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Observed 2026-11-21 in the real ledger ~/.condukt/testscope/ledger.v2.tsv,
//! in a worktree whose HEAD was the fix commit (clean tree T_fix). The
//! orchestrator ran ONE Bash call:
//!
//! `cd <wt> && git checkout 9ef69638f -- crates/blastguard/src/detect/ast/b5.rs
//!  crates/blastguard/src/detect/payload/determined_output.rs
//!  crates/blastguard/src/detect/readings.rs && git status --short &&
//!  . "$HOME/.cargo/env" && tdd red --task t3 --cmd "cargo test -p blastguard
//!  --test backlog_20261120_procsub_preexisting_holes" 2>&1 | grep ... | head`
//!
//! i.e. it reverted the implementation files to the pre-fix revision inside the
//! same call, then ran `tdd red` (which really ran on the reverted tree and
//! FAILED = genuine RED). The ledger recorded pending rows for that call keyed on
//! the tree BEFORE the call (`4406711018eb...:1dcbe1ce95dcba8e` = T_fix), and
//! PostToolUse confirmed them. The files were then restored (tree back to T_fix,
//! clean). `tdd green --task t3 --cmd "cargo test -p blastguard --test
//! backlog_20261120_procsub_preexisting_holes"` on T_fix was then denied
//! verbatim: "rerun refused: `tdd red --cmd cargo test -p blastguard --test
//! backlog_20261120_procsub_preexisting_holes` already ran on this exact tree
//! (4406711018eb...:1dcbe1ce95dcba8e ...)". The RED that ran on the reverted
//! tree is recorded as having run on T_fix, so the F->P oracle's GREEN can never
//! be recorded.
//!
//! Asserted (the RED): pre(`git checkout HEAD~1 -- f && tdd red --cmd X`) is
//! allowed; the call's mutation is applied; post-confirm for that call; the file
//! is restored so the tree equals the pre-call tree (checked with
//! `git status --porcelain`); then pre(`tdd green --cmd X`) on a new tool_use_id
//! must NOT be a deny. Ask or allow are both acceptable: the property is "a path
//! to record GREEN remains".
//!
//! Controls (not RED): (c1) the plain same-tree sequence with no in-call
//! mutation -- `tdd red --cmd X` Pre(allow)+Post on tree T, then `tdd green --cmd
//! X` on the same T -- is still denied (ruling 0c301e8b), so the RED cannot pass
//! by deleting that rule; (c2) a bare `cargo test -p x --test t` Pre+Post, then
//! the same bare command again on the same tree, is denied.
//!
//! DOES NOT PROVE: which fix is right (key the rows on the post-call tree, skip
//! recording calls that mutate tracked files, downgrade to Ask, ...); the case
//! where one call mutates AND restores the tree before ending (pre-call and
//! post-call trees equal, the run really did happen on a different tree); that
//! the `tdd` binary or the F->P oracle behave correctly (commands are only
//! judged by the hooks, never executed; the mutation is simulated by this test);
//! nested `--cmd` forms or other mutating commands than `git checkout -- <file>`.
//!
//! RED written by an independent test author, not the implementer.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use tempfile::TempDir;

const BARE: &str = "cargo test -p x --test t";
const RED: &str = "tdd red --task t --cmd \"cargo test -p x --test t\"";
const GREEN: &str = "tdd green --task t --cmd \"cargo test -p x --test t\"";
const MUTATING_RED: &str =
    "git checkout HEAD~1 -- a.txt && tdd red --task t --cmd \"cargo test -p x --test t\"";

fn git_out(dir: &Path, args: &[&str]) -> String {
    let out = harness_core::git_env::scrub(&mut Command::new("git"))
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?} failed");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

struct Env {
    home: TempDir,
    _ledger_dir: TempDir,
    ledger: PathBuf,
    root: TempDir,
}

/// Two commits touching a.txt so `git checkout HEAD~1 -- a.txt` is meaningful.
fn env() -> Env {
    let home = tempfile::tempdir().unwrap();
    let ledger_dir = tempfile::tempdir().unwrap();
    let ledger = ledger_dir.path().join("ledger.tsv");
    let root = tempfile::tempdir().unwrap();
    let p = root.path();
    git_out(p, &["init", "-q", "-b", "main"]);
    std::fs::write(p.join("a.txt"), "one\n").unwrap();
    git_out(p, &["add", "a.txt"]);
    git_out(p, &["commit", "-q", "-m", "init"]);
    std::fs::write(p.join("a.txt"), "two\n").unwrap();
    git_out(p, &["commit", "-q", "-am", "fix"]);
    Env {
        home,
        _ledger_dir: ledger_dir,
        ledger,
        root,
    }
}

fn send_as(e: &Env, sub: &str, kind: &str, cmd: &str, tuid: &str) -> (Option<i32>, String) {
    let cwd = e.root.path();
    let payload = serde_json::json!({
        "hook_event_name": kind,
        "session_id": "s",
        "tool_name": "Bash",
        "tool_input": {"command": cmd},
        "cwd": cwd.to_str().unwrap(),
        "tool_use_id": tuid,
    });
    let mut child = harness_core::git_env::scrub(&mut Command::new(env!("CARGO_BIN_EXE_condukt")))
        .arg(sub)
        .env("HOME", e.home.path())
        .env("CONDUKT_TESTSCOPE_LEDGER", &e.ledger)
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

fn pre(e: &Env, cmd: &str, tuid: &str) -> (Option<i32>, String) {
    send_as(e, "testscope", "PreToolUse", cmd, tuid)
}

fn post(e: &Env, cmd: &str, tuid: &str) {
    let c = send_as(e, "testscope-confirm", "PostToolUse", cmd, tuid);
    assert_eq!(c.0, Some(0), "confirm of {cmd:?} must exit 0: {c:?}");
}

fn decision(res: &(Option<i32>, String)) -> String {
    assert_eq!(res.0, Some(0), "hook must exit 0: {res:?}");
    if res.1.trim().is_empty() {
        return "allow".into();
    }
    let v: serde_json::Value = serde_json::from_str(&res.1)
        .unwrap_or_else(|err| panic!("stdout not JSON ({err}): {:?}", res.1));
    v["hookSpecificOutput"]["permissionDecision"]
        .as_str()
        .unwrap_or("<none>")
        .to_string()
}

fn ran(e: &Env, cmd: &str, tuid: &str) {
    let r = pre(e, cmd, tuid);
    assert_eq!(
        decision(&r),
        "allow",
        "run of {cmd:?} must be allowed: {r:?}"
    );
    post(e, cmd, tuid);
}

// test-weakening-justified: new RED for an unfixed defect (2026-11-21).
#[test]
fn red_tdd_green_not_denied_after_red_that_mutated_tree_in_same_call() {
    let e = env();
    let p = e.root.path();
    assert_eq!(
        git_out(p, &["status", "--porcelain"]),
        "",
        "apparatus: clean"
    );

    // 1. Pre of the mutating call is allowed.
    let r = pre(&e, MUTATING_RED, "A");
    assert_eq!(
        decision(&r),
        "allow",
        "apparatus: the mutating tdd red call must be allowed: {r:?}"
    );

    // 2. The call executes: the tracked file is reverted before the test runs.
    git_out(p, &["checkout", "HEAD~1", "--", "a.txt"]);
    assert_ne!(
        git_out(p, &["status", "--porcelain"]),
        "",
        "apparatus: the in-call mutation must change the tree"
    );

    // 3. Post of the same call.
    post(&e, MUTATING_RED, "A");

    // 4. Restore: the tree is identical to the pre-call tree again.
    git_out(p, &["checkout", "HEAD", "--", "a.txt"]);
    assert_eq!(
        git_out(p, &["status", "--porcelain"]),
        "",
        "apparatus: tree restored to the pre-call state"
    );

    // 5. GREEN on the restored tree.
    let green = pre(&e, GREEN, "B");
    assert_ne!(
        decision(&green),
        "deny",
        "tdd green refused on the restored tree because a tdd red that ran on a MUTATED tree \
         (in-call `git checkout HEAD~1 -- a.txt`) was recorded against the pre-call tree; \
         no path left to record GREEN: {green:?}"
    );
}

/// Control c1: with no in-call mutation, same-tree red then green is denied.
#[test]
fn control_plain_same_tree_red_then_green_is_denied() {
    let e = env();
    ran(&e, RED, "tu-1");
    let green = pre(&e, GREEN, "tu-2");
    assert_eq!(
        decision(&green),
        "deny",
        "same-tree tdd red then tdd green without any mutation must stay denied: {green:?}"
    );
}

/// Control c2: a bare rerun on the same tree is denied.
#[test]
fn control_bare_rerun_same_tree_is_denied() {
    let e = env();
    ran(&e, BARE, "tu-1");
    let second = pre(&e, BARE, "tu-2");
    assert_eq!(
        decision(&second),
        "deny",
        "identical bare rerun on one tree must stay denied: {second:?}"
    );
}
