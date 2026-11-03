// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `bin/fde` (the POSIX launcher) when NO per-platform binary is bundled for
//! the host (mirrors `crates/propguard/tests/launcher_missing_binary.rs`).
//!
//! `fde gate` is a PreToolUse hook: exit 0 there is an allow of an edit whose
//! target's ratification was never checked. A missing binary is a
//! cannot-determine, so every subcommand must exit 2 with a stderr message
//! that says the command did NOT run (CLAUDE.md §3).
//!
//! The launcher is copied into a temp dir that provably holds no
//! `fde-<os>-<arch>` sibling, so the missing-binary path is taken even when the
//! repo's own `bin/` holds a built binary after a rollout.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const GIT_LEAK_VARS: &[&str] = &["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE"];

#[derive(Debug)]
struct Run {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

fn launcher_src() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("bin")
        .join("fde")
}

/// Host binary name as the launcher computes it.
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
    format!("fde-{os}-{arch}{ext}")
}

fn sh(launcher: &Path, args: &[&str], stdin: &str) -> Run {
    let mut c = Command::new("sh");
    c.arg(launcher)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for v in GIT_LEAK_VARS {
        c.env_remove(v);
    }
    let mut child = c.spawn().expect("spawn sh");
    {
        let mut si = child.stdin.take().expect("stdin");
        let _ = si.write_all(stdin.as_bytes());
    }
    let out = child.wait_with_output().expect("wait");
    Run {
        code: out.status.code(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

/// Run a copy of the real launcher from an empty temp dir.
fn run_missing(args: &[&str], stdin: &str) -> Run {
    let dir = tempfile::tempdir().expect("tempdir");
    let launcher = dir.path().join("fde");
    std::fs::copy(launcher_src(), &launcher).expect("copy launcher");
    assert!(
        !dir.path().join(host_binary_name()).exists(),
        "fixture: the temp dir must not hold a host binary"
    );
    sh(&launcher, args, stdin)
}

/// The PreToolUse gate with no binary must block (exit 2), with a reason.
#[test]
fn gate_missing_binary_exits_2_with_a_stderr_reason() {
    let payload = r#"{"session_id":"s","cwd":"/","hook_event_name":"PreToolUse","tool_name":"Edit","tool_input":{"file_path":"/x","old_string":"a","new_string":"b"}}"#;
    let r = run_missing(&["gate"], payload);
    assert_eq!(
        r.code,
        Some(2),
        "missing binary = undetermined = PreToolUse block: {r:?}"
    );
    assert!(
        r.stderr.contains("fde") && r.stderr.contains("NOT"),
        "stderr must name fde and say the command did NOT run: {r:?}"
    );
    assert!(
        !r.stdout.contains("\"allow\""),
        "no allow decision may be printed: {r:?}"
    );
}

/// Every other subcommand (and no subcommand) also exits 2 with a reason.
#[test]
fn every_subcommand_missing_binary_exits_2() {
    for args in [
        vec!["status"],
        vec!["resolve"],
        vec!["begin", "--spec", "/nonexistent/spec.toml"],
        vec!["ratify"],
        vec!["evidence", "verify"],
        vec!["report"],
        vec![],
    ] {
        let r = run_missing(&args, "");
        assert_eq!(r.code, Some(2), "`fde {}`: {r:?}", args.join(" "));
        assert!(
            !r.stderr.trim().is_empty(),
            "`fde {}` must explain on stderr: {r:?}",
            args.join(" ")
        );
    }
}

/// Anti-vacuity: with a stub planted under the host name the launcher execs
/// it, forwards argv, and propagates its exit code (an unconditional
/// `exit 2` launcher fails here).
#[cfg(unix)]
#[test]
fn control_launcher_execs_the_host_binary_when_present() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().expect("tempdir");
    let launcher = dir.path().join("fde");
    std::fs::copy(launcher_src(), &launcher).expect("copy launcher");
    let stub = dir.path().join(host_binary_name());
    std::fs::write(&stub, "#!/bin/sh\necho \"STUB-RAN:$*\"\nexit 0\n").expect("write stub");
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).expect("chmod");

    let r = sh(&launcher, &["gate", "x"], "");
    assert_eq!(r.code, Some(0), "stub exit code must pass through: {r:?}");
    assert_eq!(r.stdout.trim(), "STUB-RAN:gate x", "argv forwarded: {r:?}");
}
