// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `bin/tdd` (the POSIX launcher) — what happens when NO per-platform
//! binary is bundled for the host.
//!
//! The launcher used to answer that with a bare `exit 0`. For a Stop hook,
//! exit 0 with no stdout is byte-for-byte what a PASSING gate looks like, so a
//! missing build made tdd silently certify every stop: the turn ended as
//! if `tdd gate` had run and found nothing to block (CLAUDE.md §1/§3 —
//! "cannot determine" was written into the same channel as "clean").
//!
//! These tests pin the corrected failure mode. The restrictive resolution is
//! spoken in the Stop event's own vocabulary — `{"decision":"block"}` on
//! stdout, exit 0 — bounded by `stop_hook_active` exactly like
//! `harness_core::gate::run::run_guarded`'s panic barrier, so a missing binary
//! surfaces once and can never trap the session. Every non-hook subcommand
//! exits non-zero instead of printing an empty result that reads as data.

use std::io::Write;
use std::process::{Command, Stdio};

#[derive(Debug)]
struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

fn launcher_src() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("bin")
        .join("tdd")
}

/// Host triple the launcher computes, so the anti-vacuity control can plant a
/// stub under exactly the name the launcher looks for.
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
    format!("tdd-{os}-{arch}{ext}")
}

/// Run the real launcher from a directory that provably contains no
/// `tdd-<os>-<arch>` sibling, so the missing-binary path is taken on every
/// platform (the repo's own `bin/` may hold a built binary after a rollout).
fn run(args: &[&str], stdin: &str) -> Run {
    let dir = tempfile::tempdir().expect("tempdir");
    let launcher = dir.path().join("tdd");
    std::fs::copy(launcher_src(), &launcher).expect("copy launcher");

    let mut child = Command::new("sh")
        .arg(&launcher)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn sh");
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(stdin.as_bytes())
        .expect("write stdin");
    let out = child.wait_with_output().expect("wait");
    Run {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

// ----------------------------------------------------------------- verdict

/// A live Stop event with no binary must BLOCK, not certify the stop.
#[test]
fn stop_gate_missing_binary_blocks_instead_of_silently_certifying() {
    let r = run(&["gate"], r#"{"session_id":"s1","stop_hook_active":false}"#);
    assert_eq!(
        r.code, 0,
        "Stop decisions ride stdout JSON, not the exit code: {r:?}",
    );
    assert!(
        !r.stdout.trim().is_empty(),
        "silence IS a passing gate — the launcher must emit a decision: {r:?}",
    );
    let v: serde_json::Value = serde_json::from_str(r.stdout.trim())
        .unwrap_or_else(|e| panic!("stdout must be hook JSON ({e}): {}", r.stdout));
    assert_eq!(
        v["decision"], "block",
        "cannot-determine must resolve to block, never to a silent allow: {v}",
    );
    let reason = v["reason"].as_str().unwrap_or_default();
    assert!(
        reason.contains("tdd") && reason.to_uppercase().contains("NOT"),
        "the reason must name the cause and say the check did NOT run: {reason}",
    );
}

/// Bounded, exactly like `run_guarded`: the post-block re-entry is allowed
/// through so a missing binary can never trap the session in a block loop.
#[test]
fn stop_gate_reentry_is_allowed_through_so_the_session_is_never_trapped() {
    let r = run(&["gate"], r#"{"session_id":"s1","stop_hook_active":true}"#);
    assert_eq!(r.code, 0, "{r:?}");
    assert!(
        r.stdout.trim().is_empty(),
        "the second consecutive stop must be allowed (bounded block): {r:?}",
    );
    assert!(
        !r.stderr.trim().is_empty(),
        "allowing it silently would hide that the gate still has NOT run: {r:?}",
    );
}

/// An empty stdin means this is a manual invocation, not a live Stop event.
/// Fabricating a decision there would be inventing a verdict out of nothing.
#[test]
fn stop_gate_without_an_event_reports_an_error_rather_than_inventing_a_verdict() {
    let r = run(&["gate"], "");
    assert_ne!(r.code, 0, "no event = no verdict to emit: {r:?}");
    assert!(r.stdout.trim().is_empty(), "{r:?}");
}

// --------------------------------------------------------------------- CLI

/// Every other subcommand is read by humans/skills as data; an empty stdout
/// with exit 0 reads as a real (empty) answer.
#[test]
fn cli_subcommands_exit_nonzero_rather_than_printing_an_empty_result() {
    for args in [
        vec!["verify", "--task", "t1"],
        vec!["oracle", "--task", "t1"],
        vec!["init"],
        vec![],
    ] {
        let r = run(&args, "");
        assert_ne!(
            r.code,
            0,
            "`tdd {}` must not report success it did not earn: {r:?}",
            args.join(" "),
        );
    }
}

// ------------------------------------------------------------- anti-vacuity

/// Without this control the assertions above would still pass if the launcher
/// were broken outright (syntax error, or an unconditional non-zero exit that
/// never execs). Plant a stub under the host triple and prove the launcher
/// still execs it, forwards argv, and propagates its exit code.
#[test]
fn control_launcher_still_execs_the_host_binary_when_it_is_present() {
    let dir = tempfile::tempdir().expect("tempdir");
    let launcher = dir.path().join("tdd");
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
        .args(["gate"])
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
        "STUB-RAN:gate",
        "argv must reach the host binary unchanged",
    );
}
