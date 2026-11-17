#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Verifier observation on 310d9a671 (backlog 8f82f09d), verbatim:
//! 「`tdd red` を実行した後の `tdd green` は、同一 tree でも以前は拒否だったのが
//! Allow に変わった」「これをテストで固定したものは見当たらない」.
//!
//! Measured at rev cd09054b1, 2026-11-17. `judge_words` keys `tdd red --cmd <c>`
//! as the bare words of `<c>`, and `tdd green --cmd <c>` as `<c>` tagged with
//! `TDD_GREEN_ROUTE`; `check_rerun` checks only the tagged key for a green, so a
//! confirmed `tdd red` of `<c>` never collides with a later `tdd green` of `<c>`
//! on the same tree.
//!
//! Expected (asserted by the RED): on one unchanged tree, `tdd red --cmd X`
//! (allowed, confirmed by its PostToolUse) followed by `tdd green --cmd X` is
//! denied. Nothing changed between RED and GREEN, so a GREEN taken there makes
//! the F->P oracle prove nothing.
//!
//! Two readings of the current Allow:
//! (a) defect: the 8f82f09d exception ("a bare run cannot block GREEN") was
//!     meant for a plain `cargo test` the agent happened to run, and widening it
//!     to `tdd red` lets RED->GREEN be recorded with no code change in between;
//! (b) intended: the module doc of testscope.rs says `tdd green --cmd <c>` "is
//!     checked only against earlier `tdd green` runs of `<c>`", and `tdd red`
//!     is judged like a bare run, so red->green on one tree is allowed by design.
//! How to tell them apart: only a human ruling can. The code and its module doc
//! agree with (b); the F->P oracle's purpose (RED then GREEN across a change)
//! argues for (a). If the ruling is (b), delete this RED instead of fixing code.
//!
//! Controls (current intended behaviour, not RED): after a bare `cargo test X`
//! on a tree, one `tdd green --cmd X` is allowed and a second is denied; after a
//! `tdd red --cmd X`, a `tdd green --cmd X` on a changed tree (new commit) is
//! allowed.
//!
//! DOES NOT PROVE: anything about the `tdd` binary or its F->P oracle (the
//! commands are only judged by the PreToolUse hook, never executed); the
//! behaviour when the `tdd red` attempt is unconfirmed (no PostToolUse); nested
//! `--cmd` forms; which fix is right (record a green key from `tdd red`, or key
//! `tdd red` like `tdd green`, ...).
//!
//! RED written by an independent test author, not the implementer.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use tempfile::TempDir;

const BARE: &str = "cargo test -p x --test t";
const RED: &str = "tdd red --task t --cmd \"cargo test -p x --test t\"";
const GREEN: &str = "tdd green --task t --cmd \"cargo test -p x --test t\"";

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

struct Env {
    home: TempDir,
    _ledger_dir: TempDir,
    ledger: PathBuf,
    root: TempDir,
}

fn env() -> Env {
    let home = tempfile::tempdir().unwrap();
    let ledger_dir = tempfile::tempdir().unwrap();
    let ledger = ledger_dir.path().join("ledger.tsv");
    let root = tempfile::tempdir().unwrap();
    git(root.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(root.path().join("a.txt"), "one\n").unwrap();
    git(root.path(), &["add", "a.txt"]);
    git(root.path(), &["commit", "-q", "-m", "init"]);
    Env {
        home,
        _ledger_dir: ledger_dir,
        ledger,
        root,
    }
}

/// A new commit: the tree identity the rerun key uses changes.
fn change_tree(e: &Env) {
    std::fs::write(e.root.path().join("a.txt"), "two\n").unwrap();
    git(e.root.path(), &["commit", "-q", "-am", "change"]);
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

/// An allowed run: Pre must allow it, then its PostToolUse confirms it ran.
fn ran(e: &Env, cmd: &str, tuid: &str) {
    let r = pre(e, cmd, tuid);
    assert_eq!(
        decision(&r),
        "allow",
        "run of {cmd:?} must be allowed: {r:?}"
    );
    let c = send_as(e, "testscope-confirm", "PostToolUse", cmd, tuid);
    assert_eq!(c.0, Some(0), "confirm of {cmd:?} must exit 0: {c:?}");
}

// test-weakening-justified: new RED for an unfixed defect (2026-11-17).
#[ignore = "RED: 2026-11-17 tdd green after tdd red on the same unchanged tree is allowed — remove when fixed"]
#[test]
fn red_tdd_green_after_tdd_red_on_same_tree_is_denied() {
    let e = env();
    ran(&e, RED, "tu-1");
    let green = pre(&e, GREEN, "tu-2");
    assert_eq!(
        decision(&green),
        "deny",
        "tdd green after a confirmed tdd red of the same command on the same unchanged tree \
         was not refused (GREEN without any change since RED): {green:?}"
    );
}

/// Control: the 8f82f09d exception. A bare run does not block one GREEN on the
/// same tree; a second GREEN there is refused.
#[test]
fn control_tdd_green_after_bare_run_is_allowed_once() {
    let e = env();
    ran(&e, BARE, "tu-1");
    let first = pre(&e, GREEN, "tu-2");
    assert_eq!(
        decision(&first),
        "allow",
        "first tdd green after a bare run, same tree: {first:?}"
    );
    let c = send_as(&e, "testscope-confirm", "PostToolUse", GREEN, "tu-2");
    assert_eq!(c.0, Some(0), "confirm of the green must exit 0: {c:?}");
    let second = pre(&e, GREEN, "tu-3");
    assert_eq!(
        decision(&second),
        "deny",
        "second tdd green, same tree: {second:?}"
    );
}

/// Control: after the tree changed, GREEN following RED is allowed.
#[test]
fn control_tdd_green_after_tdd_red_on_changed_tree_is_allowed() {
    let e = env();
    ran(&e, RED, "tu-1");
    change_tree(&e);
    let green = pre(&e, GREEN, "tu-2");
    assert_eq!(
        decision(&green),
        "allow",
        "tdd green after tdd red on a changed tree: {green:?}"
    );
}
