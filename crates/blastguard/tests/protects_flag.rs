#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED tests for backlog 3a8e3b73, slice 1: `blastguard --protects` (first
//! adopter). Independent author (CLAUDE.md 2(a)). Refusal-line summary and
//! taxonomy column are deferred to a later slice.

#[path = "support/bounded.rs"]
mod bounded;

use std::process::Command;
use std::time::Duration;

fn run(arg: &str) -> (Option<i32>, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_blastguard"));
    cmd.arg(arg).env_clear();
    // Not exited within 10s: run_bounded kills the group and panics.
    let out =
        bounded::run_bounded_stdin(&mut cmd, bounded::ChildStdin::Null, Duration::from_secs(10));
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

#[test]
fn protects_prints_three_labelled_nonempty_lines_and_exits_zero() {
    let (code, stdout) = run("--protects");
    assert_eq!(code, Some(0), "stdout={stdout:?}");
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 3, "want exactly 3 lines, got {stdout:?}");
    for (line, label) in lines.iter().zip(["PROTECTS: ", "AGAINST: ", "GROUNDS: "]) {
        let rest = line
            .strip_prefix(label)
            .unwrap_or_else(|| panic!("line {line:?} must start with {label:?}"));
        assert!(!rest.trim().is_empty(), "{label} text is empty");
    }
}

/// Control: the existing short-circuit flag keeps working.
#[test]
fn version_flag_still_works() {
    let (code, stdout) = run("--version");
    assert_eq!(code, Some(0));
    assert!(stdout.starts_with("blastguard "), "{stdout:?}");
}
