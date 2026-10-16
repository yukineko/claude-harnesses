#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `bin/onepass` with no bundled per-platform binary: `gate` must block
//! (exit 2), never exit 0 (which a PreToolUse hook reads as an allow).

use std::process::{Command, Stdio};

fn run(sub: &str) -> i32 {
    let dir = tempfile::tempdir().unwrap();
    let launcher = dir.path().join("onepass");
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("bin")
            .join("onepass"),
        &launcher,
    )
    .unwrap();
    Command::new("sh")
        .arg(&launcher)
        .arg(sub)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap()
        .code()
        .unwrap_or(-1)
}

use std::path::Path;

#[test]
fn gate_without_binary_blocks() {
    assert_eq!(run("gate"), 2);
}

#[test]
fn reset_without_binary_does_not_block_the_prompt() {
    assert_eq!(run("reset"), 0);
}
