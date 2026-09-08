// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `bin/autoflow` (the POSIX launcher) — what happens when NO per-platform
//! binary is bundled for the host.
//!
//! The launcher used to answer that with a bare `exit 0` for every subcommand,
//! its comment saying so verbatim ("it exits 0 silently so a hook NEVER breaks
//! the user's turn"). autoflow is wired into three hook events, and one of them
//! reads silence as a real answer:
//!
//! * `stop` — Stop: `stop_command` emits `{"decision":"block"}`, so no output =
//!   the turn ends as if the progress/loop state machine had run and had nothing
//!   to block.
//!
//! The other two (`pre-compact`, `prompt-submit`) are a marker write and its
//! own consumer: both live in this one binary, so when it is missing there is
//! no reader left to mistake the missing marker for "nothing to resume". They
//! keep exit 0 but must still say on stderr that they did not run.
//!
//! `session-start` was a fourth event until 2026-08-20, classified with `stop`
//! because it proposed `/flow` from the backlog queue and silence would have read
//! as an empty queue. The subcommand, the hook and the launcher arm are all
//! retired; the test below pins that the retired arg gets the catch-all's `exit
//! 1` rather than a successful-looking reply.

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
        .join("autoflow")
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
    format!("autoflow-{os}-{arch}{ext}")
}

fn run(args: &[&str], stdin: &str) -> Run {
    let dir = tempfile::tempdir().expect("tempdir");
    let launcher = dir.path().join("autoflow");
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

// -------------------------------------------------------------- Stop verdict

#[test]
fn stop_missing_binary_blocks_instead_of_silently_certifying() {
    let r = run(&["stop"], r#"{"session_id":"s1","stop_hook_active":false}"#);
    assert_eq!(r.code, 0, "Stop decisions ride stdout JSON: {r:?}");
    let v: serde_json::Value = serde_json::from_str(r.stdout.trim())
        .unwrap_or_else(|e| panic!("stdout must be hook JSON ({e}): {}", r.stdout));
    assert_eq!(
        v["decision"], "block",
        "cannot-determine must resolve to block, never to a silent allow: {v}",
    );
    let reason = v["reason"].as_str().unwrap_or_default();
    assert!(
        reason.contains("autoflow") && reason.to_uppercase().contains("NOT"),
        "the reason must name the cause and say the gate did NOT run: {reason}",
    );
}

#[test]
fn stop_reentry_is_allowed_through_so_the_session_is_never_trapped() {
    let r = run(&["stop"], r#"{"session_id":"s1","stop_hook_active":true}"#);
    assert_eq!(r.code, 0, "{r:?}");
    assert!(r.stdout.trim().is_empty(), "bounded block: {r:?}");
    assert!(!r.stderr.trim().is_empty(), "never silent: {r:?}");
}

// ------------------------------------------------------- SessionStart notice

/// RETIREMENT PIN (2026-08-20). `session-start` is no longer a subcommand, no
/// longer wired to a hook, and no longer an arm of the launcher. The launcher's
/// standing rule for an arg it does not recognise is `exit 1` — "an unrecognised
/// invocation must not look like a clean empty result" — and that is exactly what
/// a retired hook name must get. On the previous revision this same call printed
/// an `additionalContext` and exited 0; asserting non-zero here is the inversion,
/// so a re-added arm goes red.
#[test]
fn a_retired_session_start_arg_is_not_answered_as_if_it_ran() {
    let r = run(&["session-start"], r#"{"cwd":"/tmp"}"#);
    assert_ne!(
        r.code, 0,
        "a subcommand that no longer exists must not exit 0: {r:?}"
    );
    assert!(
        r.stdout.trim().is_empty(),
        "and it must not fabricate hook output either: {r:?}"
    );
}

// ------------------------------------------------- non-verdict, but not mute

#[test]
fn marker_hooks_exit_zero_but_still_say_they_did_not_run() {
    for sub in ["pre-compact", "prompt-submit"] {
        let r = run(&[sub], r#"{"session_id":"s1"}"#);
        assert_eq!(r.code, 0, "`autoflow {sub}` must not fail the turn: {r:?}");
        assert!(
            r.stdout.trim().is_empty(),
            "these inject nothing on an ordinary turn; a fabricated line would \
             be per-prompt noise: {r:?}",
        );
        assert!(
            !r.stderr.trim().is_empty(),
            "exit 0 with NO diagnostic is indistinguishable from a clean run: {r:?}",
        );
    }
}

#[test]
fn unknown_subcommand_exits_nonzero() {
    let r = run(&["not-a-real-subcommand"], "");
    assert_ne!(r.code, 0, "{r:?}");
}

// ------------------------------------------------------------- anti-vacuity

#[test]
fn control_launcher_still_execs_the_host_binary_when_it_is_present() {
    let dir = tempfile::tempdir().expect("tempdir");
    let launcher = dir.path().join("autoflow");
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
        .args(["stop"])
        .stdin(Stdio::null())
        .output()
        .expect("spawn sh");
    assert_eq!(
        out.status.code(),
        Some(0),
        "with the host binary present the launcher must exec it: {}",
        String::from_utf8_lossy(&out.stderr),
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "STUB-RAN:stop",
        "argv must reach the host binary unchanged",
    );
}
