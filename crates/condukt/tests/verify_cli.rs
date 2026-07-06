//! End-to-end coverage for the deterministic `condukt verify` verdict
//! subcommands (`regressions`, `confidence`). These wire the pure set-diff /
//! confidence functions in `verify.rs` into the real CLI the verifier agent
//! shells out to, so this test covers the WIRING (arg parsing, file reads,
//! JSON/token output, exit code) — not just the pure functions' unit tests.
//! The verifier consults `verify regressions` as the authoritative
//! baseline-exclusion decision, so a regression here is a verifier regression.

use std::path::{Path, PathBuf};
use std::process::Command;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_condukt")
}

/// A throwaway temp dir for the baseline/current input files.
fn scratch(tag: &str) -> PathBuf {
    let pid = std::process::id();
    let mut base = std::env::temp_dir();
    base.push(format!("condukt-verify-cli-{pid}-{tag}"));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    base
}

fn write(dir: &Path, name: &str, body: &str) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, body).unwrap();
    p
}

fn run_regressions(baseline: &Path, current: &Path) -> (String, i32) {
    let out = Command::new(bin())
        .args([
            "verify",
            "regressions",
            "--baseline",
            baseline.to_str().unwrap(),
            "--current",
            current.to_str().unwrap(),
        ])
        .output()
        .expect("spawn condukt verify regressions");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        out.status.code().unwrap_or(-1),
    )
}

/// A new failure in `current` that was not failing at baseline -> the CLI must
/// report `passed:false` and name the regression.
#[test]
fn verify_regressions_cli_flags_new_failure() {
    let dir = scratch("newfail");
    // Newline-list form for the baseline, cargo-output form for current — both
    // must be understood by the same extraction path.
    let baseline = write(&dir, "baseline.txt", "foo::a\n");
    let current = write(
        &dir,
        "current.txt",
        "test foo::a ... FAILED\ntest foo::b ... FAILED\n",
    );
    let (stdout, code) = run_regressions(&baseline, &current);
    assert_eq!(code, 0, "verify regressions must exit 0; stdout={stdout}");
    let v: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    assert_eq!(
        v["passed"], false,
        "a new current failure must make passed=false: {stdout}"
    );
    let regs = v["regressions"].as_array().expect("regressions array");
    assert_eq!(
        regs.iter().map(|r| r.as_str().unwrap()).collect::<Vec<_>>(),
        vec!["foo::b"],
        "the new failure foo::b must be the sole regression: {stdout}"
    );
}

/// No new failures relative to baseline (a pre-existing baseline red persists,
/// and one clears) -> `passed:true`, empty regressions.
#[test]
fn verify_regressions_cli_passes_when_no_new_failure() {
    let dir = scratch("nonew");
    let baseline = write(&dir, "baseline.txt", "foo::a\nfoo::b\n");
    // current still red on a (pre-existing), b cleared — neither is a regression.
    let current = write(&dir, "current.txt", "test foo::a ... FAILED\n");
    let (stdout, code) = run_regressions(&baseline, &current);
    assert_eq!(code, 0, "verify regressions must exit 0; stdout={stdout}");
    let v: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    assert_eq!(
        v["passed"], true,
        "no new failure must make passed=true: {stdout}"
    );
    assert!(
        v["regressions"].as_array().unwrap().is_empty(),
        "regressions must be empty: {stdout}"
    );
}

/// The `confidence` subcommand derives the token from observed facts.
#[test]
fn verify_confidence_cli_derives_token() {
    // ran + exit0 + no regressions -> high.
    let high = Command::new(bin())
        .args([
            "verify",
            "confidence",
            "--check-executed",
            "--exit-zero",
            "--no-regressions",
        ])
        .output()
        .expect("spawn confidence high");
    assert_eq!(high.status.code().unwrap_or(-1), 0);
    assert_eq!(String::from_utf8_lossy(&high.stdout).trim(), "high");

    // no check executed -> low.
    let low = Command::new(bin())
        .args(["verify", "confidence"])
        .output()
        .expect("spawn confidence low");
    assert_eq!(low.status.code().unwrap_or(-1), 0);
    assert_eq!(String::from_utf8_lossy(&low.stdout).trim(), "low");

    // ran but regressed -> medium.
    let med = Command::new(bin())
        .args(["verify", "confidence", "--check-executed", "--exit-zero"])
        .output()
        .expect("spawn confidence medium");
    assert_eq!(med.status.code().unwrap_or(-1), 0);
    assert_eq!(String::from_utf8_lossy(&med.stdout).trim(), "medium");
}
