#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro: `hypothesis list --status bogus` exits 0 with an empty listing,
//! indistinguishable from "no hypotheses have that (valid) status".

use std::process::Command;

#[test]
fn list_with_unknown_status_is_rejected() {
    let home = std::env::temp_dir().join(format!("hyp-bogus-status-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_hypothesis"))
        .args(["list", "--status", "bogus-status-zz"])
        .env("HOME", &home)
        .env_remove("HYPOTHESIS_DISABLE")
        .current_dir(&home)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&home);
    assert!(
        !out.status.success(),
        "unknown --status accepted with exit 0. stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
