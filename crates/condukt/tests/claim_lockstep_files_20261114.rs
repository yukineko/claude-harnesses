#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Version-lockstep files must not serialize cross-session file claims.
//!
//! Observed 2026-11-14: two runs in different sessions both bumped a plugin
//! version; the second run's `state set --status running` was skipped with
//! "4 file(s) already claimed by a live run" over `crates/<c>/Cargo.toml`,
//! `crates/<c>/.claude-plugin/plugin.json`, `.claude-plugin/marketplace.json`
//! and `Cargo.lock`. Those edits are line-disjoint, git merges them, and
//! `scripts/check-plugin-versions.py` guards correctness, so serializing on
//! them is a false positive of the claim gate.
//!
//! Liveness of run A's claim: `claim_files` stamps `heartbeat_at = now`, and a
//! claim is only reapable once its heartbeat is older than the stuck TTL, so
//! run B's claim a moment later sees A as live regardless of A's (exited) pid.
//!
//! Drives the real binary: `state claim` prints a ClaimOutcome JSON and exits 1
//! when any file is skipped.

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
        "condukt-claim-lockstep-{}-{tag}",
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

fn claim(repo: &Path, home: &Path, run: &str, session: &str, files: &[&str]) -> Output {
    let mut binding = Command::new(bin());
    let cmd = harness_core::git_env::scrub(&mut binding);
    cmd.args(["state", "claim", "--run", run, "--session", session]);
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

/// `skipped` is an array of `{file, holder_run, ..}` objects.
fn skipped_files(v: &serde_json::Value) -> Vec<String> {
    v["skipped"]
        .as_array()
        .unwrap_or_else(|| panic!("no `skipped` array: {v}"))
        .iter()
        .map(|s| s["file"].as_str().unwrap().to_string())
        .collect()
}

const LOCKSTEP: [&str; 4] = [
    ".claude-plugin/marketplace.json",
    "Cargo.lock",
    "crates/blastguard/.claude-plugin/plugin.json",
    "crates/blastguard/Cargo.toml",
];

#[test]
fn lockstep_files_never_conflict_across_runs() {
    let (repo, home) = setup("lockstep");
    let a = claim(&repo, &home, "runA", "sess-A", &LOCKSTEP);
    assert_eq!(
        a.status.code(),
        Some(0),
        "run A must claim cleanly: {}",
        json_of(&a)
    );

    let b = claim(&repo, &home, "runB", "sess-B", &LOCKSTEP);
    let v = json_of(&b);
    assert_eq!(
        skipped_files(&v),
        Vec::<String>::new(),
        "version-lockstep files must not be skipped by another live run's claim \
         (line-disjoint edits, guarded by check-plugin-versions.py). Got: {v}"
    );
    let mut claimed = strs(&v["claimed"]);
    claimed.sort();
    let mut want: Vec<String> = LOCKSTEP.iter().map(|s| s.to_string()).collect();
    want.sort();
    assert_eq!(
        claimed, want,
        "all lockstep files must be in `claimed`: {v}"
    );
    assert_eq!(b.status.code(), Some(0), "no skip => exit 0: {v}");
}

/// Control: ordinary source files held by another live run still conflict.
#[test]
fn ordinary_source_file_still_conflicts() {
    let (repo, home) = setup("control");
    let f = "crates/blastguard/src/detect/payload.rs";
    let a = claim(&repo, &home, "runA", "sess-A", &[f]);
    assert_eq!(
        a.status.code(),
        Some(0),
        "run A must claim cleanly: {}",
        json_of(&a)
    );

    let b = claim(&repo, &home, "runB", "sess-B", &[f]);
    let v = json_of(&b);
    assert_eq!(
        skipped_files(&v),
        vec![f.to_string()],
        "source file held by a live run must be skipped: {v}"
    );
    assert_eq!(v["claimed"], serde_json::json!([]), "nothing claimed: {v}");
    assert_eq!(b.status.code(), Some(1), "skip => exit 1: {v}");
}
