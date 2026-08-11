// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `bin/deepwiki` (the POSIX launcher) — what happens when NO per-platform
//! binary is bundled for the host.
//!
//! deepwiki has NO hook wiring (`crates/deepwiki/hooks/` does not exist); it is
//! invoked only by the `/deepwiki` command and by the compass / condukt / scout
//! skills, all of which read its stdout as data. The launcher used to exit 0
//! with an empty stdout, which reads as "the wiki lookup returned nothing"
//! rather than "the lookup never happened" (CLAUDE.md §1/§3).

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
        .join("deepwiki")
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
    format!("deepwiki-{os}-{arch}{ext}")
}

fn run(args: &[&str], stdin: &str) -> Run {
    let dir = tempfile::tempdir().expect("tempdir");
    let launcher = dir.path().join("deepwiki");
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
    for args in [vec!["ask", "repo", "q"], vec!["search", "q"], vec![]] {
        let r = run(&args, "");
        assert_ne!(
            r.code,
            0,
            "`deepwiki {}` must not report success it did not earn: {r:?}",
            args.join(" "),
        );
        assert!(
            r.stdout.trim().is_empty(),
            "a launcher that could not run the binary must not fabricate data: {r:?}",
        );
    }
    let r = run(&["ask", "repo", "q"], "");
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
    let launcher = dir.path().join("deepwiki");
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
        .args(["search", "q"])
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
        "STUB-RAN:search q",
        "argv must reach the host binary unchanged",
    );
}
