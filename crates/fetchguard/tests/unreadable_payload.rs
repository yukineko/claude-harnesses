#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Regression tests for the real `fetchguard scan` BINARY (via
//! `CARGO_BIN_EXE_fetchguard`, not the pure `HookInput::parse` +
//! `gate::analyse` seam `runtime_scan.rs` exercises) when stdin is
//! NON-EMPTY but not a parseable hook payload.
//!
//! `src/main.rs`'s `Command::Scan` arm is:
//!
//! ```ignore
//! let raw = read_stdin();
//! if let Some(input) = HookInput::parse(&raw) {
//!     if let Some(line) = fetchguard::gate::analyse(...) { println!("{line}"); }
//! }
//! ```
//!
//! `harness_core::hook::HookInput::parse` returns `None` for any non-empty
//! string that fails `serde_json::from_str` (`{not json`, a truncated
//! object, plain garbage text — see `harness-core/src/hook.rs`'s
//! `parse_absorbs_any_event_and_rejects_empty`). When that happens the
//! outer `if let Some(input) = …` body never runs, so `scan` prints
//! NOTHING and exits 0 via `run_hook`. That is silent fail-open: a
//! genuinely-undecidable payload (we could not even tell which tool was
//! called, let alone scan its response) is indistinguishable on stdout
//! from "checked, nothing found" — exactly the failure mode `gate.rs`'s
//! own module docs single out for the *parseable-but-unscannable*
//! `tool_response` case (`Extraction::Undecidable` -> a warning, "Failing
//! closed"), just one layer up: an unparseable *payload*.
//!
//! The done criteria for the fix this file pins: a non-empty, unparseable
//! stdin payload must still make `scan` print a single-line
//! `hookSpecificOutput` PostToolUse warning containing "UNTRUSTED DATA",
//! with exit code 0 (PostToolUse warn-only hooks communicate via stdout
//! JSON + exit 0, never a non-zero code).
//!
//! The broken-payload tests below are EXPECTED TO FAIL against the current
//! binary (that is the bug). The control test
//! (`valid_benign_webfetch_payload_stays_silent`) must PASS now, so this
//! file cannot be satisfied by a binary that just warns unconditionally on
//! every invocation.

use std::io::Write;
use std::process::{Command, Stdio};

#[derive(Debug)]
struct Run {
    code: i32,
    stdout: String,
    #[allow(dead_code)]
    stderr: String,
}

/// Spawn the real `fetchguard scan` binary, feed it `stdin`, collect its
/// output. No shell/launcher indirection — this exercises `main.rs`
/// directly.
fn run_scan(stdin: &str) -> Run {
    let mut child = Command::new(env!("CARGO_BIN_EXE_fetchguard"))
        .arg("scan")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn fetchguard scan");
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(stdin.as_bytes())
        .expect("write stdin");
    let out = child.wait_with_output().expect("wait for fetchguard scan");
    Run {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

/// Assert `run`'s stdout is exactly one line of valid JSON shaped as a
/// PostToolUse `additionalContext` warning naming the content untrusted,
/// and that the process exited 0.
fn assert_untrusted_data_warning(run: &Run) {
    assert_eq!(
        run.code, 0,
        "PostToolUse warn-only hooks must exit 0 (stdout JSON is the channel), got {}; stderr: {}",
        run.code, run.stderr
    );
    let line = run.stdout.trim();
    assert!(
        !line.is_empty(),
        "an unparseable-but-non-empty stdin payload must not be silent (fail-closed, not fail-open); stdout was empty, stderr: {}",
        run.stderr
    );
    assert_eq!(
        line.lines().count(),
        1,
        "warning must be a single line, got: {line:?}"
    );
    let v: serde_json::Value = serde_json::from_str(line)
        .unwrap_or_else(|e| panic!("stdout must be valid JSON: {e}; stdout was: {line:?}"));
    assert_eq!(
        v["hookSpecificOutput"]["hookEventName"], "PostToolUse",
        "warning JSON: {v}"
    );
    let ctx = v["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap_or_else(|| panic!("additionalContext must be a string, got: {v}"));
    assert!(
        ctx.contains("UNTRUSTED DATA"),
        "additionalContext must use the crate's existing fail-closed vocabulary (\"UNTRUSTED DATA\"): {ctx}"
    );
}

#[test]
fn malformed_json_payload_still_warns() {
    let run = run_scan("{not json");
    assert_untrusted_data_warning(&run);
}

#[test]
fn truncated_json_payload_still_warns() {
    let run = run_scan(r#"{"tool_name":"WebFetch","tool_response":"#);
    assert_untrusted_data_warning(&run);
}

#[test]
fn plain_garbage_text_payload_still_warns() {
    let run = run_scan("this is not json at all, just garbage text\nwith a second line");
    assert_untrusted_data_warning(&run);
}

/// Control: a VALID, benign `WebFetch` payload must still produce empty
/// stdout. Without this test, a binary that warns unconditionally on every
/// invocation (regardless of whether it could parse or scan anything) would
/// pass the three tests above for the wrong reason.
#[test]
fn valid_benign_webfetch_payload_stays_silent() {
    let payload = serde_json::json!({
        "tool_name": "WebFetch",
        "tool_response": "The weather is sunny.",
    })
    .to_string();
    let run = run_scan(&payload);
    assert_eq!(
        run.code, 0,
        "hook must exit 0 even on the clean path; stderr: {}",
        run.stderr
    );
    assert_eq!(
        run.stdout.trim(),
        "",
        "a valid, benign payload must stay silent (no warning) — got: {}",
        run.stdout
    );
}
