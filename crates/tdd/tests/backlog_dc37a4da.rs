// Integration test: unwrap/expect/panic are fine here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Backlog dc37a4da — RED repro.
//!
//! `tdd oracle` accepts a RED proof recorded with one test command and a GREEN
//! proof recorded with a DIFFERENT test command as a valid `fail_to_pass`.
//! `crates/tdd/src/transition.rs` documents the oracle as "the test failed
//! before the implementation and passes after it" — the same test. A RED of
//! command A followed by a GREEN of an unrelated command B proves no such
//! thing, yet the oracle exits 0 with `valid_fp_oracle:true`.
//!
//! Drives the real built binary: `tdd red` -> `tdd green` -> `tdd oracle`.

use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

fn unique_dir() -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let id = N.fetch_add(1, Ordering::Relaxed);
    let d = std::env::temp_dir().join(format!("tdd-dc37a4da-{}-{}", std::process::id(), id));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn tdd(dir: &PathBuf, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_tdd"))
        .args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .stdin(Stdio::null())
        .output()
        .expect("tdd binary runs")
}

#[test]
fn oracle_rejects_red_and_green_recorded_with_different_test_commands() {
    let dir = unique_dir();

    let red = tdd(
        &dir,
        &[
            "red",
            "--task",
            "t",
            "--cmd",
            "python3 -c 'raise SystemExit(1)'",
        ],
    );
    assert!(
        red.status.success(),
        "precondition: tdd red must record a RED proof: {red:?}"
    );

    let green = tdd(
        &dir,
        &[
            "green",
            "--task",
            "t",
            "--cmd",
            "python3 -c 'raise SystemExit(0)'",
        ],
    );

    // Either green refuses a command that differs from the RED's, or the
    // oracle refuses the mismatched pair. Both are acceptable fixes; accepting
    // the pair as a valid F->P oracle is the defect.
    if !green.status.success() {
        return;
    }
    let oracle = tdd(&dir, &["oracle", "--task", "t"]);
    let stdout = String::from_utf8_lossy(&oracle.stdout);
    assert_ne!(
        oracle.status.code(),
        Some(0),
        "tdd oracle accepted a RED of one command and a GREEN of a different \
         command as a valid Fail->Pass oracle: {stdout}"
    );
    assert!(
        !stdout.contains("\"valid_fp_oracle\":true"),
        "valid_fp_oracle must not be true for mismatched commands: {stdout}"
    );
}
