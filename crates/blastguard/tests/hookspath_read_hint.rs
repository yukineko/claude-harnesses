// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Regression tests for item 326edfee.
//!
//! Spec: `git config core.hooksPath` with no value must stay DENIED exactly as
//! today — nothing about the VERDICT changes. Only the deny REASON changes: it
//! must now tell the caller how to read the value safely
//! (`git config --get core.hooksPath`), because the bare one-argument form is
//! ambiguous between "I want to read this" and "I want to write this" and the
//! gate cannot tell which the caller meant from the command line alone.
//!
//! These are end-to-end tests against the real built binary, following the
//! pattern in `tests/integration.rs`: a PreToolUse Bash JSON payload on stdin,
//! `CLAUDE_CODE_SESSION_ID` removed so the repeat-refusal downgrade
//! (`tests/repeat_downgrade.rs`) cannot fire and turn an expected `deny` into
//! an `ask` on a second identical command within one `cargo test` run.

use std::io::Write;
use std::process::{Command, Stdio};

/// Run the binary with a Bash `command` on stdin. Returns
/// `(permissionDecision, permissionDecisionReason)`, where an empty decision
/// means the hook printed nothing (silent allow).
fn verdict(command: &str) -> (String, String) {
    let bin = env!("CARGO_BIN_EXE_blastguard");
    let payload = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": { "command": command },
    })
    .to_string();
    let mut child = Command::new(bin)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Test isolation for the repeat ledger — see tests/integration.rs's
        // `run()` for the full rationale. None of the commands in this file
        // repeat verbatim, but this keeps the suite robust to reordering and
        // to whoever's environment `cargo test` inherits.
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .spawn()
        .expect("binary spawns");
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(payload.as_bytes());
    }
    let out = child.wait_with_output().expect("binary runs");
    assert_eq!(
        out.status.code(),
        Some(0),
        "hook must always exit 0 (cmd: {command})"
    );
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if stdout.trim().is_empty() {
        return (String::new(), String::new());
    }
    let doc: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("stdout is not JSON for `{command}` ({e}): {stdout}"));
    let decision = doc
        .get("hookSpecificOutput")
        .and_then(|h| h.get("permissionDecision"))
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("no permissionDecision for `{command}`, got: {stdout}"))
        .to_string();
    let reason = doc
        .get("hookSpecificOutput")
        .and_then(|h| h.get("permissionDecisionReason"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    (decision, reason)
}

const HOOKSPATH_CLASS_REASON: &str =
    "git config core.hooksPath repoints every git hook at once, disabling the repo's hook gates";

/// EXPECTED RED TODAY. The bare read/write-ambiguous form must stay denied
/// (verdict unchanged), but the reason must now steer the caller to the safe
/// read spelling. Today's reason carries the class explanation but not the
/// `git config --get core.hooksPath` hint.
#[test]
fn bare_hookspath_deny_reason_points_to_the_safe_read_form() {
    let cmd = "git config core.hooksPath";
    let (decision, reason) = verdict(cmd);
    assert_eq!(
        decision, "deny",
        "`{cmd}` must stay denied — the verdict itself does not change, got reason: {reason}"
    );
    assert!(
        reason.contains("git config --get core.hooksPath"),
        "`{cmd}`'s deny reason must hint the safe read spelling `git config --get \
core.hooksPath`, got reason: {reason:?}"
    );
}

/// Every deny in this class — including the bare read/write-ambiguous form —
/// must still carry the shared class reason, since `rule_id.rs` maps rule ids
/// by that exact substring (`rule_id.rs:225`).
#[test]
fn every_hookspath_deny_still_carries_the_shared_class_reason() {
    let cases = [
        "git config core.hooksPath",
        "git config core.hooksPath .githooks",
        "git -c core.hooksPath=/tmp/evil status",
        "git config --unset core.hooksPath",
    ];
    for cmd in cases {
        let (decision, reason) = verdict(cmd);
        assert_eq!(
            decision, "deny",
            "`{cmd}` must be denied, got reason: {reason}"
        );
        assert!(
            reason.contains(HOOKSPATH_CLASS_REASON),
            "`{cmd}`'s deny reason must still contain the shared class reason \
`{HOOKSPATH_CLASS_REASON}`, got: {reason:?}"
        );
    }
}

/// Non-regression: the three non-bare deny shapes stay denied (verdict
/// unmodified by the read-hint change).
#[test]
fn write_and_repoint_shapes_stay_denied() {
    let cases = [
        "git config core.hooksPath .githooks",
        "git -c core.hooksPath=/tmp/evil status",
        "git config --unset core.hooksPath",
    ];
    for cmd in cases {
        let (decision, reason) = verdict(cmd);
        assert_eq!(
            decision, "deny",
            "`{cmd}` must be denied, got reason: {reason}"
        );
    }
}

/// Non-regression: the read-only form stays allowed (silent, no `ask`/`deny`).
#[test]
fn get_form_stays_read_only_allowed() {
    let cmd = "git config --get core.hooksPath";
    let (decision, reason) = verdict(cmd);
    assert!(
        decision != "deny" && decision != "ask",
        "`{cmd}` is a read-only form and must not be deny/ask, got decision={decision:?} reason={reason:?}"
    );
}
