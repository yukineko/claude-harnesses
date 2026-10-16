#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro: `extract` silently skips non-JSON lines, so garbage -> `[]`, exit 0.
//!
//! `extract.rs::extract_tools` does `Err(_) => continue` on every line that is
//! not valid JSON. A transcript that is entirely garbage (truncated, wrong
//! file, wrong encoding) therefore yields the empty trajectory `[]` with exit 0,
//! indistinguishable from a genuine session that made no tool calls. The exit
//! code table in main.rs reserves 2 for "unreadable or unparseable input".
//!
//! Asserts the correct behaviour: a transcript with no parseable line must be a
//! harness error (non-zero), not an empty successful extraction.

use std::process::Command;

#[test]
#[ignore = "RED repro: extract skips unparseable lines and exits 0 with []"]
fn extract_of_unparseable_transcript_does_not_exit_zero() {
    let dir = std::env::temp_dir().join(format!(
        "trajectoryeval-repro-extract-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let t = dir.join("t.jsonl");
    std::fs::write(
        &t,
        "this is not json\n\u{1}\u{2} neither is this\n{truncated\n",
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_trajectoryeval"))
        .args(["extract", "--transcript", t.to_str().unwrap()])
        .output()
        .unwrap();
    std::fs::remove_dir_all(&dir).ok();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_ne!(
        out.status.code(),
        Some(0),
        "garbage transcript extracted as {stdout:?} with exit 0"
    );
}
