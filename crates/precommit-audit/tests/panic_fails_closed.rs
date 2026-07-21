//! End-to-end regression for backlog 99b71596 ("fail-open #9").
//!
//! `precommit-audit` is a block-capable Claude Code hook. Its `main()` wraps the
//! audit body in `std::panic::catch_unwind(run)`. Previously a caught panic fell
//! back to `exit(0)` — but exit 0 IS a clean/allow verdict, so ANY panic
//! mid-audit reported the working set it had just FAILED to scan as clean (a
//! fail-open). The fix makes a caught panic exit with the mode's BLOCKING code
//! (stop→2, precommit→1), consistent with the crate's git-error and
//! incomplete-scan paths which already fail closed. This mirrors blastguard#3
//! (panic→Deny).
//!
//! These tests spawn the real built binary with the debug-only fault injector
//! `PRECOMMIT_AUDIT_FORCE_PANIC=1`, which panics `run()` right after arg parsing
//! (before any stdin read), and assert the process exits NON-ZERO with the
//! mode's blocking code. They pin panic→block, NOT panic→exit0/allow.
//!
//! NOTE: the fault injector is `#[cfg(debug_assertions)]`, so these tests are
//! meaningful only against a debug build (the default for `cargo test`).

use std::path::PathBuf;
use std::process::{Command, Stdio};

/// A fresh, unique temp dir for `--root` so the run never depends on repo git
/// state. Not created on disk — the forced panic fires before the root is used.
fn unique_root(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "precommit-audit-panic-{}-{}",
        std::process::id(),
        tag
    ))
}

/// Run the built binary under a forced panic and return its exit code.
/// `mode` of `None` omits `--mode` entirely (default-mode case).
fn run_forced_panic(mode: Option<&str>, tag: &str) -> i32 {
    let root = unique_root(tag);
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_precommit-audit"));
    cmd.arg("--root").arg(&root);
    if let Some(m) = mode {
        cmd.arg("--mode").arg(m);
    }
    let status = cmd
        .env("PRECOMMIT_AUDIT_FORCE_PANIC", "1")
        .stdin(Stdio::null()) // empty stdin
        .stdout(Stdio::null())
        .stderr(Stdio::null()) // caught panic prints a backtrace — expected, ignore it
        .status()
        .expect("failed to spawn precommit-audit binary");

    status
        .code()
        .expect("process terminated by signal, not a normal exit")
}

#[test]
fn stop_mode_panic_exits_two_not_zero() {
    let code = run_forced_panic(Some("stop"), "stop");
    assert_eq!(
        code, 2,
        "mode=stop under a forced panic must exit 2 (block the Stop hook), got {code}"
    );
    assert_ne!(
        code, 0,
        "mode=stop: a panic must never be swallowed into exit 0, got {code}"
    );
}

#[test]
fn precommit_mode_panic_exits_one_not_zero() {
    let code = run_forced_panic(Some("precommit"), "precommit");
    assert_eq!(
        code, 1,
        "mode=precommit under a forced panic must exit 1 (abort the commit), got {code}"
    );
    assert_ne!(
        code, 0,
        "mode=precommit: a panic must never be swallowed into exit 0, got {code}"
    );
}

#[test]
fn default_mode_panic_exits_two_not_zero() {
    let code = run_forced_panic(None, "default");
    assert_eq!(
        code, 2,
        "default mode (no --mode) under a forced panic must exit 2, got {code}"
    );
    assert_ne!(
        code, 0,
        "default mode: a panic must never be swallowed into exit 0, got {code}"
    );
}

/// The CORE regression assertion, stated once explicitly across every mode:
/// under a forced panic the exit code is NON-ZERO. A panic must never be
/// swallowed into exit 0 / a clean allow (fail-open #9).
#[test]
fn panic_never_yields_exit_zero_in_any_mode() {
    for mode in [Some("stop"), Some("precommit"), None] {
        let label = mode.unwrap_or("<default>");
        let code = run_forced_panic(mode, "core");
        assert_ne!(
            code, 0,
            "mode={label}: forced panic yielded exit 0 (fail-open) — the panic \
             barrier must fail CLOSED with a non-zero blocking code, got {code}"
        );
    }
}
