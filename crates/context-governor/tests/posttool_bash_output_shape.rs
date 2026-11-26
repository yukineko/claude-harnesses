//! RED(posttool-shape): the PostToolUse groomer must preserve the Bash
//! `tool_response` shape when it emits `updatedToolOutput`.
//!
//! Claude Code observed: "PostToolUse hook returned updatedToolOutput that does
//! not match Bash's output shape; using original output". Bash's
//! `tool_response` is an object `{stdout, stderr, interrupted, isImage}`; a bare
//! JSON string in `updatedToolOutput` is rejected (the groom is silently lost).
//! Suspected cause: `src/defaults/groomer.rs` `Some(serde_json::Value::String(trimmed))`.
//!
//! Runs the real binary over stdin in an isolated state dir / HOME / cwd.

use std::io::Write as _;
use std::process::{Command, Stdio};

fn run_hook(tool_response: serde_json::Value) -> String {
    let state = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let payload = serde_json::json!({
        "session_id": "posttool-shape",
        "transcript_path": "",
        "cwd": cwd.path(),
        "hook_event_name": "PostToolUse",
        "tool_name": "Bash",
        "tool_input": { "command": "cat big.log" },
        "tool_response": tool_response,
    });
    let mut child = Command::new(env!("CARGO_BIN_EXE_context-governor"))
        .env("CONTEXT_GOVERNOR_STATE_DIR", state.path())
        .env("HOME", home.path())
        .env("CLAUDE_CODE_SESSION_ID", "posttool-shape")
        .env_remove("CONTEXT_GOVERNOR_GROOM_BUDGET")
        .env_remove("CONTEXT_GOVERNOR_REFERENCE_DOC")
        .current_dir(cwd.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.to_string().as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(0), "hook must exit 0: {out:?}");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn bash_response(stdout: String) -> serde_json::Value {
    serde_json::json!({
        "stdout": stdout,
        "stderr": "",
        "interrupted": false,
        "isImage": false,
    })
}

fn updated_tool_output(stdout: &str) -> Option<serde_json::Value> {
    if stdout.is_empty() {
        return None;
    }
    let v: serde_json::Value =
        serde_json::from_str(stdout).unwrap_or_else(|e| panic!("non-JSON stdout {e}: {stdout}"));
    v.get("hookSpecificOutput")
        .and_then(|h| h.get("updatedToolOutput"))
        .cloned()
}

fn keys(v: &serde_json::Value) -> std::collections::BTreeSet<String> {
    v.as_object()
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default()
}

/// Control: a small Bash output is under budget and must not be rewritten.
#[test]
fn small_bash_output_emits_no_updated_tool_output() {
    let out = run_hook(bash_response("ok\n".to_string()));
    assert!(
        updated_tool_output(&out).is_none(),
        "small output must not be rewritten: {out}"
    );
}

/// Precondition: the oversized payload actually triggers a groom (so the shape
/// assert below is not vacuous).
#[test]
fn oversized_bash_output_is_groomed() {
    let big = "line of build output that is fairly long\n".repeat(4000);
    let out = run_hook(bash_response(big));
    assert!(
        updated_tool_output(&out).is_some(),
        "oversized Bash output must be groomed: {out}"
    );
}

#[test]
#[ignore = "RED(posttool-shape): groomer emits updatedToolOutput as a bare string for an object-shaped Bash tool_response (groomer.rs Value::String(trimmed))"]
fn oversized_bash_output_updated_tool_output_keeps_bash_shape() {
    let big = "line of build output that is fairly long\n".repeat(4000);
    let input = bash_response(big);
    let out = run_hook(input.clone());
    let updated = updated_tool_output(&out)
        .unwrap_or_else(|| panic!("precondition: oversized output must be groomed: {out}"));
    assert!(
        updated.is_object(),
        "updatedToolOutput must be an object matching Bash's output shape, got: {}",
        updated.to_string().chars().take(120).collect::<String>()
    );
    assert_eq!(keys(&updated), keys(&input), "key set must match the input");
    assert!(
        updated["stdout"].is_string(),
        "stdout must be a string: {updated}"
    );
}
