#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Observed 2026-11-21 (main 11c04144d, run run-20261121-213949-96292, task t1,
//! kind fix). The main loop ran the task's reproduction test
//! `cargo test -p blastguard --test backlog_20261121_driver_plugin_bin_flow_watch`
//! in the task worktree (Phase 6 deterministic pre-run the condukt SKILL
//! mandates). The worker's `tdd red --task t1 --cmd "<same cmd>"` was refused,
//! verbatim: "rerun refused: `cargo test -p blastguard --test
//! backlog_20261121_driver_plugin_bin_flow_watch` already ran on this exact tree
//! (11c04144d…:ed2bad9ab266d79e …); running the same test twice is forbidden
//! (user ruling 2026-11-07)"; `tdd green --task t1` then refused "tdd: no RED
//! proof found"; `condukt state set --status verified` refused "no valid
//! fail-to-pass reproduction oracle (tdd oracle exited non-zero …); no tdd proof
//! files (*.red.json / *.green.json) found in <worktree>; the oracle looks for
//! 't1'". An independent verifier had observed a real F->P (base FAILED deny,
//! fix 2 passed), so a correct kind:fix task could not reach verified.
//!
//! Measured at rev c15091db8: the module doc of `crates/condukt/src/testscope.rs`
//! says "A `tdd red --cmd <c>` is checked against the bare key like a bare run",
//! while `tdd green --cmd <c>` is exempted from the bare key (backlog 8f82f09d,
//! "so a bare run cannot block the GREEN evidence the F->P oracle requires").
//! The RED side has no such exemption.
//!
//! Two readings:
//! (a) defect: `tdd red|green` proof runs must be exempt from / reconciled with
//!     the rerun ledger (as 8f82f09d did for green only), or the ledger's earlier
//!     run must count as the proof; otherwise one plain run on a tree makes the
//!     RED proof unrecordable there and `verified` is unreachable.
//! (b) intended: the worker must run `tdd red` before any plain run; then the
//!     defect is that the SKILL's Phase 6 main-loop pre-run (and a failed first
//!     `tdd` attempt, e.g. a missing `--task`) consumes the single allowed run.
//!
//! Asserted (RED): on one tree, a confirmed bare `cargo test -p x --test t`
//! followed by `tdd red --task t1 --cmd "cargo test -p x --test t"` is not
//! denied, i.e. a path to record the RED proof remains (mirror of 8f82f09d).
//! Controls (current intended behaviour, not RED): a bare rerun on the same tree
//! is still denied; a `tdd red` with no prior run on the tree is allowed.
//!
//! DOES NOT PROVE: which reading is right or how to fix it (exempt `tdd red`,
//! import the ledger run as proof, or reorder the SKILL); anything about the
//! `tdd` binary or `tdd oracle` (commands are only judged by the PreToolUse
//! hook, never executed); whether the observed pre-run and the refused red were
//! on the base or the fixed tree; `condukt state set --status verified` itself.
//!
//! RED written by an independent test author, not the implementer.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use tempfile::TempDir;

const BARE: &str = "cargo test -p x --test t";
const RED: &str = "tdd red --task t1 --cmd \"cargo test -p x --test t\"";

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

// test-weakening-justified: new RED for an unfixed defect (2026-11-21).
#[test]
fn red_tdd_red_after_bare_run_on_same_tree_is_not_denied() {
    let e = env();
    ran(&e, BARE, "tu-1");
    let red = pre(&e, RED, "tu-2");
    assert_ne!(
        decision(&red),
        "deny",
        "tdd red refused as a rerun after a confirmed bare run of the same test on the same \
         tree; no path left to record the RED proof the F->P oracle requires: {red:?}"
    );
}

/// Control: the identical bare command twice on one tree is still denied.
#[test]
fn control_bare_rerun_on_same_tree_is_denied() {
    let e = env();
    ran(&e, BARE, "tu-1");
    let second = pre(&e, BARE, "tu-2");
    assert_eq!(decision(&second), "deny", "bare rerun: {second:?}");
}

/// Control: `tdd red` with no prior run on the tree is allowed.
#[test]
fn control_tdd_red_first_on_tree_is_allowed() {
    let e = env();
    let red = pre(&e, RED, "tu-1");
    assert_eq!(decision(&red), "allow", "first tdd red: {red:?}");
}
