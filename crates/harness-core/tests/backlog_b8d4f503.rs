#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro for backlog b8d4f503: `run_with_timeout` waits for the child to
//! exit BEFORE it starts draining stdout/stderr, so a child that writes more
//! than the OS pipe buffer (64 KiB) blocks in write(2), never exits, and the
//! call ends as Undetermined "timed out" although the child would finish in
//! milliseconds. Each test uses a 1 MiB payload and a 10 s timeout.

use harness_core::boundary::run_with_timeout;
use harness_core::verdict::Determination;
use std::process::Command;
use std::time::{Duration, Instant};

const MIB: usize = 1_048_576;
const TIMEOUT: Duration = Duration::from_secs(10);

/// Run `script` under `sh -c`; return (exit code, stdout, stderr-len, elapsed).
fn run(script: &str) -> (i32, String, usize, Duration) {
    let mut cmd = Command::new("sh");
    cmd.arg("-c").arg(script);
    let start = Instant::now();
    let out = match run_with_timeout(&mut cmd, TIMEOUT) {
        Determination::Known(o) => o,
        Determination::Undetermined(why) => {
            panic!("expected Known for a fast child with large output, got Undetermined: {why}")
        }
    };
    let elapsed = start.elapsed();
    let code = out.code();
    let stderr_len = out.stderr().len();
    let stdout = match out.stdout_allowing(&[code]) {
        Determination::Known(s) => s,
        Determination::Undetermined(why) => panic!("stdout_allowing({code}) failed: {why}"),
    };
    (code, stdout, stderr_len, elapsed)
}

const GEN: &str = "head -c 1048576 /dev/zero | tr '\\0' a";

#[test]
fn one_mib_on_stdout_exit_zero_is_known_and_complete() {
    let (code, stdout, _, elapsed) = run(GEN);
    assert_eq!(code, 0);
    assert_eq!(stdout.len(), MIB);
    assert!(elapsed < Duration::from_secs(5), "took {elapsed:?}");
}

#[test]
fn one_mib_on_stderr_exit_zero_is_known_and_complete() {
    let (code, stdout, stderr_len, elapsed) = run(&format!("{GEN} >&2"));
    assert_eq!(code, 0);
    assert_eq!(stdout.len(), 0);
    assert_eq!(stderr_len, MIB);
    assert!(elapsed < Duration::from_secs(5), "took {elapsed:?}");
}

#[test]
fn stderr_flood_then_stdout_flood_is_known_and_complete() {
    let (code, stdout, stderr_len, elapsed) = run(&format!("{GEN} >&2; {GEN}"));
    assert_eq!(code, 0);
    assert_eq!(stdout.len(), MIB);
    assert_eq!(stderr_len, MIB);
    assert!(elapsed < Duration::from_secs(5), "took {elapsed:?}");
}

#[test]
fn stdout_flood_then_stderr_flood_is_known_and_complete() {
    let (code, stdout, stderr_len, elapsed) = run(&format!("{GEN}; {GEN} >&2"));
    assert_eq!(code, 0);
    assert_eq!(stdout.len(), MIB);
    assert_eq!(stderr_len, MIB);
    assert!(elapsed < Duration::from_secs(5), "took {elapsed:?}");
}

#[test]
fn nonzero_exit_with_large_stdout_is_known_with_code_and_full_output() {
    let (code, stdout, _, elapsed) = run(&format!("{GEN}; exit 3"));
    assert_eq!(code, 3);
    assert_eq!(stdout.len(), MIB);
    assert!(elapsed < Duration::from_secs(5), "took {elapsed:?}");
}
