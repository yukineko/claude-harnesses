// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED for two false positives observed 2026-11-14 in subagents of condukt run
//! run-20261114-120314-34769 (session b3435a5e), measured with the installed
//! blastguard 0.2.568 and main 6d5c1627c. Nothing harmful is executed in either
//! command; both are QUOTED-delimiter heredocs, whose bodies the shell does not
//! expand.
//!
//! FP-A (task e787b168 worker, relay bgr-5dc49e87261a): `python3 - <<'EOF'`
//! whose Python program holds a backtick span `` `gh` `` inside a Python string
//! literal. Observed verbatim reason (main: ask, subagent: deny):
//!   "gh: no subcommand given — blastguard cannot tell what this does"
//! The backticks are Python text, not a shell command substitution.
//!
//! FP-B (task f70e5164 worker, blocks.jsonl ts=1794625871): a quoted heredoc
//! appending Rust source to a `.rs` file whose body has a Rust string literal
//! `"rm -rf ./a",`, FOLLOWED by a `cargo test` line. Observed verbatim reason
//! (deny in main mode too): "recursive rm (-r) can delete an entire directory
//! tree". Standing ruling (2026-11-08): a heredoc body written to a .rs target
//! is not read as shell lines. Without the trailing `cargo test` the same
//! command is Allow (control below), so the trailing command is what flips it.
//! Related but distinct content: backlog_20261113_fp_heredoc_rs_body_and_commit_msg_quotes
//! (FP1 python3 -c text, FP2b git commit text).
//!
//! CONTROLS (not ignored, pass at HEAD): the FP-A program without backticks is
//! Allow; the FP-B command without the trailing `cargo test` is Allow; an
//! UNQUOTED heredoc whose body has `` `rm -rf ~` `` stays Deny for both shapes
//! (the shell does expand it).
//!
//! DOES NOT PROVE: which stage misreads the body, subagent-mode verdicts (the
//! main-mode verdict is already wrong), or other interpreters / targets.

#[path = "support/bounded.rs"]
mod bounded;

use std::process::{Command, Stdio};

fn verdict(command: &str) -> (String, String) {
    let bin = env!("CARGO_BIN_EXE_blastguard");
    let payload = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": { "command": command },
    })
    .to_string();
    let mut hook = Command::new(bin);
    hook.env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = bounded::run_bounded(&mut hook, payload.as_bytes(), bounded::HOOK_DEADLINE);
    assert_eq!(out.status.code(), Some(0), "hook must exit 0 ({command:?})");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if stdout.trim().is_empty() {
        return ("allow".to_string(), String::new());
    }
    let doc: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("stdout not JSON for {command:?} ({e}): {stdout}"));
    let hso = doc.get("hookSpecificOutput");
    let d = hso
        .and_then(|h| h.get("permissionDecision"))
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("no permissionDecision for {command:?}: {stdout}"))
        .to_string();
    let r = hso
        .and_then(|h| h.get("permissionDecisionReason"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    (d, r)
}

const FP_A: &str = "cd /tmp/wt && python3 - <<'EOF'\n\
s='''// sync reads issue state through `gh`; a closed one is reopened\n\
'''\n\
print(s)\n\
EOF";

const FP_B: &str = "cd /tmp/wt && cat >> src/tests.rs <<'EOF'\n\
        \"rm -rf ./a\",\n\
EOF\n\
cargo test -p x --lib t";

#[test]
#[ignore = "RED: python3 - <<'EOF' program with a backtick span in a Python string is Ask (gh: no subcommand given) (2026-11-14) — remove the ignore when fixed"]
fn fp_a_quoted_python_heredoc_program_backticks_are_not_commands() {
    let (d, r) = verdict(FP_A);
    assert_eq!(d, "allow", "FP-A must be allow; reason: {r}");
}

#[test]
#[ignore = "RED: quoted .rs heredoc body with an rm -rf string literal followed by cargo test is Deny (2026-11-14) — remove the ignore when fixed"]
fn fp_b_quoted_rs_heredoc_body_then_cargo_test_is_not_shell() {
    let (d, r) = verdict(FP_B);
    assert_eq!(d, "allow", "FP-B must be allow; reason: {r}");
}

#[test]
fn control_fp_a_without_backticks_is_allowed() {
    let cmd = FP_A.replace("`gh`", "gh");
    let (d, r) = verdict(&cmd);
    assert_eq!(d, "allow", "{cmd:?}: {r}");
}

#[test]
fn control_fp_b_without_trailing_command_is_allowed() {
    let cmd = FP_B.trim_end_matches("\ncargo test -p x --lib t");
    assert!(cmd.ends_with("EOF"), "{cmd:?}");
    let (d, r) = verdict(cmd);
    assert_eq!(d, "allow", "{cmd:?}: {r}");
}

#[test]
fn control_unquoted_heredoc_backtick_rm_home_stays_denied() {
    let a = FP_A
        .replace("<<'EOF'", "<<EOF")
        .replace("`gh`", "`rm -rf ~`");
    let b = FP_B
        .replace("<<'EOF'", "<<EOF")
        .replace("\"rm -rf ./a\"", "\"`rm -rf ~`\"");
    for cmd in [a, b] {
        let (d, r) = verdict(&cmd);
        assert_eq!(d, "deny", "{cmd:?}: {r}");
    }
}
