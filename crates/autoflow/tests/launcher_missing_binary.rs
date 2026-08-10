// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `bin/autoflow` (the POSIX launcher) — what happens when NO per-platform
//! binary is bundled for the host.
//!
//! The launcher used to answer that with a bare `exit 0` for every subcommand,
//! its comment saying so verbatim ("it exits 0 silently so a hook NEVER breaks
//! the user's turn"). autoflow is wired into four hook events, and two of them
//! read silence as a real answer:
//!
//! * `stop` — Stop: `stop_command` emits `{"decision":"block"}` (main.rs:435),
//!   so no output = the turn ends as if the progress/loop state machine had run
//!   and had nothing to block.
//! * `session-start` — SessionStart: `session_start_command` ALREADY
//!   distinguishes the three answers (main.rs:531-563) — `Known(empty)` stays
//!   silent, but `Determination::Undetermined` prints an `additionalContext`
//!   saying the queue could not be checked and that this "does not mean there
//!   is no open work". A missing binary is exactly that Undetermined case, so
//!   the launcher must speak the same sentence rather than fall back into the
//!   silent `Known(empty)` shape.
//!
//! The other two (`pre-compact`, `prompt-submit`) are a marker write and its
//! own consumer: both live in this one binary, so when it is missing there is
//! no reader left to mistake the missing marker for "nothing to resume". They
//! keep exit 0 but must still say on stderr that they did not run.

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

/// The binary's own `Undetermined` arm says the queue could not be checked and
/// that this "does not mean there is no open work". A missing binary is the
/// same condition, so the launcher must say it too — staying silent puts it
/// back into the `Known(empty)` shape, which reads as "no pending work".
#[test]
fn session_start_missing_binary_reports_undetermined_rather_than_staying_silent() {
    let r = run(&["session-start"], r#"{"cwd":"/tmp"}"#);
    assert_eq!(r.code, 0, "SessionStart cannot block: {r:?}");
    assert!(
        !r.stdout.trim().is_empty(),
        "silence here reads as an empty queue — the very thing that could not \
         be established: {r:?}",
    );
    let v: serde_json::Value = serde_json::from_str(r.stdout.trim())
        .unwrap_or_else(|e| panic!("stdout must be hook JSON ({e}): {}", r.stdout));
    let ctx = v["additionalContext"]
        .as_str()
        .or_else(|| v["hookSpecificOutput"]["additionalContext"].as_str())
        .unwrap_or_default();
    assert!(
        ctx.contains("autoflow"),
        "the injected context must name the cause: {v}",
    );
    assert!(
        ctx.to_uppercase().contains("NOT")
            && (ctx.contains("open work") || ctx.contains("empty queue")),
        "it must explicitly deny the 'no open work' reading: {ctx}",
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
