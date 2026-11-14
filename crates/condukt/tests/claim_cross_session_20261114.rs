#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Cross-session file claims must not exclude (user ruling 2026-11-14).
//!
//! CLAUDE.md section 8: avoiding simultaneous edits is the rule, avoiding
//! conflicts is not. Each session works in its own worktree and integrates by
//! merge; double-start of a task is prevented by the hashkey task claim. So a
//! file claim held by a run of a DIFFERENT session no longer excludes, while a
//! claim held by a run of the SAME session still does. Undeterminable
//! (either session id None) resolves to the restrictive side: exclude.
//!
//! Drives the real binary like `claim_lockstep_files_20261114.rs`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_condukt")
}

fn run_git(dir: &Path, args: &[&str]) {
    let out = harness_core::git_env::scrub(&mut Command::new("git"))
        .args(args)
        .current_dir(dir)
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Fresh temp repo + redirected HOME so the real `~/.condukt` is never touched.
fn setup(tag: &str) -> (PathBuf, PathBuf) {
    let base = std::env::temp_dir().join(format!(
        "condukt-claim-xsession-{}-{tag}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&base);
    let home = base.join("home");
    let repo = base.join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    run_git(&repo, &["init", "-q"]);
    (repo, home)
}

/// `session = None` omits `--session` and removes the env fallback, so the
/// claim is recorded / evaluated with no session id at all.
fn claim(repo: &Path, home: &Path, run: &str, session: Option<&str>, files: &[&str]) -> Output {
    let mut binding = Command::new(bin());
    let cmd = harness_core::git_env::scrub(&mut binding);
    cmd.args(["state", "claim", "--run", run]);
    if let Some(s) = session {
        cmd.args(["--session", s]);
    }
    for f in files {
        cmd.args(["--file", f]);
    }
    cmd.current_dir(repo)
        .env("HOME", home)
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .output()
        .expect("spawn condukt")
}

fn json_of(o: &Output) -> serde_json::Value {
    serde_json::from_slice(&o.stdout).unwrap_or_else(|e| {
        panic!(
            "not ClaimOutcome JSON ({e}): stdout={:?} stderr={:?}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        )
    })
}

fn strs(v: &serde_json::Value) -> Vec<String> {
    v.as_array()
        .unwrap_or_else(|| panic!("not an array: {v}"))
        .iter()
        .map(|x| x.as_str().unwrap().to_string())
        .collect()
}

fn skipped_files(v: &serde_json::Value) -> Vec<String> {
    v["skipped"]
        .as_array()
        .unwrap_or_else(|| panic!("no `skipped` array: {v}"))
        .iter()
        .map(|s| s["file"].as_str().unwrap().to_string())
        .collect()
}

const GLOB: &str = "crates/x/src/**";
const FILE: &str = "crates/x/src/a.rs";

fn assert_skipped(v: &serde_json::Value, o: &Output, why: &str) {
    assert_eq!(skipped_files(v), vec![FILE.to_string()], "{why}: {v}");
    assert_eq!(v["claimed"], serde_json::json!([]), "nothing claimed: {v}");
    assert_eq!(o.status.code(), Some(1), "skip => exit 1: {v}");
}

fn hold_a(repo: &Path, home: &Path, session: Option<&str>) {
    let a = claim(repo, home, "runA", session, &[GLOB]);
    assert_eq!(
        a.status.code(),
        Some(0),
        "run A must claim cleanly: {}",
        json_of(&a)
    );
}

/// 1. Different session overlapping holder's glob: claimed, not skipped.
#[test]
fn different_session_overlap_is_claimed() {
    let (repo, home) = setup("diff");
    hold_a(&repo, &home, Some("S1"));
    let b = claim(&repo, &home, "runB", Some("S2"), &[FILE]);
    let v = json_of(&b);
    assert_eq!(
        skipped_files(&v),
        Vec::<String>::new(),
        "another session's file claim must not exclude (CLAUDE.md s8): {v}"
    );
    assert_eq!(strs(&v["claimed"]), vec![FILE.to_string()], "{v}");
    assert_eq!(b.status.code(), Some(0), "no skip => exit 0: {v}");
}

/// 2. Control: same session still excludes.
#[test]
fn same_session_overlap_is_skipped() {
    let (repo, home) = setup("same");
    hold_a(&repo, &home, Some("S1"));
    let b = claim(&repo, &home, "runB", Some("S1"), &[FILE]);
    let v = json_of(&b);
    assert_skipped(&v, &b, "same-session claim must still exclude");
}

/// 3. Holder session None: undeterminable => exclude.
#[test]
fn holder_session_none_is_skipped() {
    let (repo, home) = setup("holdernone");
    hold_a(&repo, &home, None);
    let b = claim(&repo, &home, "runB", Some("S2"), &[FILE]);
    let v = json_of(&b);
    assert_skipped(&v, &b, "holder without session id must exclude");
}

/// 4. Caller session None: undeterminable => exclude.
#[test]
fn caller_session_none_is_skipped() {
    let (repo, home) = setup("callernone");
    hold_a(&repo, &home, Some("S1"));
    let b = claim(&repo, &home, "runB", None, &[FILE]);
    let v = json_of(&b);
    assert_skipped(&v, &b, "caller without session id must exclude");
}

/// 5. Allowing S2 must not erase/overwrite holder A: S1's run C is still skipped.
#[test]
fn holder_survives_cross_session_claim() {
    let (repo, home) = setup("survive");
    hold_a(&repo, &home, Some("S1"));
    let b = claim(&repo, &home, "runB", Some("S2"), &[FILE]);
    assert_eq!(
        b.status.code(),
        Some(0),
        "precondition: cross-session claim allowed: {}",
        json_of(&b)
    );
    let c = claim(&repo, &home, "runC", Some("S1"), &[FILE]);
    let v = json_of(&c);
    assert_skipped(&v, &c, "holder A (S1) must remain and exclude run C");
    assert_eq!(v["skipped"][0]["holder_run"], "runA", "holder must be A: {v}");
}
