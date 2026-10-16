#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro: an empty spec in subsequence mode passes any trajectory.
//!
//! `{"mode":"subsequence"}` deserialises with `steps = []` (`#[serde(default)]`).
//! The subsequence matcher requires nothing and reports nothing unexpected, so
//! any actual trajectory - including one with arbitrary tools - gets
//! "trajectory matched (pass)" and exit 0. A spec that constrains nothing is
//! indistinguishable from a spec that was satisfied.
//!
//! Asserts the correct behaviour: a spec with no steps must not pass (it is a
//! harness error or a failure), not exit 0.

use std::process::Command;

#[test]
#[ignore = "RED repro: empty subsequence spec passes any trajectory"]
fn empty_subsequence_spec_does_not_exit_zero() {
    let dir =
        std::env::temp_dir().join(format!("trajectoryeval-repro-empty-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("expected.json"), r#"{"mode":"subsequence"}"#).unwrap();
    std::fs::write(dir.join("actual.json"), r#"["Bash","WebFetch","Write"]"#).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_trajectoryeval"))
        .args([
            "check",
            "--expected",
            dir.join("expected.json").to_str().unwrap(),
            "--actual",
            dir.join("actual.json").to_str().unwrap(),
        ])
        .output()
        .unwrap();
    std::fs::remove_dir_all(&dir).ok();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_ne!(
        out.status.code(),
        Some(0),
        "empty spec constrains nothing yet the check passed:\n{stdout}"
    );
}
