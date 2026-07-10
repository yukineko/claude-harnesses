//! Problem-2.3 wiring: run the self-contained bash test that proves
//! `scripts/rollout-plugins.sh` requires `--canary` for GATE crates (and that
//! `--no-canary` overrides while non-gate crates stay unaffected) as part of
//! `cargo test -p overwatch`, so the repo's gate/CI runs it. The bash script
//! (`scripts/tests/canary-gate-crates.sh`) is the source of truth for the
//! assertions; this test just invokes it and forwards its verdict.
//!
//! Fail-soft: if `bash` isn't available (e.g. a non-unix CI leg) the test skips
//! rather than failing — the shell contract only applies where the shell runs.

use std::path::PathBuf;
use std::process::Command;

fn repo_root() -> PathBuf {
    // CARGO_MANIFEST_DIR = <repo>/crates/overwatch → go up two levels.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("repo root")
        .to_path_buf()
}

#[test]
fn rollout_requires_canary_for_gate_crates() {
    if Command::new("bash").arg("--version").output().is_err() {
        eprintln!("skipping: bash not available");
        return;
    }
    let repo = repo_root();
    let script = repo.join("scripts/tests/canary-gate-crates.sh");
    assert!(script.exists(), "missing test script: {}", script.display());

    // Point the script at THIS crate's freshly built binary so it doesn't have
    // to rebuild (and so it tests the code under test, not a stale install).
    let ow = env!("CARGO_BIN_EXE_overwatch");

    let out = Command::new("bash")
        .arg(&script)
        .env("OVERWATCH_BIN", ow)
        .current_dir(&repo)
        .output()
        .expect("run canary-gate-crates.sh");

    if !out.status.success() {
        panic!(
            "gate-crate rollout test failed (rc={:?})\n--- stdout ---\n{}\n--- stderr ---\n{}",
            out.status.code(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
    }
}
