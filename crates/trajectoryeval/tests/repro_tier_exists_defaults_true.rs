#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro: `tier --exists` defaults to true when the existence input is omitted.
//!
//! `main.rs` declares `--exists` with `default_value_t = true`. For a non-core
//! flow the verdict is Pass whenever `exists` is true, so a caller that forgets
//! (or fails to compute) the existence input gets "existence: present" and
//! exit 0 for a flow nobody checked. "No input" is read as "it exists".
//!
//! Asserts the correct behaviour: for a non-core flow, omitting `--exists` must
//! not produce a passing verdict (usage/harness error or needs-human).

use std::process::Command;

#[test]
fn omitted_exists_does_not_pass_a_non_core_flow() {
    let dir = std::env::temp_dir().join(format!(
        "trajectoryeval-repro-exists-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("cfg.json"), r#"{"core":[]}"#).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_trajectoryeval"))
        .args([
            "tier",
            "--config",
            dir.join("cfg.json").to_str().unwrap(),
            "--flow",
            "never-checked-flow",
        ])
        .output()
        .unwrap();
    std::fs::remove_dir_all(&dir).ok();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_ne!(
        out.status.code(),
        Some(0),
        "no existence input was supplied but the flow passed:\n{stdout}"
    );
}
