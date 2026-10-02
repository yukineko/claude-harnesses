#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! backlog fc6b48d2 (hook-message half): when no bundled binary exists, the
//! `bin/overwatch` launcher's `status` hook tells the operator to recover with
//! `scripts/build-plugin-bin.sh`. That script only stages into
//! `crates/<name>/bin/` (see its own header) and never reaches the live plugin
//! cache the hook runs from, so following the advice does not bring the hook
//! back. The working recovery observed in the item is
//! `scripts/rebuild-plugins.sh` (it seeds the cache). The advice disagrees
//! with what actually works (CLAUDE.md §4).
//!
//! The other half of the item (rollout `--canary` cannot bootstrap without an
//! overwatch binary) is in scripts/rollout-plugins.sh and is not exercised here.

use std::process::{Command, Stdio};

const BUILD_PLUGIN_BIN_SH: &str = include_str!("../../../scripts/build-plugin-bin.sh");

fn run_status_without_binary() -> String {
    let dir = tempfile::tempdir().expect("tempdir");
    let launcher = dir.path().join("overwatch");
    std::fs::copy(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("bin")
            .join("overwatch"),
        &launcher,
    )
    .expect("copy launcher");
    let out = Command::new("sh")
        .arg(&launcher)
        .arg("status")
        .stdin(Stdio::null())
        .output()
        .expect("spawn sh");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Precondition (GREEN): build-plugin-bin.sh documents that it stages into
/// the crate's own bin/ dir, not the live cache.
#[test]
fn build_plugin_bin_sh_only_stages_into_crate_bin() {
    let head: String = BUILD_PLUGIN_BIN_SH
        .lines()
        .take(6)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        head.contains("bin/"),
        "build-plugin-bin.sh header no longer says where it stages:\n{head}"
    );
}

#[test]
#[ignore = "backlog fc6b48d2: open defect, remove ignore when fixed"]
fn missing_binary_status_does_not_advise_a_script_that_cannot_restore_the_hook() {
    let stdout = run_status_without_binary();
    assert!(
        stdout.contains("did NOT run"),
        "precondition: missing-binary status banner: {stdout}"
    );
    assert!(
        !stdout.contains("build-plugin-bin.sh"),
        "the hook advises scripts/build-plugin-bin.sh, which only stages into \
         crates/<name>/bin/ and does not restore the live-cache binary the hook \
         runs: {stdout}"
    );
}
