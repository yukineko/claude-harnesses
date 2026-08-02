// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! The ENTRY boundary: what happens before the analyser ever sees a command.
//!
//! `detect.rs` applies the three-answer discipline thoroughly to the ANALYSIS —
//! 25-odd sub-analysers return `Ask` for constructs they cannot parse. The
//! front door did not. Two different situations both arrived as a silent
//! `Allow`:
//!
//!   * stdin was EMPTY — genuinely nothing to judge. Correct to stay silent.
//!   * stdin was NON-EMPTY and unparseable — a tool call is being made and
//!     blastguard could not read it. That is "failed to determine", and
//!     `main.rs`'s own module docstring claimed it was the first case.
//!
//! A silent exit 0 IS an allow (`main.rs:19-21`), so the second case allowed
//! precisely the call it had failed to read.
//!
//! These run the REAL binary over stdin. Headless (no TTY), so
//! `interactive::ask_available()` is false and an `Ask` hardens to `deny` in
//! the output — that hardening is itself the contract being relied on here.

use std::io::Write;
use std::process::{Command, Stdio};

/// Run the binary with its ask-availability PINNED, never inherited.
///
/// The first version of this file inherited the parent's environment and
/// asserted `deny`, on the assumption that a `cargo test` process is headless.
/// It is not: `cargo test` run from an interactive Claude Code session inherits
/// `CLAUDECODE=1` and `CLAUDE_CODE_ENTRYPOINT=cli`, so `interactive::ask_available`
/// answered TRUE and the `Ask` was never hardened. The tests failed — correctly,
/// and against the test rather than the fix.
///
/// A test whose verdict depends on who launched it is not an observation, so
/// the child's env is set explicitly here. `BLASTGUARD_ASK` is the operator
/// override documented in `interactive.rs` (`never` → force hardening, `always`
/// → force asking); it takes priority over the entrypoint sniffing, which is
/// what makes both branches reachable from one test binary without mutating
/// this process's own environment.
fn run_with_ask(payload: &str, ask: &str) -> (i32, String) {
    let bin = env!("CARGO_BIN_EXE_blastguard");
    let mut child = Command::new(bin)
        .env("BLASTGUARD_ASK", ask)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("binary spawns");
    if let Some(mut child_stdin) = child.stdin.take() {
        let _ = child_stdin.write_all(payload.as_bytes());
    }
    let out = child.wait_with_output().expect("binary runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

/// No human can answer → the refusal must harden to a `deny`, never relax to
/// silence. This is the branch that matters operationally: condukt workers,
/// cron and `claude -p` all land here.
fn run_headless(payload: &str) -> (i32, String) {
    run_with_ask(payload, "never")
}

/// A human IS present → the refusal stays an `ask`. Asserted alongside the
/// headless branch so a regression that collapsed the tri-state back to two
/// answers cannot pass by satisfying only one of them.
fn run_interactive(payload: &str) -> (i32, String) {
    run_with_ask(payload, "always")
}

fn decision_of(stdout: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).ok()?;
    v["hookSpecificOutput"]["permissionDecision"]
        .as_str()
        .map(|s| s.to_string())
}

/// Every payload that is non-empty but unreadable, in one place.
///
/// Each is asserted twice — once headless, once interactive — because the
/// property under test is "a refusal is EMITTED", and its polarity is the
/// separate hardening contract. Asserting only `deny` would let a regression
/// that dropped the hardening pass in an interactive session and fail in CI,
/// and asserting only "non-empty" would let one that emitted an `allow` pass.
const UNREADABLE_PAYLOADS: &[(&str, &str)] = &[
    // Truncated: the tool call is real, the read of it was not.
    (
        "truncated json",
        r#"{"tool_name":"Bash","tool_input":{"command":"rm -rf "#,
    ),
    ("not json at all", "this is not json"),
    // Valid JSON, wrong shape — schema drift rather than corruption.
    ("json of the wrong shape", "[1, 2, 3]"),
];

#[test]
fn non_empty_unparseable_stdin_is_not_a_silent_allow() {
    for (label, payload) in UNREADABLE_PAYLOADS {
        let (code, out) = run_headless(payload);
        assert_eq!(code, 0, "{label}: the turn must not break");
        assert!(
            !out.trim().is_empty(),
            "{label}: silence IS an allow; unparseable stdin must print a decision"
        );
        assert_eq!(
            decision_of(&out).as_deref(),
            Some("deny"),
            "{label} (headless): stdout: {out}"
        );
    }
}

#[test]
fn non_empty_unparseable_stdin_asks_when_a_human_can_answer() {
    for (label, payload) in UNREADABLE_PAYLOADS {
        let (code, out) = run_interactive(payload);
        assert_eq!(code, 0, "{label}: the turn must not break");
        assert_eq!(
            decision_of(&out).as_deref(),
            Some("ask"),
            "{label} (interactive): stdout: {out}"
        );
    }
}

/// ANTI-VACUITY CONTROL — EMPTY stdin stays a silent allow.
///
/// This is the case that genuinely determined there is nothing to judge, and it
/// is the one the module docstring was right about. If the fix were "print a
/// decision whenever parse returns None", this test fails — which is how it
/// proves the fix distinguishes the two situations instead of collapsing them
/// the other way.
#[test]
fn empty_stdin_stays_a_silent_allow() {
    for payload in ["", "   ", "\n\n"] {
        // Asserted in BOTH modes: silence here is not a hardening artefact.
        for (mode, out) in [
            ("headless", run_headless(payload)),
            ("interactive", run_interactive(payload)),
        ] {
            let (code, out) = out;
            assert_eq!(code, 0, "{mode}");
            assert!(
                out.trim().is_empty(),
                "{mode}: empty stdin is genuinely nothing to judge; got: {out}"
            );
        }
    }
}

/// ANTI-VACUITY CONTROL — a WELL-FORMED payload still gets its real verdict.
///
/// Proves the entry-boundary change did not turn the binary into a blanket
/// denier: an ordinary command still prints nothing, a destructive one still
/// denies.
#[test]
fn a_well_formed_payload_still_gets_its_real_verdict() {
    // An ordinary command allows even where a human COULD have been asked —
    // run interactively so a regression that asked about everything shows up
    // here rather than being masked by hardening.
    let (code, out) = run_interactive(r#"{"tool_name":"Bash","tool_input":{"command":"ls -la"}}"#);
    assert_eq!(code, 0);
    assert!(out.trim().is_empty(), "an ordinary command allows: {out}");

    // A genuinely destructive command is a DENY, not an ask, in both modes —
    // it was analysed and found guilty, which is a different answer from
    // "could not be analysed".
    for (mode, out) in [
        (
            "headless",
            run_headless(r#"{"tool_name":"Bash","tool_input":{"command":"rm -rf /"}}"#),
        ),
        (
            "interactive",
            run_interactive(r#"{"tool_name":"Bash","tool_input":{"command":"rm -rf /"}}"#),
        ),
    ] {
        let (code, out) = out;
        assert_eq!(code, 0, "{mode}");
        assert_eq!(
            decision_of(&out).as_deref(),
            Some("deny"),
            "{mode}: stdout: {out}"
        );
    }
}

/// A Bash payload whose `command` cannot be read must reach the same
/// conclusion end to end, not just in the unit test — this is the path an
/// actual schema drift would take.
#[test]
fn a_bash_payload_with_an_unreadable_command_refuses_end_to_end() {
    let payload = r#"{"tool_name":"Bash","tool_input":{"cmd":"rm -rf /"}}"#;

    let (code, out) = run_headless(payload);
    assert_eq!(code, 0);
    assert_eq!(
        decision_of(&out).as_deref(),
        Some("deny"),
        "headless: stdout: {out}"
    );

    let (code, out) = run_interactive(payload);
    assert_eq!(code, 0);
    assert_eq!(
        decision_of(&out).as_deref(),
        Some("ask"),
        "interactive: stdout: {out}"
    );
}
