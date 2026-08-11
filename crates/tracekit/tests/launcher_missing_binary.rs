// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `bin/tracekit` (the POSIX launcher) — what happens when NO per-platform
//! binary is bundled for the host.
//!
//! tracekit has NO hook wiring (`crates/tracekit/hooks/` does not exist). Its
//! consumers are the human CLI and replaykit — and replaykit reads
//! `~/.tracekit/<run_id>/spans.jsonl` straight off disk, not through this
//! binary. The read commands (`trace` / `export` / `list`) print a span set, so
//! an empty stdout with exit 0 reads as "this run has no spans" rather than
//! "the span store was never opened" (CLAUDE.md §1/§3).
//!
//! NOTE (recorded, not fixed here): the launcher's previous comment claimed
//! "condukt's state-set calls `tracekit record` opportunistically", but
//! `grep -rn tracekit crates/condukt/src/` returns ZERO hits at d48c7133, so
//! that justification for exiting 0 did not describe the code.

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
        .join("tracekit")
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
    format!("tracekit-{os}-{arch}{ext}")
}

fn run(args: &[&str], stdin: &str) -> Run {
    let dir = tempfile::tempdir().expect("tempdir");
    let launcher = dir.path().join("tracekit");
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

/// Every subcommand here is CLI/skill-consumed: an empty stdout with exit 0
/// reads as a real (empty) answer.
#[test]
fn every_subcommand_fails_loudly_rather_than_returning_an_empty_result() {
    for args in [
        vec!["list"],
        vec!["trace", "r1"],
        vec!["export", "--run", "r1"],
        vec!["record"],
        vec![],
    ] {
        let r = run(&args, "");
        assert_ne!(
            r.code,
            0,
            "`tracekit {}` must not report success it did not earn: {r:?}",
            args.join(" "),
        );
        assert!(
            r.stdout.trim().is_empty(),
            "a launcher that could not run the binary must not fabricate data: {r:?}",
        );
    }
    let r = run(&["list"], "");
    assert!(
        r.stderr.to_uppercase().contains("NOT"),
        "the diagnostic must say the command did NOT run: {r:?}",
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
    let launcher = dir.path().join("tracekit");
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
        .args(["list"])
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
        "STUB-RAN:list",
        "argv must reach the host binary unchanged",
    );
}
