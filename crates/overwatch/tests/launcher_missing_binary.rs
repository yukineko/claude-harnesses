// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `bin/overwatch` (the POSIX launcher) — what happens when NO per-platform
//! binary is bundled for the host.
//!
//! `status` is the SessionStart AND Stop hook (hooks/hooks.json) and also a
//! plain operator command. It is the FIRST measured consumer of backlog item
//! a33f644f: at d48c7133 the same `overwatch status` printed "Backlog pending:
//! 0" against a 219-item queue purely because the backlog launcher exited 0
//! with no stdout.
//!
//! Its own missing-binary path had the same shape one level up: exit 0 + empty
//! stdout, which is what "nothing to report" looks like. The restrictive answer
//! is an explicit UNKNOWN banner (the `unknown` band ctxrot's statusline uses —
//! a blank bar reads as headroom, CLAUDE.md §1 / commit 3b1eb24), never
//! silence.
//!
//! The CLI surface is verdict-carrying by exit code too: `canary-gate`'s
//! consumer treats every code outside {0,3} as CANNOT EVALUATE and fails closed
//! (`scripts/rollout-plugins.sh:915`), so exiting non-zero there is both
//! restrictive and already handled.

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
        .join("overwatch")
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
    format!("overwatch-{os}-{arch}{ext}")
}

fn run(args: &[&str], stdin: &str) -> Run {
    let dir = tempfile::tempdir().expect("tempdir");
    let launcher = dir.path().join("overwatch");
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
    let r = run(&["status"], r#"{"cwd":"/tmp"}"#);
    assert_eq!(r.code, 0, "this hook event cannot block: {r:?}");
    assert!(
        !r.stdout.trim().is_empty(),
        "silence is what a clean run looks like: {r:?}",
    );
    let s = r.stdout.to_uppercase();
    assert!(
        r.stdout.contains("overwatch") && s.contains("NOT"),
        "the injected text must name the cause and say it did NOT run: {r:?}",
    );
}

/// Everything else is CLI/skill-consumed data.
#[test]
fn cli_subcommands_exit_nonzero_rather_than_printing_an_empty_result() {
    for args in [
        vec!["canary-gate", "--threshold", "1"],
        vec!["review-queue"],
        vec!["undetermined-metrics"],
        vec!["sessions"],
        vec!["violations"],
        vec![],
    ] {
        let r = run(&args, "");
        assert_ne!(
            r.code,
            0,
            "`overwatch {}` must not report success it did not earn: {r:?}",
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
    let launcher = dir.path().join("overwatch");
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
        .args(["review-queue"])
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
        "STUB-RAN:review-queue",
        "argv must reach the host binary unchanged",
    );
}
