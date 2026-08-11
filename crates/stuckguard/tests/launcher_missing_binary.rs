// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `bin/stuckguard` (the POSIX launcher) — what happens when NO per-platform
//! binary is bundled for the host.
//!
//! The launcher used to answer that with a bare `exit 0`. `watch` is
//! PostToolUse, whose only steering channel is `additionalContext`, and the
//! binary ALREADY distinguishes the three answers there: when the session
//! history cannot be read it emits `undetermined_history_message()`
//! (src/main.rs:154/235) — "ループ検知の窓を復元できないため、念のため通知します"
//! — rather than staying silent. A missing binary is the same cannot-determine,
//! only worse (nothing was even recorded), so the launcher must speak the same
//! sentence instead of collapsing back into the silent "no loop detected"
//! shape (CLAUDE.md §1/§3).

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
        .join("stuckguard")
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
    format!("stuckguard-{os}-{arch}{ext}")
}

fn run(args: &[&str], stdin: &str) -> Run {
    let dir = tempfile::tempdir().expect("tempdir");
    let launcher = dir.path().join("stuckguard");
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

#[test]
fn watch_missing_binary_warns_instead_of_reading_as_no_loop_detected() {
    let r = run(&["watch"], r#"{"tool_name":"Bash"}"#);
    assert_eq!(r.code, 0, "PostToolUse hooks cannot block: {r:?}");
    assert!(
        !r.stdout.trim().is_empty(),
        "silence is what 'no stuck pattern' looks like: {r:?}",
    );
    let v: serde_json::Value = serde_json::from_str(r.stdout.trim())
        .unwrap_or_else(|e| panic!("stdout must be hook JSON ({e}): {}", r.stdout));
    assert_eq!(
        v["hookSpecificOutput"]["hookEventName"], "PostToolUse",
        "{v}"
    );
    let ctx = v["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap_or_default();
    assert!(
        ctx.contains("stuckguard"),
        "the warning must name the cause: {ctx}",
    );
    assert!(
        ctx.to_uppercase().contains("NOT") || ctx.contains("ません"),
        "it must say the loop detection did NOT run: {ctx}",
    );
}

#[test]
fn cli_subcommands_exit_nonzero_rather_than_printing_an_empty_result() {
    for args in [vec!["status"], vec!["init"], vec!["install"], vec![]] {
        let r = run(&args, "");
        assert_ne!(
            r.code,
            0,
            "`stuckguard {}` must not report success it did not earn: {r:?}",
            args.join(" "),
        );
    }
}

// ------------------------------------------------------------- anti-vacuity

#[test]
fn control_launcher_still_execs_the_host_binary_when_it_is_present() {
    let dir = tempfile::tempdir().expect("tempdir");
    let launcher = dir.path().join("stuckguard");
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
        .args(["watch"])
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
        "STUB-RAN:watch",
        "argv must reach the host binary unchanged",
    );
}
