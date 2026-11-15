//! The canonical spelling of the fixed temp root `/tmp`, for integration-test
//! fixtures (backlog a77dd80f).
//!
//! On macOS `/tmp` is a symlink to `/private/tmp`, so this returns
//! `/private/tmp` — byte-identical to the literal the fixtures used to
//! hard-code. On Linux `/tmp` is a real directory and `/private` does not
//! exist, so it returns `/tmp`. Both are fixed temp roots blastguard knows
//! (`TEMP_ROOTS` / `DELETION_TEMP_ROOTS` in `src/scope.rs`), so a fixture
//! placed here sits under a real (non-symlink) fixed temp root on either OS.
//!
//! Deliberately NOT `std::env::temp_dir()`: that honours `$TMPDIR`, which may
//! point at a per-user dir (macOS `/var/folders/...`) that is not the fixed
//! temp root these fixtures exercise, and several fixtures strip or override
//! `$TMPDIR` for the hook under test.
//!
//! Panics with the reason when `/tmp` cannot be canonicalised: a fixture that
//! silently landed elsewhere would stop testing what it tests (cannot
//! determine is never "clean").

#![allow(dead_code)]

use std::path::PathBuf;

/// `/private/tmp` on macOS, `/tmp` on Linux.
pub fn canonical_fixed_temp_root() -> PathBuf {
    std::fs::canonicalize("/tmp").unwrap_or_else(|e| {
        panic!(
            "cannot canonicalise the fixed temp root /tmp ({e}); the \
             fixed-temp-root fixtures cannot be placed in this environment"
        )
    })
}

/// [`canonical_fixed_temp_root`] as a display string (no trailing slash),
/// for building command text and `format!` paths.
pub fn canonical_fixed_temp_root_str() -> String {
    canonical_fixed_temp_root().display().to_string()
}
