#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Reproduction tests backing rows of `docs/audit-reviewgate-verdict-paths.md`:
//!
//! * "rejected candidate" row (Claim C): when `state::save` / `append_jsonl`
//!   cannot write, `last_hash` is not persisted, so the next stop with the same
//!   reviewable diff does NOT reach `already-reviewed`; it takes the Block arm
//!   again (restrictive direction). Because the repeat ledger may waive an
//!   identical second block to a `systemMessage`, the Block arm is observed via
//!   the re-issued review text (`round 1/2`) and a second violation event.
//! * `install.rs` `dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))`
//!   (Claim D): with no resolvable home, `reviewgate install` writes
//!   `./.claude/settings.json` relative to cwd and reports success.
//!   `install_with_unresolvable_home_writes_to_cwd_and_succeeds` pins the CURRENT,
//!   DEFECTIVE (fail-open) behaviour; a fix should FLIP it (and un-ignore
//!   `install_with_unresolvable_home_refuses`, which asserts the correct
//!   behaviour and fails today).
//!
//! Written by a disinterested test author (CLAUDE.md §2(a)). Drives the real
//! binary with a sandboxed HOME.
//!
//! How "no home" is forced (`dirs::home_dir()` = `$HOME`, else `getpwuid_r`):
//! `HOME` is removed and the binary runs under
//! `unshare -U --map-user=54321 --map-group=54321`, a uid that has no passwd
//! entry, so the fallback fails too. If `unshare` cannot do that on this host the
//! precondition assertion FAILS loudly (it never silently skips).

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

/// Run `reviewgate install` (not dry-run) in a fresh cwd with HOME unset and an
/// unresolvable uid. Returns (cwd, exit code, stdout, stderr).
fn install_no_home() -> (PathBuf, i32, String, String) {
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let cwd = std::env::temp_dir().join(format!("rg-auditd-cwd-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&cwd);
    std::fs::create_dir_all(&cwd).unwrap();
    // Precondition: prove home really is unresolvable in this environment.
    let probe = Command::new("unshare")
        .args(["-U", "--map-user=54321", "--map-group=54321", "sh", "-c"])
        .arg("getent passwd 54321 >/dev/null 2>&1 && echo HAS_PASSWD || echo NO_PASSWD")
        .env_remove("HOME")
        .output()
        .expect("`unshare` is required to force an unresolvable home dir");
    assert_eq!(
        String::from_utf8_lossy(&probe.stdout).trim(),
        "NO_PASSWD",
        "cannot force dirs::home_dir()==None here (unshare failed or uid has a passwd entry): {probe:?}"
    );
    let out = Command::new("unshare")
        .args(["-U", "--map-user=54321", "--map-group=54321"])
        .arg(env!("CARGO_BIN_EXE_reviewgate"))
        .arg("install")
        .current_dir(&cwd)
        .env_remove("HOME")
        .output()
        .unwrap();
    (
        cwd,
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Claim D, pinning CURRENT (DEFECTIVE, fail-open) behaviour: no resolvable
/// home => install silently writes `./.claude/settings.json` under cwd and
/// reports success. A fix should FLIP this test.
#[test]
fn install_with_unresolvable_home_writes_to_cwd_and_succeeds() {
    let (cwd, code, stdout, stderr) = install_no_home();
    assert_eq!(code, 0, "stdout={stdout} stderr={stderr}");
    assert!(
        stdout.contains("Installed Stop hook"),
        "reports success: {stdout} {stderr}"
    );
    let written = cwd.join(".claude").join("settings.json");
    assert!(
        written.is_file(),
        "settings written relative to cwd (defect): {written:?}"
    );
}

/// Claim D, CORRECT behaviour (backlog repro): must refuse. FAILS today.
#[test]
#[ignore = "fails today: install falls back to ./ when home is unresolvable"]
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
