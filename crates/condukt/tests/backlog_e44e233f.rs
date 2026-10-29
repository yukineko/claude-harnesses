#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! backlog e44e233f — `condukt shadow-run finish` declares `--cost` and
//! `--duration` as REQUIRED `f64`s, so a caller that measured neither must
//! invent values (the fabricated-measurement class 55fb3861 removed elsewhere).
//! `fugu-router record`, the only consumer, already takes both as optional.
//! Omitting them must not be rejected at argument parsing.

use std::process::Command;

#[test]
fn shadow_run_finish_does_not_require_unmeasured_cost_and_duration() {
    let home = tempfile::tempdir().unwrap();
    let out = harness_core::git_env::scrub(&mut Command::new(env!("CARGO_BIN_EXE_condukt")))
        .args([
            "shadow-run",
            "finish",
            "--path",
            "/nonexistent-e44e233f",
            "--branch",
            "b",
            "--title",
            "t",
            "--model",
            "m",
            "--pass",
        ])
        .current_dir(home.path())
        .env("HOME", home.path())
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    let rejected_for_missing_measurement = err.contains("required arguments were not provided")
        && (err.contains("--cost") || err.contains("--duration"));
    assert!(
        !rejected_for_missing_measurement,
        "a caller with no cost/duration measurement is forced to fabricate one: {err}"
    );
}
