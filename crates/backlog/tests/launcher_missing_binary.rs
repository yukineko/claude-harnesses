// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `bin/backlog` (the POSIX launcher) — what happens when NO per-platform
//! binary is bundled for the host.
//!
//! The launcher used to answer that with `exit 0` and an empty stdout, and its
//! own comment stated the intent verbatim: *"it exits 0 silently so a hook
//! NEVER breaks the user's turn"* — the exact sentence CLAUDE.md §1 names as
//! the red flag for verdict-carrying code.
//!
//! backlog has NO hook wiring at all (`crates/backlog/hooks/` does not exist);
//! every consumer is a CLI/skill caller that reads the stdout as *data*. An
//! empty list therefore reads as "the queue is empty", not as "the queue could
//! not be read". Measured 2026-08-10 at `d48c7133`, same cwd, same instant,
//! varying only which launcher was on PATH:
//!
//! * broken launcher (0.2.16, host binary absent) → `overwatch status` printed
//!   `Backlog pending: 0`; the SessionStart banner printed `== Backlog ==
//!   (none)`.
//! * working launcher (0.2.19) → the same `overwatch status` printed
//!   `Backlog pending: 219`.
//!
//! These tests pin the corrected failure mode: a missing binary is a
//! CANNOT-DETERMINE, so the launcher exits non-zero and prints nothing on
//! stdout, which is the one thing every consumer here already knows how to
//! read as "no answer" (`flow::query_backlog_pending` and
//! `overwatch::aggregate::shell_soft` both gate on `status.success()`).

use std::process::{Command, Stdio};

#[derive(Debug)]
struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

/// Run the real launcher from a directory that provably contains no
/// `backlog-<os>-<arch>` sibling, so the missing-binary path is taken on every
/// platform (the repo's own `bin/` may hold a built binary after a rollout).
fn run(args: &[&str]) -> Run {
    let dir = tempfile::tempdir().expect("tempdir");
    let launcher = dir.path().join("backlog");
    std::fs::copy(launcher_src(), &launcher).expect("copy launcher");

    let out = Command::new("sh")
        .arg(&launcher)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("spawn sh");
    Run {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

fn launcher_src() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("bin")
        .join("backlog")
}

/// Host triple the launcher computes, so the anti-vacuity control can plant a
/// stub under exactly the name the launcher will look for.
fn host_binary_name() -> String {
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "windows",
        other => other,
    };
    let arch = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        other => other,
    };
    let ext = if os == "windows" { ".exe" } else { "" };
    format!("backlog-{os}-{arch}{ext}")
}

// ------------------------------------------------------------------ the fix

/// `list --json` is the call every consumer makes. An empty stdout with exit 0
/// parses as "zero tasks" — this is the measured defect.
#[test]
fn list_json_missing_binary_exits_nonzero_instead_of_printing_an_empty_queue() {
    let r = run(&["list", "--json"]);
    assert_ne!(
        r.code, 0,
        "exit 0 + empty stdout IS an empty queue to every consumer \
         (flow::query_backlog_pending, overwatch::aggregate::shell_soft): {r:?}",
    );
    assert!(
        r.stdout.trim().is_empty(),
        "a launcher that could not run the binary must not fabricate data: {r:?}",
    );
    assert!(
        r.stderr.contains("backlog") && r.stderr.contains("no bundled binary"),
        "the diagnostic must name the cause: {r:?}",
    );
}

/// The other queue-shaped reads have the same "empty = none" hazard.
#[test]
fn every_subcommand_fails_loudly_rather_than_returning_an_empty_result() {
    for args in [
        vec!["list"],
        vec!["next"],
        vec!["next", "--claim"],
        vec!["lock", "status"],
        vec!["--version"],
        vec![],
    ] {
        let r = run(&args);
        assert_ne!(
            r.code,
            0,
            "`backlog {}` must not report success it did not earn: {r:?}",
            args.join(" "),
        );
    }
}

/// The diagnostic must say the read did NOT happen, not merely that something
/// is missing — CLAUDE.md §1 requires "UNCHECKED, not approved" phrasing so a
/// reader cannot mistake it for a clean run.
#[test]
fn diagnostic_states_the_queue_was_not_read() {
    let r = run(&["list", "--json"]);
    let s = r.stderr.to_uppercase();
    assert!(
        s.contains("NOT READ") || s.contains("UNREAD") || s.contains("UNKNOWN"),
        "stderr must say the queue could not be read, so an empty result is \
         never mistaken for an empty queue: {r:?}",
    );
}

// ------------------------------------------------------------- anti-vacuity

/// Without this control, `assert_ne!(code, 0)` above would still pass if the
/// launcher were broken outright (e.g. a syntax error, or an unconditional
/// `exit 1` that never execs). Plant a stub under the host triple and prove the
/// launcher still execs it, forwards argv, and propagates its exit code.
#[test]
fn control_launcher_still_execs_the_host_binary_when_it_is_present() {
    let dir = tempfile::tempdir().expect("tempdir");
    let launcher = dir.path().join("backlog");
    std::fs::copy(launcher_src(), &launcher).expect("copy launcher");

    let stub = dir.path().join(host_binary_name());
    std::fs::write(&stub, "#!/bin/sh\necho \"STUB-RAN:$*\"\nexit 0\n").expect("write stub");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }

    let out = Command::new("sh")
        .arg(&launcher)
        .args(["list", "--json"])
        .stdin(Stdio::null())
        .output()
        .expect("spawn sh");
    assert_eq!(
        out.status.code(),
        Some(0),
        "with the host binary present the launcher must exec it and pass its \
         exit code through: {}",
        String::from_utf8_lossy(&out.stderr),
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "STUB-RAN:list --json",
        "argv must reach the host binary unchanged",
    );
}
