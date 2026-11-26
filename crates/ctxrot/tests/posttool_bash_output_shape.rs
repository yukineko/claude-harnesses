//! RED(posttool-shape): `ctxrot toolguard` must preserve the Bash
//! `tool_response` shape when it emits `updatedToolOutput`.
//!
//! Claude Code observed: "PostToolUse hook returned updatedToolOutput that does
//! not match Bash's output shape; using original output". Bash's
//! `tool_response` is an object `{stdout, stderr, interrupted, isImage}`; a bare
//! JSON string in `updatedToolOutput` is rejected (the replacement is lost).
//! Suspected cause: `src/main.rs` Toolguard arm
//! `"updatedToolOutput" => serde_json::Value::String(updated)`.
//!
//! Runs the real binary over stdin with an isolated HOME / cwd.

use std::io::Write as _;
use std::process::{Command, Stdio};

fn run_toolguard(tool_response: serde_json::Value) -> String {
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
    let mut c = Command::new(env!("CARGO_BIN_EXE_ctxrot"));
    harness_core::git_env::scrub(&mut c);
    let mut child = c
        .arg("toolguard")
        .env("HOME", home.path())
        .env_remove("GUARD_DISABLE")
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
    assert_eq!(out.status.code(), Some(0), "toolguard must exit 0: {out:?}");
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

/// Above the default `huge_tool_output_bytes` (50_000).
fn huge() -> String {
    "x".repeat(200_000)
}

/// Control: a small Bash output must not be replaced.
#[test]
fn small_bash_output_emits_no_updated_tool_output() {
    let out = run_toolguard(bash_response("ok\n".to_string()));
    assert!(
        updated_tool_output(&out).is_none(),
        "small output must not be replaced: {out}"
    );
}

/// Precondition: the huge payload actually triggers a replacement (so the shape
/// assert below is not vacuous).
#[test]
fn huge_bash_output_is_replaced() {
    let out = run_toolguard(bash_response(huge()));
    assert!(
        updated_tool_output(&out).is_some(),
        "huge Bash output must be replaced: {out}"
    );
}

#[test]
#[ignore = "RED(79a6f4be): toolguard emits updatedToolOutput as a bare string for an object-shaped Bash tool_response (main.rs Toolguard Value::String(updated))"]
fn huge_bash_output_updated_tool_output_keeps_bash_shape() {
    let input = bash_response(huge());
    let out = run_toolguard(input.clone());
    let updated = updated_tool_output(&out)
        .unwrap_or_else(|| panic!("precondition: huge output must be replaced: {out}"));
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
