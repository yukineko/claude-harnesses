#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Reproduction tests backing rows of the former reviewgate verdict-path audit
//! table (the audit doc was deleted on the operator's instruction; each row
//! the tests back is restated below):
//!
//! * Row B2: `emit_violation` is reached only from the block arms (plus the
//!   give-up allow and config-invalid), so an ordinary ALLOW leaves no event in
//!   the overwatch violation store while a BLOCK records one.
//! * Audit row at `state::load(&cfg.state_dir, &session)`: a corrupt per-session
//!   state file is read as the default (attempts 0, empty `last_hash`), i.e.
//!   "not yet reviewed", so a reviewable change BLOCKS instead of being waved
//!   through as `already-reviewed`.
//!
//! Written by a disinterested test author (CLAUDE.md §2(a)). Drives the real
//! binary with a sandboxed HOME; every store path is asserted to live under it.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

fn project(tag: &str, with_change: bool) -> PathBuf {
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("rg-auditb2-{tag}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for args in [
        vec!["init", "-q"],
        vec!["config", "user.email", "t@t"],
        vec!["config", "user.name", "t"],
    ] {
        let _ = harness_core::git_env::scrub(&mut Command::new("git"))
            .current_dir(&dir)
            .args(&args)
            .output();
    }
    std::fs::write(dir.join("seed.rs"), "fn a() {}\n").unwrap();
    for args in [vec!["add", "-A"], vec!["commit", "-qm", "seed"]] {
        let _ = harness_core::git_env::scrub(&mut Command::new("git"))
            .current_dir(&dir)
            .args(&args)
            .output();
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

const SESSION: &str = "s-audit-b2";

/// HOME is the project dir itself (sandbox).
fn stop(dir: &Path) -> (i32, String, String) {
    ensure_inject_config(dir);
    let payload = serde_json::json!({
        "session_id": SESSION,
        "cwd": dir.to_string_lossy(),
        "hook_event_name": "Stop",
    })
    .to_string();
    let mut child =
        harness_core::git_env::scrub(&mut Command::new(env!("CARGO_BIN_EXE_reviewgate")))
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

fn is_block(stdout: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(stdout.trim())
        .map(|v| v["decision"] == "block")
        .unwrap_or(false)
}

/// All `violations.jsonl` files under `<home>/.overwatch`, asserted to be
/// under the sandbox HOME (never the live registry).
fn violation_files(home: &Path) -> Vec<PathBuf> {
    fn walk(d: &Path, out: &mut Vec<PathBuf>) {
        let Ok(rd) = std::fs::read_dir(d) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.file_name().is_some_and(|n| n == "violations.jsonl") {
                out.push(p);
            }
        }
    }
    let mut out = Vec::new();
    walk(&home.join(".overwatch"), &mut out);
    for p in &out {
        assert!(
            p.starts_with(home),
            "violation store {p:?} escaped {home:?}"
        );
    }
    out
}

fn violation_lines(home: &Path) -> usize {
    violation_files(home)
        .iter()
        .map(|p| std::fs::read_to_string(p).unwrap().lines().count())
        .sum()
}

fn state_file(home: &Path) -> PathBuf {
    home.join(".reviewgate")
        .join("state")
        .join("sessions")
        .join(format!("{SESSION}.json"))
}

/// Row B2: allow records no violation; block records one.
#[test]
fn allow_records_no_violation_but_block_records_one() {
    // ALLOW: clean repo.
    let clean = project("b2-allow", false);
    let (code, stdout, stderr) = stop(&clean);
    assert_eq!(code, 0, "stderr={stderr}");
    assert!(!is_block(&stdout), "clean repo must allow: {stdout}");
    assert_eq!(
        violation_lines(&clean),
        0,
        "an allow must leave no violation event (row B2)"
    );

    // BLOCK: reviewable change.
    let dirty = project("b2-block", true);
    assert_eq!(violation_lines(&dirty), 0, "precondition: store empty");
    let (code, stdout, stderr) = stop(&dirty);
    assert_eq!(code, 0, "stderr={stderr}");
    assert!(is_block(&stdout), "reviewable change must block: {stdout}");
    assert_eq!(
        violation_lines(&dirty),
        1,
        "a block must record exactly one violation event; stdout={stdout} stderr={stderr}"
    );
    assert!(
        !violation_files(&dirty).is_empty(),
        "store file must exist under the sandbox HOME"
    );
}

/// Audit row at `state::load`: corrupt state file => "not yet reviewed" => block.
/// Control: the uncorrupted identical repeat is `already-reviewed` (allow).
#[test]
fn corrupt_state_file_reads_as_unreviewed_and_blocks() {
    // Control: first stop blocks and records state; identical repeat allows.
    let ctl = project("state-ctl", true);
    let (_, out1, err1) = stop(&ctl);
    assert!(is_block(&out1), "first stop must block: {out1} {err1}");
    let sf = state_file(&ctl);
    assert!(sf.starts_with(&ctl), "state file must be sandboxed");
    assert!(sf.exists(), "state file must exist after a block: {sf:?}");
    let (code, out2, err2) = stop(&ctl);
    assert_eq!(code, 0);
    assert!(
        !is_block(&out2) && !out2.contains("round "),
        "control: uncorrupted identical repeat must be already-reviewed (allow, no review reason): {out2} {err2}"
    );
    assert_eq!(
        violation_lines(&ctl),
        1,
        "control: the already-reviewed allow must record no further violation"
    );

    // Test: same sequence, but corrupt the state file between the stops.
    let dir = project("state-corrupt", true);
    let (_, out1, err1) = stop(&dir);
    assert!(is_block(&out1), "first stop must block: {out1} {err1}");
    let sf = state_file(&dir);
    assert!(sf.exists(), "state file must exist: {sf:?}");
    std::fs::write(&sf, "\u{0}\u{1}{{{ not json ]]]").unwrap();
    let (code, out2, err2) = stop(&dir);
    assert_eq!(code, 0, "stderr={err2}");
    // The repeat ledger (operator ruling 2026-09-18) turns an identical
    // SECOND block into a one-line `systemMessage` waiver rather than a
    // `decision:block` line; since the 2026-10-27 ruling that one-liner no
    // longer carries the reason, which is recorded in the session's
    // `waivers.log` instead. So the gate's Block decision is observed via
    // (a) the review reason (round 1/2: attempts were reset to 0, i.e. the
    // corrupt state read as "not yet reviewed") being re-issued — on stdout
    // if it blocked, or in the waiver log if the ledger waived it — and (b) a
    // second violation event, which only the Block arm emits.
    let waivers = std::fs::read_to_string(
        dir.join(".claude/harness-state/repeat")
            .join(SESSION)
            .join("waivers.log"),
    )
    .unwrap_or_default();
    assert!(
        out2.contains("round 1/2")
            || (out2.contains("同じ指摘の2度目のため通しました") && waivers.contains("round 1/2")),
        "corrupt state must read as unreviewed and re-issue the review, not allow silently: \
         stdout={out2} stderr={err2} waivers.log={waivers:?}"
    );
    assert_eq!(
        violation_lines(&dir),
        2,
        "the second stop must have taken the Block arm (violation event recorded): {out2}"
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
