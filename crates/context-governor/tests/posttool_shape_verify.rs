//! Adversarial verification of 8cc17e1fc (`groom_object`). The groomer only
//! trims `stdout`/`stderr`; every other object-shaped `tool_response` in the
//! hooks.json matcher (Read|Bash|Grep|Glob|WebFetch|BashOutput|NotebookRead|Task)
//! hits `total == 0 -> None` and is silently left ungroomed (no rewrite, no
//! ledger row, no note). WebFetch's text field is `result` (the same key
//! ctxrot's `TEXT_KEYS` treats as text), so an oversized WebFetch result is
//! never groomed.
//!
//! Runs the real binary over stdin in an isolated state dir / HOME / cwd.

use std::io::Write as _;
use std::process::{Command, Stdio};

fn run_hook(tool_name: &str, tool_response: serde_json::Value) -> String {
    let state = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let payload = serde_json::json!({
        "session_id": "posttool-shape-verify",
        "transcript_path": "",
        "cwd": cwd.path(),
        "hook_event_name": "PostToolUse",
        "tool_name": tool_name,
        "tool_input": { "url": "https://example.invalid/", "prompt": "summarize" },
        "tool_response": tool_response,
    });
    let mut child = Command::new(env!("CARGO_BIN_EXE_context-governor"))
        .env("CONTEXT_GOVERNOR_STATE_DIR", state.path())
        .env("HOME", home.path())
        .env("CLAUDE_CODE_SESSION_ID", "posttool-shape-verify")
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

/// An oversized WebFetch `result` (~40k tokens, far above the 2048 default
/// budget) must be groomed with the WebFetch object shape kept.
#[test]
fn oversized_webfetch_result_is_groomed_with_shape_kept() {
    let big = "line of fetched page text that is fairly long\n".repeat(4000);
    let input = serde_json::json!({
        "bytes": big.len(),
        "code": 200,
        "codeText": "OK",
        "result": big,
        "durationMs": 120,
        "url": "https://example.invalid/",
    });
    let out = run_hook("WebFetch", input.clone());
    let v = updated_tool_output(&out)
        .unwrap_or_else(|| panic!("oversized WebFetch result was not groomed: {out:?}"));
    assert!(v.is_object(), "shape must stay an object: {v}");
    let r = v
        .get("result")
        .and_then(|x| x.as_str())
        .expect("result str");
    assert!(
        r.chars().count() < big.chars().count(),
        "result not trimmed"
    );
}
