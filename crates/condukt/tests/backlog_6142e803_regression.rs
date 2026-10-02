#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Closure regression for backlog 6142e803.
//!
//! 6142e803: `fp_gate_scope_contract::absent_decomposition_file_still_accepts_a_fix_task_with_no_proofs`
//! and `fp_oracle_e2e::genuinely_absent_decomposition_still_verifies` were RED
//! on main. Root cause (22fa75af): both asserted exit 0, but since 306cf008 /
//! ruling 9a4fb884 `state set` exits non-zero AFTER the durable write when the
//! terminal claim release is Undetermined (no decomposition => files unknown).
//! The property both tests name -- "an absent decomposition must still allow
//! verification" -- is observed here on DURABLE state (`state show`), not on
//! the exit code, plus the ruled exit contract.
//!
//! Control: with the decomposition PRESENT, the same kind:"fix" task with no
//! F->P proofs is refused, so the absent-case acceptance is not a gate that
//! accepts everything.
//!
//! Written by an independent closure verifier, not the implementer.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

fn unique_dir(tag: &str) -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "condukt-6142e803-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn condukt(dir: &Path, home: &Path, args: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_condukt"))
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .stdin(Stdio::null())
        .output()
        .expect("condukt spawns");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn find(dir: &Path, name: &str) -> Option<PathBuf> {
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let p = e.path();
        if p.is_dir() {
            if let Some(f) = find(&p, name) {
                return Some(f);
            }
        } else if p.file_name().and_then(|s| s.to_str()) == Some(name) {
            return Some(p);
        }
    }
    None
}

/// Init a run holding one kind:"fix" task with no proofs.
fn seed(tag: &str, run: &str) -> (PathBuf, PathBuf) {
    let dir = unique_dir(tag);
    let home = unique_dir(&format!("{tag}-home"));
    let dec = dir.join("decomposition.json");
    let body = serde_json::json!({
        "goal": "g",
        "tasks": [{"id": "task1", "title": "t", "kind": "fix",
                   "reproduction_tests": "cargo test -p demo"}]
    });
    std::fs::write(&dec, body.to_string()).unwrap();
    let (code, out, err) = condukt(
        &dir,
        &home,
        &[
            "state",
            "init",
            "--run",
            run,
            "--file",
            dec.to_str().unwrap(),
        ],
    );
    assert_eq!(code, 0, "state init: {out} {err}");
    (dir, home)
}

fn status(dir: &Path, home: &Path, run: &str) -> String {
    let (code, out, err) = condukt(dir, home, &["state", "show", "--run", run]);
    assert_eq!(code, 0, "state show: {out} {err}");
    let v: serde_json::Value = serde_json::from_str(&out).expect("state show JSON");
    v["tasks"]
        .as_array()
        .expect("tasks")
        .iter()
        .find(|t| t["id"] == "task1")
        .expect("task1")["status"]
        .as_str()
        .expect("status string")
        .to_string()
}

fn set_verified(dir: &Path, home: &Path, run: &str) -> (i32, String) {
    let (code, _out, err) = condukt(
        dir,
        home,
        &[
            "state", "set", "--run", run, "--task", "task1", "--status", "verified",
        ],
    );
    (code, err)
}

#[test]
fn absent_decomposition_still_reaches_verified_on_durable_state() {
    let (dir, home) = seed("absent", "run-6142-absent");
    let persisted = find(&home, "run-6142-absent.decomposition.json")
        .expect("state init persisted a decomposition");
    std::fs::remove_file(&persisted).unwrap();

    let (code, err) = set_verified(&dir, &home, "run-6142-absent");
    assert!(
        !err.contains("refusing to verify"),
        "F->P gate refused: {err}"
    );
    assert_eq!(
        status(&dir, &home, "run-6142-absent"),
        "verified",
        "absent decomposition must still allow verification; stderr: {err}"
    );
    // Ruled contract (9a4fb884): the claim release after the durable write is
    // Undetermined, so the exit is non-zero with the named diagnostic.
    assert_ne!(code, 0, "stderr: {err}");
    assert!(err.contains("claim release NOT PERFORMED"), "stderr: {err}");
}

#[test]
fn control_present_decomposition_refuses_a_fix_task_without_proofs() {
    let (dir, home) = seed("present", "run-6142-present");
    let (code, err) = set_verified(&dir, &home, "run-6142-present");
    assert_ne!(code, 0, "stderr: {err}");
    assert_ne!(
        status(&dir, &home, "run-6142-present"),
        "verified",
        "a fix task with no F->P proofs must not be verified; stderr: {err}"
    );
}
