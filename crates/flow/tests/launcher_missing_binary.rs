// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `bin/flow` (the POSIX launcher) — what happens when NO per-platform
//! binary is bundled for the host.
//!
//! `propose` is flow's only subcommand and its only hook (SessionStart). It
//! injects a "/flow を開始しますか" proposal when the backlog has pending items
//! and is SILENT when the queue is empty — so a missing binary's silent exit 0
//! is byte-for-byte "the queue is empty". That is impact (b) of backlog item
//! a33f644f: flow's Step 3-1 stop predicate reads "no pending tasks" as an
//! empty queue and ends the autonomous loop.
//!
//! The binary already distinguishes the third answer (`fallback_directive`,
//! src/main.rs, for when the backlog binary is not available); the launcher
//! must say the same thing for its own absence.

use std::io::Write;
use std::process::{Command, Stdio};

#[derive(Debug)]
struct Run {
    code: i32,
    stdout: String,
    // Kept for the `{r:?}` assertion messages: a failure here is only
    // diagnosable with the launcher's own diagnostic in hand.
    #[allow(dead_code)]
    stderr: String,
}

fn launcher_src() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("bin")
        .join("flow")
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
    format!("flow-{os}-{arch}{ext}")
}

fn run(args: &[&str], stdin: &str) -> Run {
    let dir = tempfile::tempdir().expect("tempdir");
    let launcher = dir.path().join("flow");
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

/// The hook subcommand. Silence here is read as a real, clean answer, so the
/// launcher must inject an explicit "did not run / UNKNOWN" instead.
#[test]
fn hook_subcommand_reports_unknown_rather_than_staying_silent() {
    let r = run(&["propose"], r#"{"cwd":"/tmp"}"#);
    assert_eq!(r.code, 0, "this hook event cannot block: {r:?}");
    assert!(
        !r.stdout.trim().is_empty(),
        "silence is what a clean run looks like: {r:?}",
    );
    let s = r.stdout.to_uppercase();
    assert!(
        r.stdout.contains("flow") && s.contains("NOT"),
        "the injected text must name the cause and say it did NOT run: {r:?}",
    );
}

/// `propose` is flow's ONLY subcommand, so there is no CLI surface to protect —
/// but an unrecognised invocation must still not look like a clean empty run.
#[test]
fn unknown_subcommand_exits_nonzero_rather_than_printing_an_empty_result() {
    let r = run(&["not-a-real-subcommand"], "");
    assert_ne!(
        r.code, 0,
        "an unrecognised CLI call must not look like clean empty output: {r:?}",
    );
}

// ------------------------------------------------------------- anti-vacuity

/// Without this control the assertions above would still pass if the launcher
/// were broken outright (syntax error, or an unconditional non-zero exit that
/// never execs). Plant a stub under the host triple and prove the launcher
/// still execs it, forwards argv, and propagates its exit code.
#[test]
fn control_launcher_still_execs_the_host_binary_when_it_is_present() {
    let dir = tempfile::tempdir().expect("tempdir");
    let launcher = dir.path().join("flow");
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
        .args(["propose"])
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
        "STUB-RAN:propose",
        "argv must reach the host binary unchanged",
    );
}
