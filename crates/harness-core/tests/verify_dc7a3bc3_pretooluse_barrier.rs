#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Independent verifier test for backlog dc7a3bc3. A verdict-bearing
//! PreToolUse hook that panics must exit 2 (block), never 0 (= allow).
//!
//! The barrier ends in `process::exit`, so it is observed in a child process:
//! this test binary re-executes itself, running only `child_entry`, which picks
//! its behaviour from `VERIFY_DC7A3BC3_MODE`.

use std::process::{Command, Stdio};

const ENV: &str = "VERIFY_DC7A3BC3_MODE";

#[test]
fn child_entry() {
    let Ok(mode) = std::env::var(ENV) else {
        return; // normal test run: no-op
    };
    harness_core::hook::run_pretooluse_guarded("verify gate", || match mode.as_str() {
        "ok" => {}
        "panic_str" => panic!("boom"),
        "panic_string" => panic!("boom {}", 1),
        "panic_any" => std::panic::panic_any(7_u8),
        "explicit_exit" => std::process::exit(3),
        other => panic!("unknown mode {other}"),
    });
}

fn run(mode: &str) -> (Option<i32>, String) {
    let child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "child_entry", "--nocapture", "--test-threads=1"])
        .env(ENV, mode)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let out = child.wait_with_output().unwrap();
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stderr).into(),
    )
}

#[test]
fn completing_body_exits_zero() {
    assert_eq!(run("ok").0, Some(0));
}

#[test]
fn every_panic_shape_exits_two_not_zero() {
    for mode in ["panic_str", "panic_string", "panic_any"] {
        let (code, err) = run(mode);
        assert_eq!(
            code,
            Some(2),
            "{mode}: panic must block (exit 2); stderr={err}"
        );
        assert!(err.contains("BLOCKED"), "{mode}: stderr={err}");
    }
}

#[test]
fn explicit_exit_inside_body_is_preserved() {
    assert_eq!(run("explicit_exit").0, Some(3));
}
