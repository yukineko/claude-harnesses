// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `bin/propguard` (the POSIX launcher) — what happens when NO per-platform
//! binary is bundled for the host.
//!
//! The launcher used to answer that with a bare `exit 0`. For a Stop hook,
//! exit 0 with no stdout is byte-for-byte what a PASSING gate looks like, so a
//! missing build made propguard silently certify every stop: the turn ended as
//! if `propguard check` had run and found nothing to block (CLAUDE.md §1/§3 —
//! "cannot determine" was written into the same channel as "clean").
//!
//! These tests pin the corrected failure mode. The restrictive resolution is
//! spoken in the Stop event's own vocabulary — `{"decision":"block"}` on
//! stdout, exit 0 — bounded like `harness_core::gate::run::run_guarded`'s panic
//! barrier: `stop_hook_active` alone is NOT enough (another gate's block sets
//! it), so only this gate's SECOND CONSECUTIVE failure in the session (a
//! per-gate, per-session marker under `$HOME/.harness/state/panic-markers/`) is
//! allowed through, and a re-entry whose streak cannot be determined is a loud
//! `systemMessage` allow. A missing binary thus never traps the session. Every non-hook subcommand
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
        .join("propguard")
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
    format!("propguard-{os}-{arch}{ext}")
}

/// Run the real launcher from a directory that provably contains no
/// `propguard-<os>-<arch>` sibling, so the missing-binary path is taken on every
/// platform (the repo's own `bin/` may hold a built binary after a rollout).
fn run(args: &[&str], stdin: &str) -> Run {
    let home = tempfile::tempdir().expect("home tempdir");
    run_in(home.path(), args, stdin)
}

/// Same, but with an explicit `HOME` so several launcher runs can share one
/// failure-streak marker store. `HOME` is always a tempdir and
/// `CLAUDE_CODE_SESSION_ID` is always removed, so a test never writes a marker
/// into the real home nor keys on the live session.
fn run_in(home: &std::path::Path, args: &[&str], stdin: &str) -> Run {
    let dir = tempfile::tempdir().expect("tempdir");
    let launcher = dir.path().join("propguard");
    std::fs::copy(launcher_src(), &launcher).expect("copy launcher");

    let mut child = harness_core::git_env::scrub(&mut Command::new("sh"))
        .arg(&launcher)
        .args(args)
        .env("HOME", home)
        .env_remove("CLAUDE_CODE_SESSION_ID")
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
    let r = run(
        &["check"],
        r#"{"session_id":"s1","stop_hook_active":false}"#,
    );
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
        reason.contains("propguard") && reason.to_uppercase().contains("NOT"),
        "the reason must name the cause and say the check did NOT run: {reason}",
    );
}

// ------------------------------------------------- failure-streak (9b3f98ec)

