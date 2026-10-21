#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Reproduction tests backing rows of the former reviewgate verdict-path audit
//! table (the audit doc was deleted on the operator's instruction; each row
//! the tests back is restated below):
//!
//! * "rejected candidate" row (Claim C): when `state::save` / `append_jsonl`
//!   cannot write, `last_hash` is not persisted, so the next stop with the same
//!   reviewable diff does NOT reach `already-reviewed`; it takes the Block arm
//!   again (restrictive direction). Because the repeat ledger may waive an
//!   identical second block to a `systemMessage`, the Block arm is observed via
//!   the re-issued review text (`round 1/2`) and a second violation event.
//! * `install.rs` used `dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))`
//!   (Claim D): with no resolvable home, `reviewgate install` wrote
//!   `./.claude/settings.json` relative to cwd and reported success (backlog
//!   12657c5e). Fixed: `install_with_unresolvable_home_refuses` (and its
//!   dry-run twin) assert the refusal. The former
//!   `install_with_unresolvable_home_writes_to_cwd_and_succeeds`, which pinned
//!   the defective behaviour and said a fix should flip it, was removed in the
//!   fixing commit (contract change).
//!
//! Written by a disinterested test author (CLAUDE.md §2(a)). Drives the real
//! binary with a sandboxed HOME.
//!
//! How "no home" is forced, portably (backlog bc6508e7): `install` resolves
//! home with `harness_core::config::home()`, which on unix reads ONLY `$HOME`
//! (it deliberately never guesses the passwd home, see that function's doc) and
//! consults the account home only when `HARNESS_SANDBOX_HOME` is declared. So
//! removing both variables makes the home undeterminable on any unix host;
//! the former Linux-only `unshare -U` (a uid with no passwd entry) is no longer
//! needed.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

fn project(tag: &str, with_change: bool) -> PathBuf {
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("rg-auditcd-{tag}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for args in [
        vec!["init", "-q"],
        vec!["config", "user.email", "t@t"],
        vec!["config", "user.name", "t"],
    ] {
        let _ = Command::new("git").current_dir(&dir).args(&args).output();
    }
    std::fs::write(dir.join("seed.rs"), "fn a() {}\n").unwrap();
    for args in [vec!["add", "-A"], vec!["commit", "-qm", "seed"]] {
        let _ = Command::new("git").current_dir(&dir).args(&args).output();
    }
    if with_change {
        std::fs::write(
            dir.join("changed.rs"),
            "fn f() {\n    let x = 1;\n    println!(\"{}\", x);\n}\n",
        )
        .unwrap();
    }
    dir
}

const SESSION: &str = "s-audit-cd";

fn stop(dir: &Path) -> (i32, String, String) {
    ensure_inject_config(dir);
    let payload = serde_json::json!({
        "session_id": SESSION,
        "cwd": dir.to_string_lossy(),
        "hook_event_name": "Stop",
    })
    .to_string();
    let mut child = Command::new(env!("CARGO_BIN_EXE_reviewgate"))
        .arg("review")
        .current_dir(dir)
        .env("HOME", dir)
        .env("CLAUDE_CODE_SESSION_ID", SESSION)
        .env_remove("REVIEWGATE_DISABLE")
        .env_remove("HARNESS_TRUST_ALL")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn violation_lines(home: &Path) -> usize {
    fn walk(d: &Path, out: &mut usize, home: &Path) {
        let Ok(rd) = std::fs::read_dir(d) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out, home);
            } else if p.file_name().is_some_and(|n| n == "violations.jsonl") {
                assert!(
                    p.starts_with(home),
                    "violation store {p:?} escaped {home:?}"
                );
                *out += std::fs::read_to_string(&p).unwrap().lines().count();
            }
        }
    }
    let mut n = 0;
    walk(&home.join(".overwatch"), &mut n, home);
    n
}

