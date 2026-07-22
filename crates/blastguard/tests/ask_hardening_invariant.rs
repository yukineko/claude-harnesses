// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used)]
//! End-to-end invariant: the Ask-hardening rule
//! (`crate::model::Decision::hardened`, wired in `main.rs` behind
//! `interactive::ask_available`) still resolves exactly the same way after the
//! `harness_core::verdict` migration (t1) touched `diffrisk.rs`/`classify.rs`.
//! `main.rs`/`model.rs`/`interactive.rs` were NOT touched by that migration,
//! so this is a regression pin, not a new-behavior test: it proves the
//! Ask-vs-Deny wiring downstream of the type-contract change is unchanged.
//!
//! This spawns the real built binary (same convention as `tests/integration.rs`)
//! rather than calling `interactive::resolve` directly, because `resolve` is
//! private and because the thing actually at risk is the WIRING between
//! `ask_available()` and `Decision::hardened()` in `main.rs`, not either
//! function in isolation (both are already unit-tested).
//!
//! `is_blocking()` is true for both `Ask` and `Deny`, so a test that only
//! checked "did it block" would not observe the hardening rule at all. Every
//! assertion here reads the `permissionDecision` STRING itself (`"ask"` vs
//! `"deny"`).
//!
//! Env hygiene: the child's environment is fully cleared
//! (`Command::env_clear`) and only the variables the test cares about are set
//! explicitly. Without this, a stray `CLAUDECODE`/`CLAUDE_CODE_ENTRYPOINT`
//! leaking in from the process actually running this test suite (which is
//! itself very possibly a Claude Code hook invocation) would make the result
//! depend on the ambient run environment instead of on the scenario under
//! test -- i.e. prove nothing.

use std::io::Write;
use std::process::{Command, Stdio};

/// A Bash command guaranteed to produce `Decision::Ask` (never `Allow` or an
/// unconditional `Deny`): an unrecognised wrapper head in front of a line that
/// parses as destructive. See `detect::unknown_wrapper_ask_covers_every_unrecognized_head`
/// in `src/detect.rs`, which pins the same shape.
const ASK_PRODUCING_COMMAND: &str = "my-cleanup-wrapper rm -rf /some/path";

fn ask_payload() -> String {
    format!(
        r#"{{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{{"command":"{ASK_PRODUCING_COMMAND}"}}}}"#
    )
}

/// Run the binary with a fully-controlled environment: cleared, then only
/// `envs` set. Returns the parsed `permissionDecision` field (panics if the
/// output isn't the expected single-line hook JSON -- every scenario here is
/// constructed to produce exactly one).
fn permission_decision_with_env(envs: &[(&str, &str)]) -> String {
    let bin = env!("CARGO_BIN_EXE_blastguard");
    let mut cmd = Command::new(bin);
    cmd.env_clear();
    for (k, v) in envs {
        cmd.env(k, v);
    }
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("binary spawns");
    if let Some(mut child_stdin) = child.stdin.take() {
        child_stdin
            .write_all(ask_payload().as_bytes())
            .expect("write payload to stdin");
    }
    let out = child.wait_with_output().expect("binary runs");
    assert_eq!(
        out.status.code(),
        Some(0),
        "hook must always exit 0 regardless of the decision"
    );
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let v: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("expected single-line hook JSON, got {stdout:?}: {e}"));
    v["hookSpecificOutput"]["permissionDecision"]
        .as_str()
        .unwrap_or_else(|| panic!("no permissionDecision field in {v}"))
        .to_string()
}

#[test]
fn interactive_cli_session_stays_ask() {
    // CLAUDECODE=1 AND CLAUDE_CODE_ENTRYPOINT=cli is the one measured
    // affirmatively-interactive shape (see `interactive.rs` module docs): a
    // human is presumed present, so the Ask must reach them unhardened.
    let decision =
        permission_decision_with_env(&[("CLAUDECODE", "1"), ("CLAUDE_CODE_ENTRYPOINT", "cli")]);
    assert_eq!(
        decision, "ask",
        "an interactive cli session must receive the raw ask, not a hardened deny"
    );
}

#[test]
fn headless_sdk_entrypoint_hardens_to_deny() {
    // CLAUDECODE=1 is present (this IS Claude Code) but the entrypoint is the
    // measured headless shape (`claude -p`) -- nobody can answer an ask here,
    // so it must harden.
    let decision =
        permission_decision_with_env(&[("CLAUDECODE", "1"), ("CLAUDE_CODE_ENTRYPOINT", "sdk-cli")]);
    assert_eq!(
        decision, "deny",
        "a headless (non-cli) entrypoint must harden the ask to a deny"
    );
}

#[test]
fn missing_entrypoint_signal_hardens_to_deny() {
    // Bare shell / cron / condukt worker with neither var set: no positive
    // proof of a human, so it must resolve to the restricted side.
    let decision = permission_decision_with_env(&[]);
    assert_eq!(
        decision, "deny",
        "a missing interactivity signal must harden the ask to a deny"
    );
}

#[test]
fn blastguard_ask_never_hardens_to_deny_even_when_interactive() {
    // The operator override must win even over an environment that would
    // otherwise be treated as interactive.
    let decision = permission_decision_with_env(&[
        ("CLAUDECODE", "1"),
        ("CLAUDE_CODE_ENTRYPOINT", "cli"),
        ("BLASTGUARD_ASK", "never"),
    ]);
    assert_eq!(
        decision, "deny",
        "BLASTGUARD_ASK=never must harden the ask to a deny regardless of the environment"
    );
}

#[test]
fn blastguard_ask_unrecognised_value_hardens_to_deny_not_to_asking() {
    // An unrecognised override value is an unknown, and unknown must resolve
    // to the safe side (never to "ask" and never to "allow") -- this is the
    // scenario that would most easily regress into an optimistic default.
    let decision = permission_decision_with_env(&[
        ("CLAUDECODE", "1"),
        ("CLAUDE_CODE_ENTRYPOINT", "cli"),
        ("BLASTGUARD_ASK", "yes-please"),
    ]);
    assert_eq!(
        decision, "deny",
        "an unrecognised BLASTGUARD_ASK value must harden to deny, not resolve to ask"
    );
}
