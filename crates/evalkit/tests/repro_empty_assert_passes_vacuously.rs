#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro: a case with an empty `assert` passes vacuously.
//!
//! `case.rs` documents "an empty `Assert` asserts only that the subject could be
//! acquired", `validate` does not reject it, and `run.rs::check_assert` loops
//! over four empty vectors, so zero assertions evaluated is reported as `ok`
//! and the run exits 0. A golden case that asserts nothing is a no-op that
//! reads as a passing regression check.
//!
//! Asserts the correct behaviour: a non-draft case that carries no assertion at
//! all must be rejected (non-zero), not counted as passed.

use std::process::Command;

#[test]
#[ignore = "RED repro: empty assert passes vacuously"]
fn case_with_no_assertions_is_not_a_pass() {
    let dir = std::env::temp_dir().join(format!("evalkit-repro-empty-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("subject.txt"), "anything").unwrap();
    std::fs::write(
        dir.join("g.jsonl"),
        "{\"id\":\"vacuous\",\"file\":\"subject.txt\",\"assert\":{}}\n",
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_evalkit"))
        .args([
            "run",
            "--root",
            dir.to_str().unwrap(),
            "--dir",
            dir.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    std::fs::remove_dir_all(&dir).ok();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_ne!(
        out.status.code(),
        Some(0),
        "case with zero assertions was reported as passed:\n{stdout}"
    );
}
