// Integration test: unwrap/expect/panic are the failure channel here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Tests that `ChildStdin::Null` and `ChildStdin::Inherit` of
//! `support/bounded.rs::run_bounded_stdin` really connect the child's stdin to
//! /dev/null and to the caller's own stdin respectively.
//!
//! The test process's own stdin under `cargo test` is not under our control, so
//! the `Inherit`/`Null` observations are made in a re-executed copy of this
//! test binary whose stdin is a known pipe carrying `PAYLOAD`. Each of the two
//! tests plays both roles in one fn:
//!
//! - with `BG_STDIN_MODE_PROBE=<mode>` set it is the probe: it runs `/bin/cat`
//!   with that mode and prints a marker line with what `cat` saw;
//! - otherwise it is the parent: it re-runs this binary with `--exact <its own
//!   name>`, the env var set and `PAYLOAD` on the stdin pipe, and asserts on the
//!   marker line (so a probe that did not run cannot pass vacuously).
//!
//! - `Inherit`: the probe's `cat` must echo `PAYLOAD` (a `Null` mapping shows
//!   nothing).
//! - `Null`: the probe's `cat` must echo nothing although the probe's own stdin
//!   carries `PAYLOAD` (an `Inherit` mapping leaks it).

#[path = "support/bounded.rs"]
mod bounded;

use bounded::{run_bounded, run_bounded_stdin, ChildStdin, TOOL_DEADLINE};
use std::process::Command;
use std::time::Duration;

const PAYLOAD: &[u8] = b"STDIN-PAYLOAD-7f3a\n";
const PROBE_ENV: &str = "BG_STDIN_MODE_PROBE";

/// Run `cat` with `stdin` and return the marker line the probe prints.
fn probe_output(mode: &str, stdin: ChildStdin<'_>) -> String {
    let mut cmd = Command::new("/bin/cat");
    let out = run_bounded_stdin(&mut cmd, stdin, TOOL_DEADLINE);
    format!(
        "PROBE-RAN mode={mode} saw=[{}]",
        String::from_utf8_lossy(&out.stdout)
    )
}

/// Re-run test `name` of this binary as the probe for `mode`, with `PAYLOAD`
/// on its stdin. Returns the child's stdout after checking that exactly one
/// test ran and passed.
fn run_as_parent(name: &str, mode: &str) -> String {
    let exe = std::env::current_exe().expect("current_exe");
    let mut cmd = Command::new(exe);
    cmd.args(["--exact", name, "--nocapture", "--test-threads=1"])
        .env(PROBE_ENV, mode);
    let out = run_bounded(&mut cmd, PAYLOAD, TOOL_DEADLINE);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "probe {name} failed: {:?}\nstdout:\n{stdout}\nstderr:\n{}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains("1 passed"),
        "probe {name} did not run exactly one test:\n{stdout}"
    );
    stdout
}

#[test]
fn inherit_connects_child_stdin_to_callers_stdin() {
    if std::env::var(PROBE_ENV).as_deref() == Ok("inherit") {
        println!("{}", probe_output("inherit", ChildStdin::Inherit));
        return;
    }
    let stdout = run_as_parent("inherit_connects_child_stdin_to_callers_stdin", "inherit");
    let want = format!(
        "PROBE-RAN mode=inherit saw=[{}]",
        String::from_utf8_lossy(PAYLOAD)
    );
    assert!(
        stdout.contains(&want),
        "Inherit child did not see the caller's stdin (or the probe did not run):\n{stdout}"
    );
}

#[test]
fn null_does_not_leak_callers_stdin() {
    if std::env::var(PROBE_ENV).as_deref() == Ok("null") {
        println!("{}", probe_output("null", ChildStdin::Null));
        return;
    }
    let stdout = run_as_parent("null_does_not_leak_callers_stdin", "null");
    assert!(
        stdout.contains("PROBE-RAN mode=null saw=[]"),
        "Null child saw something on stdin, or the probe did not run (expected EOF immediately):\n{stdout}"
    );
    assert!(
        !stdout.contains("STDIN-PAYLOAD-7f3a"),
        "Null child received the caller's stdin:\n{stdout}"
    );
}

#[test]
fn null_child_sees_immediate_eof() {
    let mut cmd = Command::new("/bin/sh");
    cmd.args(["-c", "cat; echo done"]);
    let out = run_bounded_stdin(&mut cmd, ChildStdin::Null, Duration::from_secs(20));
    assert!(out.status.success(), "status: {:?}", out.status);
    assert_eq!(String::from_utf8_lossy(&out.stdout), "done\n");
}
