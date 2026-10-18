// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `bin/precommit-audit` (the POSIX launcher) — what happens when NO
//! per-platform binary is bundled for the host.
//!
//! The launcher used to answer that with a bare `exit 0`, which for this crate
//! is not merely silence but the literal PASS verdict: precommit-audit carries
//! its decision in the EXIT CODE, not in hook JSON. `blocking_exit_for_mode`
//! (src/main.rs) is the crate's own statement of the restrictive answer —
//! `stop` (and its SessionEnd variant) → 2, `precommit` → 1 — and the caught-
//! panic barrier already routes through it precisely because "exit 0 IS a
//! clean/allow verdict, so any crash in the audit reported the working set it
//! had just FAILED to scan as clean" (src/main.rs comment).
//!
//! A missing binary is the same cannot-determine as that caught panic, so the
//! launcher mirrors `blocking_exit_for_mode`. The bound is NOT the binary's
//! bare `stop_hook_active` guard (another gate's block sets that flag): like
//! `run_guarded`, only this gate's SECOND CONSECUTIVE failure in the session (a
//! per-gate, per-session marker under `$HOME/.harness/state/panic-markers/`) is
//! allowed through; a re-entry with an undetermined streak is a loud
//! `systemMessage` allow.

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
        .join("precommit-audit")
}

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
    format!("precommit-audit-{os}-{arch}{ext}")
}

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
    let launcher = dir.path().join("precommit-audit");
    std::fs::copy(launcher_src(), &launcher).expect("copy launcher");

    let mut child = Command::new("sh")
        .arg(&launcher)
        .args(args)
        .env("HOME", home)
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .env_remove("AUDIT_MODE")
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

/// The hooks.json invocation. 2 is this crate's blocking code for `stop`.
#[test]
fn stop_mode_missing_binary_exits_with_the_crates_own_blocking_code() {
    let r = run(
        &["--mode", "stop"],
        r#"{"session_id":"s1","stop_hook_active":false}"#,
    );
    assert_eq!(
        r.code, 2,
        "blocking_exit_for_mode(\"stop\") == 2; exit 0 IS the clean verdict: {r:?}",
    );
    assert!(
        r.stderr.to_uppercase().contains("NOT"),
        "the diagnostic must say the audit did NOT run: {r:?}",
    );
}

/// `stop` is also the DEFAULT mode (`resolve_mode` falls back to "stop"), so a
/// bare invocation must not slip past into the permissive side.
#[test]
fn default_mode_is_stop_and_still_blocks() {
    let r = run(&[], r#"{"session_id":"s1","stop_hook_active":false}"#);
    assert_eq!(r.code, 2, "default mode is stop: {r:?}");
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
    assert_eq!(r.code, 2, "{what}: {r:?}");
    assert!(
        !r.stderr.trim().is_empty(),
        "block reason rides stderr: {r:?}"
    );
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
        msg.contains("precommit-audit") && msg.contains("NOT"),
        "{what}: systemMessage must name the gate and say it did NOT check: {v}",
    );
}

/// Fresh session (another gate's block caused the re-entry; this gate has not
/// failed before) -> block, not allow.
#[test]
fn stop_reentry_in_a_fresh_session_blocks() {
    let r = run(&["--mode", "stop"], &ev(true, Some("fresh-s")));
    assert_blocks(
        &r,
        "first failure of this gate must block even on a re-entry",
    );
}

/// The "never trapped" bound: first stop blocks, the re-entry after it (same
/// session, same HOME) is allowed with empty stdout, and the streak continues.
#[test]
fn stop_reentry_is_allowed_through_so_the_session_is_never_trapped() {
    let home = tempfile::tempdir().expect("home");
    let r1 = run_in(home.path(), &["--mode", "stop"], &ev(false, Some("s1")));
    assert_blocks(&r1, "first stop");
    let r2 = run_in(home.path(), &["--mode", "stop"], &ev(true, Some("s1")));
    assert_plain_allow(&r2, "second consecutive failure");
    let r3 = run_in(home.path(), &["--mode", "stop"], &ev(true, Some("s1")));
    assert_plain_allow(&r3, "streak continues");
    // a different session in the same HOME has its own (empty) streak
    let r4 = run_in(home.path(), &["--mode", "stop"], &ev(true, Some("other")));
    assert_blocks(&r4, "markers are per session");
}

/// No session id at all (payload without `session_id`, env removed): a
/// re-entry's streak is undetermined -> loud allow; the same payload as a first
/// stop still blocks.
#[test]
fn stop_reentry_without_session_id_is_a_loud_allow_but_first_stop_still_blocks() {
    let home = tempfile::tempdir().expect("home");
    let r = run_in(home.path(), &["--mode", "stop"], &ev(true, None));
    assert_loud_allow(&r, "undetermined streak");
    let r = run_in(home.path(), &["--mode", "stop"], &ev(false, None));
    assert_blocks(&r, "first stop without a session id");
}

/// The git pre-commit invocation aborts the commit with 1, not 2.
#[test]
fn precommit_mode_missing_binary_exits_one() {
    let r = run(&["--mode", "precommit"], "");
    assert_eq!(
        r.code, 1,
        "blocking_exit_for_mode(\"precommit\") == 1: {r:?}",
    );
}

/// `trust` is an operator write command, not a hook; an exit 0 that trusted
/// nothing would read as "the root is now trusted".
#[test]
fn trust_subcommand_exits_nonzero() {
    let r = run(&["trust"], "");
    assert_ne!(r.code, 0, "{r:?}");
    assert!(r.stdout.trim().is_empty(), "{r:?}");
}

// ------------------------------------------------------------- anti-vacuity

#[test]
fn control_launcher_still_execs_the_host_binary_when_it_is_present() {
    let dir = tempfile::tempdir().expect("tempdir");
    let launcher = dir.path().join("precommit-audit");
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
        .args(["--mode", "stop"])
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
        "STUB-RAN:--mode stop",
        "argv must reach the host binary unchanged",
    );
}
