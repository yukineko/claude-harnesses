#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! IMPLEMENTER-WRITTEN (backlog 3a8e3b73, slice 2): written by the same agent
//! that added `parallelguard --protects`, so it is not independent evidence in the
//! CLAUDE.md 2(a) sense. Modeled on crates/blastguard/tests/protects_flag.rs.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn run(arg: &str) -> (Option<i32>, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_parallelguard"))
        .arg(arg)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn parallelguard");
    let deadline = Instant::now() + Duration::from_secs(10);
    while child.try_wait().expect("poll").is_none() {
        if Instant::now() > deadline {
            let _ = child.kill();
            panic!("parallelguard {arg} did not exit within 10s");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let out = child.wait_with_output().expect("output");
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

/// Control: an unknown argument is still refused (the new arm did not turn
/// the dispatcher into a catch-all).
#[test]
fn unknown_argument_still_exits_one() {
    let (code, _stdout) = run("--bogus");
    assert_eq!(code, Some(1));
}
