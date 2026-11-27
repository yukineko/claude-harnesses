//! Third adversarial verification (d0e467ee5, recursive `harness_core::shrink`)
//! through the real context-governor binary in an isolated state dir / HOME /
//! cwd. Default budget 2048 tokens = 8192 chars on the compact serialization.

use std::io::Write as _;
use std::process::{Command, Stdio};

/// Default groom budget (tokens) at zero window pressure.
const BUDGET: usize = 2048;

fn run_raw(payload: &str) -> String {
    let state = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_context-governor"))
        .env("CONTEXT_GOVERNOR_STATE_DIR", state.path())
        .env("HOME", home.path())
        .env("CLAUDE_CODE_SESSION_ID", "posttool-shape-verify3")
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
        .write_all(payload.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(0), "hook must exit 0: {out:?}");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn run_hook(tool_name: &str, tool_response: serde_json::Value) -> String {
    let payload = serde_json::json!({
        "session_id": "posttool-shape-verify3",
        "transcript_path": "",
        "cwd": "/tmp",
        "hook_event_name": "PostToolUse",
        "tool_name": tool_name,
        "tool_input": {},
        "tool_response": tool_response,
    });
    run_raw(&payload.to_string())
}

fn hso(stdout: &str) -> Option<serde_json::Value> {
    if stdout.is_empty() {
        return None;
    }
    let v: serde_json::Value =
        serde_json::from_str(stdout).unwrap_or_else(|e| panic!("non-JSON stdout {e}: {stdout}"));
    v.get("hookSpecificOutput").cloned()
}

fn updated(stdout: &str) -> Option<serde_json::Value> {
    hso(stdout).and_then(|h| h.get("updatedToolOutput").cloned())
}

fn within_budget(v: &serde_json::Value) -> bool {
    v.to_string().chars().count().div_ceil(4) <= BUDGET * 11 / 10
}

/// Escape-heavy stdout (progress-bar `ESC[K` + `\r`) groomed to "" with no
/// marker.
#[test]
fn escape_heavy_stdout_not_emptied() {
    let input = serde_json::json!({
        "stdout": "\u{1b}[K\r".repeat(20_000),
        "stderr": "",
        "interrupted": false,
        "isImage": false,
    });
    let out = run_hook("Bash", input);
    let v = updated(&out).unwrap_or_else(|| panic!("over budget, no groom: {out}"));
    let s = v["stdout"].as_str().expect("stdout str");
    assert!(!s.is_empty(), "stdout emptied without any marker");
}

/// Payload nested deeper than serde_json's default recursion limit (128): the
/// hook must not stay silent about a ~50k-token output.
#[test]
fn deeply_nested_response_is_not_silent() {
    let depth = 140;
    let inner = serde_json::Value::String("fetched text line\n".repeat(11_000)).to_string();
    let resp = format!(
        "{}{{\"result\":{inner}}}{}",
        "[".repeat(depth),
        "]".repeat(depth)
    );
    let payload = format!(
        "{{\"session_id\":\"v3\",\"transcript_path\":\"\",\"cwd\":\"/tmp\",\
         \"hook_event_name\":\"PostToolUse\",\"tool_name\":\"WebFetch\",\"tool_input\":{{}},\
         \"tool_response\":{resp}}}"
    );
    let out = run_raw(&payload);
    assert!(
        hso(&out).is_some(),
        "nested oversized output produced no groom and no note: {out:?}"
    );
}

/// Array of 3000 small `{type,text}` objects: items are never elided, so the
/// groomed output stays far over budget.
#[test]
fn array_of_objects_within_budget() {
    let items: Vec<serde_json::Value> = (0..3000)
        .map(|i| serde_json::json!({"type": "text", "text": format!("match line {i:05} in some file")}))
        .collect();
    let out = run_hook("Grep", serde_json::Value::Array(items));
    let v = updated(&out).unwrap_or_else(|| panic!("no groom: {out}"));
    assert!(
        within_budget(&v),
        "groomed {} chars",
        v.to_string().chars().count()
    );
}

/// Control: nested Read `file.content` is groomed within budget, scalars kept.
#[test]
fn control_nested_read_groomed_scalars_kept() {
    let input = serde_json::json!({
        "type": "text",
        "file": {
            "filePath": "/tmp/x.rs",
            "content": "line of file text that is fairly long here\n".repeat(4000),
            "numLines": 4000,
            "startLine": 1,
            "totalLines": 4000,
        }
    });
    let out = run_hook("Read", input.clone());
    let v = updated(&out).unwrap_or_else(|| panic!("no groom: {out}"));
    assert!(within_budget(&v));
    for k in ["numLines", "startLine", "totalLines", "filePath"] {
        assert_eq!(v["file"][k], input["file"][k], "{k} changed");
    }
}
