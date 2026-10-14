//! Independent verifier tests for backlog f7a0cadc / 4fdf13ad (condukt 0.7.221).
//!
//! A `#[cfg(test)]` unit-test module rather than `tests/verifier_f7a0cadc.rs`:
//! `condukt` is a bin crate, and the fix has no binary-observable effect — the
//! `condukt editgate` hook printed byte-identical output at base 5f176b81 and at
//! 4f0dace5 for a clean crate, a compile error, a rustup toolchain failure and
//! cargo missing from PATH (measured by the verifier). The defect only exists
//! when a SIBLING THREAD in the same process swaps the toolchain env while
//! `editgate::check_edit` spawns cargo, which only an in-process test can drive.
//!
//! Only `check_edit` and `env_lock::HOME_ENV_LOCK` are used, both present at
//! base, so this module compiles at base and is observed RED there.
//!
//! Differs from `editgate::backlog_f7a0cadc` (the worker's repro): that one swaps
//! HOME and REMOVES RUSTUP_HOME/RUSTUP_TOOLCHAIN. These SET all four pinned keys
//! (HOME, RUSTUP_HOME, CARGO_HOME, RUSTUP_TOOLCHAIN) to bogus values, so a fix
//! that pinned only HOME — or only removed keys — stays RED; and the second test
//! checks a genuinely broken crate is still judged by rustc under the swap.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use crate::editgate::check_edit;
use std::ffi::OsString;
use std::path::Path;

const KEYS: [&str; 4] = ["HOME", "RUSTUP_HOME", "CARGO_HOME", "RUSTUP_TOOLCHAIN"];

/// Holds `HOME_ENV_LOCK` and restores every key in `Drop`, so a failing
/// assertion (expected at base) cannot leave the process env poisoned.
struct SwappedEnv {
    saved: Vec<(&'static str, Option<OsString>)>,
    _sandbox: tempfile::TempDir,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl Drop for SwappedEnv {
    fn drop(&mut self) {
        for (k, v) in self.saved.drain(..) {
            match v {
                Some(v) => std::env::set_var(k, v),
                None => std::env::remove_var(k),
            }
        }
    }
}

/// Swap all four toolchain keys to values under which a rustup proxy cannot
/// run any cargo: an empty HOME / RUSTUP_HOME / CARGO_HOME and a toolchain name
/// that is not installed.
fn swap_toolchain_env() -> SwappedEnv {
    let lock = crate::env_lock::HOME_ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let sandbox = tempfile::tempdir().expect("sandbox");
    let saved = KEYS.iter().map(|k| (*k, std::env::var_os(k))).collect();
    std::env::set_var("HOME", sandbox.path());
    std::env::set_var("RUSTUP_HOME", sandbox.path().join("rustup"));
    std::env::set_var("CARGO_HOME", sandbox.path().join("cargo"));
    std::env::set_var("RUSTUP_TOOLCHAIN", "verifier-f7a0cadc-not-installed");
    SwappedEnv {
        saved,
        _sandbox: sandbox,
        _lock: lock,
    }
}

fn scratch_crate(lib_rs: &str) -> tempfile::TempDir {
    let d = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(d.path().join("src")).unwrap();
    std::fs::write(
        d.path().join("Cargo.toml"),
        "[package]\nname = \"verifier_f7a0cadc\"\nversion = \"0.1.0\"\n\
         edition = \"2021\"\n\n[workspace]\n\n[lib]\npath = \"src/lib.rs\"\n",
    )
    .unwrap();
    std::fs::write(d.path().join("src/lib.rs"), lib_rs).unwrap();
    d
}

fn check_under_swapped_env(d: &Path) -> serde_json::Value {
    let _env = swap_toolchain_env();
    check_edit(&d.join("src/lib.rs"), Some(d), true)
}

/// Done criterion 1: a clean crate is not reported broken while a sibling holds
/// a swapped toolchain env — for every pinned key, not just HOME.
#[test]
fn clean_crate_stays_clean_while_all_toolchain_keys_are_swapped() {
    let d = scratch_crate("pub fn f() -> i32 { 1 }\n");
    let out = check_under_swapped_env(d.path());
    assert_eq!(
        out["broken"], false,
        "a compiling crate must not be broken because a sibling swapped \
         HOME/RUSTUP_HOME/CARGO_HOME/RUSTUP_TOOLCHAIN: {out}"
    );
    assert_eq!(out["fallback"], false, "{out}");
    assert_eq!(
        crate::state::enforce_edit_gate(&out),
        crate::state::EditGateDecision::Allow,
        "{out}"
    );
}

/// The swap must not turn a real compile error into an environment verdict
/// either: rustc must actually judge the broken crate (E0308 in the
/// diagnostics), the verdict blocks, and the reason is the compile-error one.
#[test]
fn broken_crate_is_judged_by_rustc_while_all_toolchain_keys_are_swapped() {
    let d = scratch_crate("pub fn f() -> i32 { let s: &str = \"x\"; s }\n");
    let out = check_under_swapped_env(d.path());
    let diag = out["diagnostics"].as_str().unwrap_or_default();
    assert!(
        diag.contains("E0308"),
        "rustc must have run and rejected the code, not rustup: {out}"
    );
    assert_eq!(out["broken"], true, "{out}");
    assert_eq!(
        crate::state::enforce_edit_gate(&out),
        crate::state::EditGateDecision::Reject,
        "{out}"
    );
    assert_eq!(
        out["reason"], "cargo check reported compile/type errors",
        "{out}"
    );
}
