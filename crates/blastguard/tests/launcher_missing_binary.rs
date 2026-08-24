//! `bin/blastguard` (the POSIX launcher) — what happens when NO per-platform
//! binary is bundled for the host.
//!
//! blastguard has no gate/mark/clear split (unlike ctxrot): it is a
//! single PreToolUse entrypoint that always either stays silent (Allow) or
//! prints one line of hook JSON (Deny/Ask). A missing binary is therefore
//! ALWAYS a missing verdict, and the old launcher printed nothing and exited
//! 0 — byte-for-byte indistinguishable from a real Allow (backlog 2ec9d740).
//! The fix mirrors `crate::interactive::ask_available`'s positive-interactive
//! check so the launcher's fallback obeys the exact same policy the compiled
//! binary does: ask only when affirmatively interactive, deny otherwise
//! (never a silent allow either way).

// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::Write;
use std::process::{Command, Stdio};

#[derive(Debug)]
struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

/// Run the real launcher from a directory that provably contains no
/// `blastguard-<os>-<arch>` sibling, so the missing-binary path is taken on
/// every platform (the repo's own `bin/` may hold a built binary once this
/// task's rollout has run). `envs` fully controls the interactive-detection
/// env vars: unset ones are removed from the child's environment entirely
/// (not just inherited from this test process), so the test is not at the
/// mercy of whatever happens to run it.
fn run(envs: &[(&str, &str)]) -> Run {
    let dir = tempfile::tempdir().expect("tempdir");
    let launcher = dir.path().join("blastguard");
    std::fs::copy(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("bin")
            .join("blastguard"),
        &launcher,
    )
    .expect("copy launcher");

    let mut cmd = Command::new("sh");
    cmd.arg(&launcher)
        .env_remove("BLASTGUARD_ASK")
        .env_remove("CLAUDECODE")
        .env_remove("CLAUDE_CODE_ENTRYPOINT")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().expect("spawn sh");
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(br#"{"tool_name":"Bash","tool_input":{"command":"rm -rf /"}}"#)
        .expect("write stdin");
    let out = child.wait_with_output().expect("wait");
    Run {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

fn parse_decision(stdout: &str) -> serde_json::Value {
    serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("stdout must be hook JSON ({e}): {stdout}"))
}

#[test]
fn missing_binary_never_prints_nothing() {
    // Non-interactive env (no CLAUDECODE/CLAUDE_CODE_ENTRYPOINT at all).
    let r = run(&[]);
    assert_eq!(r.code, 0, "PreToolUse decisions ride stdout, not exit code");
    assert!(
        !r.stdout.trim().is_empty(),
        "silence IS the allow — the launcher must emit a decision: {r:?}",
    );
    assert!(
        r.stderr.contains("blastguard"),
        "the missing-binary condition itself must also be logged, not just the verdict: {r:?}",
    );
}

#[test]
fn missing_binary_in_non_interactive_context_denies() {
    let r = run(&[("CLAUDECODE", "1"), ("CLAUDE_CODE_ENTRYPOINT", "sdk-cli")]);
    let v = parse_decision(&r.stdout);
    assert_eq!(
        v["hookSpecificOutput"]["hookEventName"], "PreToolUse",
        "{v}"
    );
    assert_eq!(
        v["hookSpecificOutput"]["permissionDecision"], "deny",
        "no human is present to answer an ask in a headless/sdk context, so a \
         missing verdict must harden to deny, not ask or allow: {v}",
    );
    let reason = v["hookSpecificOutput"]["permissionDecisionReason"]
        .as_str()
        .unwrap_or_default();
    assert!(
        reason.contains("blastguard"),
        "the reason is the only steering channel; name the cause: {reason}",
    );
}

#[test]
fn missing_binary_in_interactive_cli_context_asks() {
    let r = run(&[("CLAUDECODE", "1"), ("CLAUDE_CODE_ENTRYPOINT", "cli")]);
    let v = parse_decision(&r.stdout);
    assert_eq!(
        v["hookSpecificOutput"]["permissionDecision"], "ask",
        "a human is present in an interactive cli session, so put the call to \
         them instead of a silent allow or an unanswerable deny: {v}",
    );
}

#[test]
fn blastguard_ask_never_override_forces_deny_even_when_interactive() {
    let r = run(&[
        ("BLASTGUARD_ASK", "never"),
        ("CLAUDECODE", "1"),
        ("CLAUDE_CODE_ENTRYPOINT", "cli"),
    ]);
    let v = parse_decision(&r.stdout);
    assert_eq!(v["hookSpecificOutput"]["permissionDecision"], "deny", "{v}");
}

#[test]
fn blastguard_ask_always_override_forces_ask_even_when_non_interactive() {
    let r = run(&[("BLASTGUARD_ASK", "always")]);
    let v = parse_decision(&r.stdout);
    assert_eq!(v["hookSpecificOutput"]["permissionDecision"], "ask", "{v}");
}
