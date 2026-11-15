// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Regression test (formerly the RED for 38fb077c, never filed): the library
//! entry `blastguard::detect::detect` used to run with `SafeRoots::none()`,
//! so library callers (condukt gate_exec/verify, specguard forge implement,
//! daily) got `Allow` for a clone into `$HOME`, which the hook path's
//! scratch-place rule refuses. Observed passing on main 07bf4b5f2
//! (2026-11-16, `--include-ignored`: 2 passed), so the `#[ignore]` is gone and
//! this file pins the fixed behaviour.

use blastguard::model::Decision;
use serde_json::json;

/// Exactly the call specguard's `forge::implement` makes.
fn library_verdict(command: &str) -> Decision {
    blastguard::detect::detect("Bash", Some(&json!({ "command": command }))).hardened()
}

/// REGRESSION: must not be Allow.
#[test]
fn library_entry_refuses_git_clone_into_home() {
    let d = library_verdict("git clone https://x/y ~/y");
    assert_ne!(
        d,
        Decision::Allow,
        "library entry detect(..).hardened() allowed `git clone https://x/y ~/y`; \
         the hook binary refuses the same command (observed 2026-11-08: Ask, \
         cannot tell whether the clone lands in a managed place) but library \
         callers get SafeRoots::none() and no scratch-place rule. \
         observed decision: {d:?}"
    );
}

/// CONTROL: the detector is live through the same entry (not inert).
#[test]
fn library_entry_control_refuses_rm_rf_root() {
    let d = library_verdict("rm -rf /");
    assert_ne!(
        d,
        Decision::Allow,
        "control: detect(..).hardened() must refuse `rm -rf /`; observed: {d:?}"
    );
}
