//! backlog 2026-11-18: the blastguard binary exits 0 with no output for an
//! unknown subcommand (observed on 0.2.675 and 0.2.690). The relay
//! subcommands moved to the `askrelay` plugin, so `blastguard relay dismiss
//! <id> --reason x` prints nothing, records nothing, and looks like success.
//!
//! Repro: cargo test -p blastguard --test backlog_20261118_unknown_subcommand_fails_loudly

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

struct Run {
    out: Output,
}

impl Run {
    fn stdout(&self) -> String {
        String::from_utf8_lossy(&self.out.stdout).into_owned()
    }
    fn stderr(&self) -> String {
        String::from_utf8_lossy(&self.out.stderr).into_owned()
    }
}

fn scratch(tag: &str) -> PathBuf {
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("unknown-subcmd-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&d).expect("create scratch dir");
    d
}

fn run(tag: &str, args: &[&str], stdin: &str) -> Run {
    let base = scratch(tag);
    let proj = base.join("proj");
    let home = base.join("home");
    std::fs::create_dir_all(&proj).expect("create proj");
    std::fs::create_dir_all(&home).expect("create home");
    let mut child = Command::new(env!("CARGO_BIN_EXE_blastguard"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("CLAUDE_PROJECT_DIR", &proj)
        .env("BLASTGUARD_APPROVALS_DIR", base.join("approvals"))
        .env("HOME", &home)
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .env("CLAUDECODE", "1")
        .env_remove("BLASTGUARD_ASK")
        .env_remove("TMPDIR")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .current_dir(&base)
        .spawn()
        .expect("spawn blastguard");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(stdin.as_bytes())
        .expect("write stdin");
    let out = child.wait_with_output().expect("wait");
    Run { out }
}

#[test]
fn unknown_subcommand_exits_nonzero_with_stderr() {
    let r = run("unknown", &["no-such-subcommand"], "");
    let code = r.out.status.code();
    assert!(
        code.is_some() && code != Some(0),
        "unknown subcommand must exit non-zero, got {code:?} (stdout={:?} stderr={:?})",
        r.stdout(),
        r.stderr()
    );
    assert!(
        !r.stderr().trim().is_empty(),
        "unknown subcommand must explain itself on stderr, got empty stderr"
    );
}

#[test]
fn moved_relay_subcommand_exits_nonzero_and_names_askrelay() {
    let r = run(
        "relay",
        &["relay", "dismiss", "bgr-000000000000", "--reason", "x"],
        "",
    );
    let code = r.out.status.code();
    assert!(
        code.is_some() && code != Some(0),
        "moved `relay` subcommand must exit non-zero, got {code:?} (stdout={:?} stderr={:?})",
        r.stdout(),
        r.stderr()
    );
    let both = format!("{}{}", r.stdout(), r.stderr());
    assert!(
        both.contains("askrelay"),
        "output must point at the askrelay plugin, got {both:?}"
    );
}

#[test]
fn control_version_still_works() {
    let r = run("version", &["--version"], "");
    assert_eq!(r.out.status.code(), Some(0));
    assert_eq!(
        r.stdout(),
        format!("blastguard {}\n", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn control_hook_entry_point_allows_benign_command() {
    let base = scratch("hook-cwd");
    let payload = format!(
        r#"{{"session_id":"unknown-subcmd","cwd":"{cwd}","tool_name":"Bash","tool_input":{{"command":"ls"}}}}"#,
        cwd = base.display()
    );
    let r = run("hook", &[], &payload);
    assert_eq!(r.out.status.code(), Some(0), "stderr={:?}", r.stderr());
    assert!(
        r.stdout().trim().is_empty(),
        "benign `ls` must allow with empty stdout, got {:?}",
        r.stdout()
    );
}
