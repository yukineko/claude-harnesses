//! RED, 2026-11-14, by a non-implementer (flow-8cca7ef5 calib RED agent).
//!
//! Property: `neutral_base()` (support/neutral_base.rs) finds a fixture base
//! regardless of the caller's environment. `backlog done` runs the test
//! command with the environment cleared down to `INHERITED_ENV`
//! (crates/backlog/src/evidence.rs); under that env the calibration probe
//! `rm -rf <cwd>/inner` answers Deny instead of Ask and `neutral_base()`
//! panics ("no location-neutral fixture base ... was not Ask").
//!
//! Suspected cause (checked by adding `.env("CLAUDECODE", "1")` to
//! `run_probe`, see the commit message): the probe sets
//! `CLAUDE_CODE_ENTRYPOINT=cli` but not `CLAUDECODE=1`; `ask_available()`
//! needs both, so the confined Ask is hardened to Deny when the caller's env
//! lacks `CLAUDECODE`.
//!
//! Mechanism: the test re-executes this test binary as a child with
//! `env_clear()` plus only the `INHERITED_ENV` names copied from this process,
//! running the child-only entry `child_entry_neutral_base` (a no-op unless
//! `NEUTRAL_BASE_CALIB_CHILD` is set).
//!
//! DOES NOT PROVE: that `INHERITED_ENV` below still equals backlog's list
//! (it is a copy; nothing here reads evidence.rs), that other env differences
//! (e.g. missing `CARGO_TARGET_TMPDIR` or `BLASTGUARD_*` vars) are harmless,
//! or anything about `backlog done` itself; only that the calibration depends
//! on `CLAUDECODE` in a child with that env.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "support/bounded.rs"]
mod bounded;
#[path = "support/neutral_base.rs"]
mod neutral_base;

use std::process::Command;

const CHILD_ENV: &str = "NEUTRAL_BASE_CALIB_CHILD";

/// Copy of crates/backlog/src/evidence.rs INHERITED_ENV (2026-11-14).
const INHERITED_ENV: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
    "TMPDIR",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "TERM",
    "TZ",
    "CARGO_HOME",
    "RUSTUP_HOME",
    "RUSTUP_TOOLCHAIN",
];

/// Child-only entry; a no-op in normal runs.
#[test]
fn child_entry_neutral_base() {
    if std::env::var_os(CHILD_ENV).is_none() {
        return;
    }
    let base = neutral_base::neutral_base();
    println!("CHILD_BASE={}", base.display());
}

/// Run the child with a cleared env + INHERITED_ENV (+ `extra`), return
/// (success, rendered stdout/stderr).
fn run_child(extra: &[(&str, &str)]) -> (bool, String) {
    let mut cmd = Command::new(std::env::current_exe().unwrap());
    cmd.args(["child_entry_neutral_base", "--exact", "--nocapture"])
        .env_clear();
    for k in INHERITED_ENV {
        if let Some(v) = std::env::var_os(k) {
            cmd.env(k, v);
        }
    }
    cmd.env(CHILD_ENV, "1");
    for (k, v) in extra {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    let text = format!(
        "status: {:?}\n--- stdout ---\n{}\n--- stderr ---\n{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.success(), text)
}

#[test]
#[ignore = "RED: neutral_base calibration depends on the caller's CLAUDECODE env (backlog done clears it)"]
fn neutral_base_works_under_backlog_done_env() {
    let (ok, text) = run_child(&[]);
    assert!(
        ok,
        "neutral_base() must succeed under the env `backlog done` gives its test command \
         (INHERITED_ENV only); child output verbatim:\n{text}"
    );
}

/// Control: the same child with `CLAUDECODE=1` added passes, which shows the
/// discriminating variable.
#[test]
fn neutral_base_works_with_claudecode_set_control() {
    let (ok, text) = run_child(&[("CLAUDECODE", "1")]);
    assert!(
        ok,
        "control: neutral_base() with INHERITED_ENV + CLAUDECODE=1 must succeed; child output verbatim:\n{text}"
    );
}
