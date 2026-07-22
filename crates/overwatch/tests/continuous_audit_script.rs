// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Wiring test (2630b4c5): run the self-contained bash test that proves
//! `scripts/continuous-audit.sh` behaves (help / dry-run is side-effect free /
//! the record path ingests a finding into the review queue AND appends a round
//! to the convergence ledger) as part of `cargo test -p overwatch`, so the
//! repo's gate/CI runs it. The bash script
//! (`scripts/tests/continuous-audit.sh`) is the source of truth for the
//! assertions; this test invokes it with the freshly built binary pinned via
//! OVERWATCH_BIN and forwards its verdict.
//!
//! Fail-soft: if `bash` isn't available the test skips rather than failing —
//! the shell contract only applies where the shell runs.

use std::path::PathBuf;
use std::process::Command;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("repo root")
        .to_path_buf()
}

#[test]
fn continuous_audit_script_behaves() {
    if Command::new("bash").arg("--version").output().is_err() {
        eprintln!("skipping: bash not available");
        return;
    }
    let repo = repo_root();
    let script = repo.join("scripts/tests/continuous-audit.sh");
    assert!(script.exists(), "missing test script: {}", script.display());

    // Pin the script at THIS crate's freshly built binary (OVERWATCH_BIN) so it
    // tests the code under test, not a stale install, and doesn't rebuild.
    let ow = env!("CARGO_BIN_EXE_overwatch");

    let out = Command::new("bash")
        .arg(&script)
        .env("OVERWATCH_BIN", ow)
        .current_dir(&repo)
        .output()
        .expect("run continuous-audit.sh");

    if !out.status.success() {
        panic!(
            "continuous-audit script test failed (rc={:?})\n--- stdout ---\n{}\n--- stderr ---\n{}",
            out.status.code(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
    }
}