/// Claim C control: writable state dir => identical repeat is a silent
/// `already-reviewed` allow (no review text, no further violation event).
#[test]
fn writable_state_repeat_is_already_reviewed() {
    let dir = project("c-ctl", true);
    let (_, out1, err1) = stop(&dir);
    assert!(
        out1.contains("round 1/2"),
        "first stop must block: {out1} {err1}"
    );
    assert!(
        dir.join(".reviewgate/state/sessions").exists(),
        "state must be persisted"
    );
    let (code, out2, err2) = stop(&dir);
    assert_eq!(code, 0);
    assert!(
        !out2.contains("round "),
        "control: repeat must be a silent already-reviewed allow: {out2} {err2}"
    );
    assert_eq!(violation_lines(&dir), 1);
}

/// Claim C: unwritable state dir (a regular file where the dir must be) =>
/// last_hash is never persisted => the identical repeat re-takes the Block arm.
#[test]
fn unwritable_state_dir_repeat_blocks_again() {
    let dir = project("c-unwritable", true);
    std::fs::create_dir_all(dir.join(".reviewgate")).unwrap();
    // `.reviewgate/state` as a regular file: create_dir_all/write beneath it fail.
    let state = dir.join(".reviewgate/state");
    std::fs::write(&state, "not a directory").unwrap();

    let (_, out1, err1) = stop(&dir);
    assert!(
        out1.contains("round 1/2"),
        "first stop must block: {out1} {err1}"
    );
    assert!(state.is_file(), "precondition: state path still a file");

    let (code, out2, err2) = stop(&dir);
    assert_eq!(code, 0, "stderr={err2}");
    assert!(
        out2.contains("round 1/2"),
        "unpersisted state must NOT reach already-reviewed; review must be re-issued: {out2} {err2}"
    );
    assert_eq!(
        violation_lines(&dir),
        2,
        "second stop must have taken the Block arm again"
    );
}

/// Run `reviewgate <args>` in a fresh cwd with `HOME` and
/// `HARNESS_SANDBOX_HOME` unset, i.e. an undeterminable home. Returns (cwd,
/// exit code, stdout, stderr).
fn run_no_home(args: &[&str]) -> (PathBuf, i32, String, String) {
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let cwd = std::env::temp_dir().join(format!("rg-auditd-cwd-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&cwd);
    std::fs::create_dir_all(&cwd).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_reviewgate"))
        .args(args)
        .current_dir(&cwd)
        .env_remove("HOME")
        .env_remove("HARNESS_SANDBOX_HOME")
        .output()
        .unwrap();
    (
        cwd,
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Run `reviewgate install` (not dry-run) with an undeterminable home.
fn install_no_home() -> (PathBuf, i32, String, String) {
    run_no_home(&["install"])
}

/// Claim D, dry-run twin: with an undeterminable home, `install --dry-run`
/// must refuse too rather than preview a merge into a settings file whose
/// location it does not know.
#[test]
fn install_dry_run_with_unresolvable_home_refuses() {
    let (cwd, code, stdout, stderr) = run_no_home(&["install", "--dry-run"]);
    assert_ne!(
        code, 0,
        "must exit non-zero; stdout={stdout} stderr={stderr}"
    );
    assert!(
        !cwd.join(".claude").exists(),
        "nothing may be written under cwd"
    );
}

/// Claim D, CORRECT behaviour (backlog 12657c5e repro): must refuse.
#[test]
fn install_with_unresolvable_home_refuses() {
    let (cwd, code, stdout, stderr) = install_no_home();
    assert_ne!(
        code, 0,
        "must exit non-zero; stdout={stdout} stderr={stderr}"
    );
    assert!(
        !stdout.contains("Installed Stop hook"),
        "must not report success: {stdout}"
    );
    assert!(
        !cwd.join(".claude").exists(),
        "nothing may be written under cwd"
    );
}

/// reviewgate's built-in default is `mode = "off"` (no review). These tests
/// exercise the reviewing path, so they opt in explicitly with a home config
/// `mode = "inject"` -- written only when the test has not placed its own.
fn ensure_inject_config(home: &Path) {
    let cfg = home.join(".reviewgate").join("config.toml");
    if !cfg.exists() {
        std::fs::create_dir_all(home.join(".reviewgate")).unwrap();
        std::fs::write(cfg, "mode = \"inject\"\n").unwrap();
    }
}