/// A re-entry (`stop_hook_active:true`) is set by ANY gate's block, so on its
/// own it says nothing about THIS gate. Only this gate's second consecutive
/// failure in the session may be allowed.
fn ev(active: bool, sid: Option<&str>) -> String {
    match sid {
        Some(s) => format!(r#"{{"session_id":"{s}","stop_hook_active":{active}}}"#),
        None => format!(r#"{{"stop_hook_active":{active}}}"#),
    }
}

fn assert_blocks(r: &Run, what: &str) {
    assert_eq!(r.code, 0, "Stop decisions ride stdout JSON: {r:?}");
    let v: serde_json::Value = serde_json::from_str(r.stdout.trim())
        .unwrap_or_else(|e| panic!("stdout must be a block decision ({e}): {r:?}"));
    assert_eq!(v["decision"], "block", "{what}: {r:?}");
}

fn assert_plain_allow(r: &Run, what: &str) {
    assert_eq!(r.code, 0, "{what}: {r:?}");
    assert!(
        r.stdout.trim().is_empty(),
        "{what}: allow carries no stdout: {r:?}"
    );
    assert!(
        !r.stderr.trim().is_empty(),
        "{what}: allowing silently would hide that the gate has NOT run: {r:?}",
    );
}

fn assert_loud_allow(r: &Run, what: &str) {
    assert_eq!(r.code, 0, "{what}: {r:?}");
    let v: serde_json::Value = serde_json::from_str(r.stdout.trim())
        .unwrap_or_else(|e| panic!("{what}: stdout must be one JSON object ({e}): {r:?}"));
    assert!(v.get("decision").is_none(), "{what}: no decision key: {v}");
    let msg = v["systemMessage"].as_str().unwrap_or_default();
    assert!(
        msg.contains("propguard") && msg.contains("NOT"),
        "{what}: systemMessage must name the gate and say it did NOT check: {v}",
    );
}

/// Fresh session (another gate's block caused the re-entry; this gate has not
/// failed before) -> block, not allow.
#[test]
fn stop_reentry_in_a_fresh_session_blocks() {
    let r = run(&["check"], &ev(true, Some("fresh-s")));
    assert_blocks(
        &r,
        "first failure of this gate must block even on a re-entry",
    );
}

/// The "never trapped" bound: first stop blocks, the re-entry after it (same
/// session, same HOME) is allowed with empty stdout, and the streak continues.
#[test]
fn stop_gate_reentry_is_allowed_through_so_the_session_is_never_trapped() {
    let home = tempfile::tempdir().expect("home");
    let r1 = run_in(home.path(), &["check"], &ev(false, Some("s1")));
    assert_blocks(&r1, "first stop");
    let r2 = run_in(home.path(), &["check"], &ev(true, Some("s1")));
    assert_plain_allow(&r2, "second consecutive failure");
    let r3 = run_in(home.path(), &["check"], &ev(true, Some("s1")));
    assert_plain_allow(&r3, "streak continues");
    // a different session in the same HOME has its own (empty) streak
    let r4 = run_in(home.path(), &["check"], &ev(true, Some("other")));
    assert_blocks(&r4, "markers are per session");
}

/// No session id at all (payload without `session_id`, env removed): a
/// re-entry's streak is undetermined -> loud allow; the same payload as a first
/// stop still blocks.
#[test]
fn stop_reentry_without_session_id_is_a_loud_allow_but_first_stop_still_blocks() {
    let home = tempfile::tempdir().expect("home");
    let r = run_in(home.path(), &["check"], &ev(true, None));
    assert_loud_allow(&r, "undetermined streak");
    let r = run_in(home.path(), &["check"], &ev(false, None));
    assert_blocks(&r, "first stop without a session id");
}

/// An empty stdin means this is a manual invocation, not a live Stop event.
/// Fabricating a decision there would be inventing a verdict out of nothing.
#[test]
fn stop_gate_without_an_event_reports_an_error_rather_than_inventing_a_verdict() {
    let r = run(&["check"], "");
    assert_ne!(r.code, 0, "no event = no verdict to emit: {r:?}");
    assert!(r.stdout.trim().is_empty(), "{r:?}");
}

// --------------------------------------------------------------------- CLI

/// Every other subcommand is read by humans/skills as data; an empty stdout
/// with exit 0 reads as a real (empty) answer.
#[test]
fn cli_subcommands_exit_nonzero_rather_than_printing_an_empty_result() {
    for args in [
        vec!["derive", "x"],
        vec!["status"],
        vec!["trust"],
        vec!["init"],
        vec![],
    ] {
        let r = run(&args, "");
        assert_ne!(
            r.code,
            0,
            "`propguard {}` must not report success it did not earn: {r:?}",
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
    let launcher = dir.path().join("propguard");
    std::fs::copy(launcher_src(), &launcher).expect("copy launcher");

    let stub = dir.path().join(host_binary_name());
    std::fs::write(&stub, "#!/bin/sh\necho \"STUB-RAN:$*\"\nexit 0\n").expect("write stub");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }

    let out = harness_core::git_env::scrub(&mut Command::new("sh"))
        .arg(&launcher)
        .args(["check"])
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
        "STUB-RAN:check",
        "argv must reach the host binary unchanged",
    );
}
