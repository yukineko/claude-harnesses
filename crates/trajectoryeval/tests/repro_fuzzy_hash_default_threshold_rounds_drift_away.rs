#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro: `fuzzy_hash` at the default `threshold_permille` (0) passes real drift.
//!
//! `tier.rs::fuzzy_diff` computes `distance_permille` with round-half-up integer
//! division. One changed leaf out of 2001 is 1000*1/2001 = 0.4997 permille,
//! which rounds to 0, and `0 <= threshold_permille (0)` is `Match`. The doc on
//! `TierConfig::threshold_permille` says the default 0 means "no tolerance - any
//! drift escalates", so a single real change is silently absorbed by rounding
//! and the CLI exits 0 (pass).
//!
//! Asserts the correct behaviour: with zero tolerance, a snapshot that differs
//! from its baseline must not be reported as a match / exit 0.

use std::process::Command;

#[test]
#[ignore = "RED repro: fuzzy_hash default threshold 0 rounds 1/2001 drift to match"]
fn zero_tolerance_fuzzy_hash_does_not_pass_a_changed_leaf() {
    let dir =
        std::env::temp_dir().join(format!("trajectoryeval-repro-fuzzy-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();

    let base: Vec<u32> = (0..2001).collect();
    let mut snap = base.clone();
    snap[1000] = 999_999; // exactly one leaf differs
    std::fs::write(dir.join("base.json"), serde_json::to_string(&base).unwrap()).unwrap();
    std::fs::write(dir.join("snap.json"), serde_json::to_string(&snap).unwrap()).unwrap();
    std::fs::write(
        dir.join("cfg.json"),
        r#"{"core":["f"],"diff_strategy":"fuzzy_hash"}"#,
    )
    .unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_trajectoryeval"))
        .args([
            "tier",
            "--config",
            dir.join("cfg.json").to_str().unwrap(),
            "--flow",
            "f",
            "--baseline",
            dir.join("base.json").to_str().unwrap(),
            "--snapshot",
            dir.join("snap.json").to_str().unwrap(),
        ])
        .output()
        .unwrap();
    std::fs::remove_dir_all(&dir).ok();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_ne!(
        out.status.code(),
        Some(0),
        "one of 2001 leaves changed under zero tolerance but the tier check passed:\n{stdout}"
    );
}
