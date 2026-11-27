//! Second adversarial verification of `groom_object` (419987f78). Probes
//! object-shaped `tool_response`s whose bulk is not a top-level string >= 256
//! chars: nested objects (Read `file.content`), arrays (Glob/Grep
//! `filenames`), and many short fields. Each over-budget input must be groomed,
//! with keys kept, to roughly the budget (10% tolerance, see the control).
//!
//! The Read / Glob shapes are Claude Code tool output shapes as known to the
//! verifier; they are not taken from fixtures in this repository.
//!
//! Runs the real binary over stdin in an isolated state dir / HOME / cwd.

use std::io::Write as _;
use std::process::{Command, Stdio};

/// Default groom budget (tokens) at zero window pressure.
const BUDGET: usize = 2048;

fn run_hook(tool_name: &str, tool_response: serde_json::Value) -> String {
    let state = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let payload = serde_json::json!({
        "session_id": "posttool-shape-verify2",
        "transcript_path": "",
        "cwd": cwd.path(),
        "hook_event_name": "PostToolUse",
        "tool_name": tool_name,
        "tool_input": {},
        "tool_response": tool_response,
    });
    let mut child = Command::new(env!("CARGO_BIN_EXE_context-governor"))
        .env("CONTEXT_GOVERNOR_STATE_DIR", state.path())
        .env("HOME", home.path())
        .env("CLAUDE_CODE_SESSION_ID", "posttool-shape-verify2")
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

/// ~4 chars/token, the groomer's own estimate, over the compact serialization.
fn est_tokens(v: &serde_json::Value) -> usize {
    v.to_string().chars().count().div_ceil(4)
}

/// Tolerance: the string path also overshoots by its marker, so allow 10%.
fn within_budget(v: &serde_json::Value) -> bool {
    est_tokens(v) <= BUDGET * 11 / 10
}

fn same_keys(a: &serde_json::Value, b: &serde_json::Value) -> bool {
    let mut x: Vec<_> = a.as_object().unwrap().keys().collect();
    let mut y: Vec<_> = b.as_object().unwrap().keys().collect();
    x.sort();
    y.sort();
    x == y
}

/// Read keeps its text in nested `file.content` (~40k tokens): must be groomed.
#[test]
fn read_nested_file_content_is_groomed() {
    let big = "line of file text that is fairly long here\n".repeat(4000);
    let input = serde_json::json!({
        "type": "text",
        "file": {
            "filePath": "/tmp/x.rs",
            "content": big,
            "numLines": 4000,
            "startLine": 1,
            "totalLines": 4000,
        }
    });
    let out = run_hook("Read", input.clone());
    let v = updated_tool_output(&out)
        .unwrap_or_else(|| panic!("oversized nested Read content not groomed: {out:?}"));
    assert!(same_keys(&v, &input), "keys changed: {v}");
    assert!(
        within_budget(&v),
        "groomed Read still {} tokens",
        est_tokens(&v)
    );
}

/// Glob `filenames` array of 5000 short paths (~40k tokens): bulk is in an
/// array of short strings, so `total == 0` and nothing is groomed.
#[test]
fn glob_filenames_array_is_groomed() {
    let names: Vec<String> = (0..5000)
        .map(|i| format!("/repo/src/module_{i:05}/some/deeper/path/file_{i:05}.rs"))
        .collect();
    let input = serde_json::json!({
        "filenames": names,
        "durationMs": 12,
        "numFiles": 5000,
        "truncated": false,
    });
    let out = run_hook("Glob", input.clone());
    let v = updated_tool_output(&out)
        .unwrap_or_else(|| panic!("oversized Glob filenames not groomed: {out:?}"));
    assert!(same_keys(&v, &input), "keys changed: {v}");
    assert!(
        within_budget(&v),
        "groomed Glob still {} tokens",
        est_tokens(&v)
    );
}

/// One 20k-char field plus 300 fields of 200 chars (each < 256, so never
/// trimmed): the rewrite is emitted (strictly smaller) but stays ~8x over budget.
#[test]
fn many_short_fields_rewrite_fits_budget() {
    let mut m = serde_json::Map::new();
    m.insert(
        "result".into(),
        serde_json::Value::String("bulk text line that is long\n".repeat(700)),
    );
    for i in 0..300 {
        m.insert(
            format!("f{i:03}"),
            serde_json::Value::String("s".repeat(200)),
        );
    }
    let input = serde_json::Value::Object(m);
    let out = run_hook("WebFetch", input.clone());
    let v = updated_tool_output(&out)
        .unwrap_or_else(|| panic!("over-budget object not groomed: {out:?}"));
    assert!(same_keys(&v, &input), "keys changed");
    assert!(
        within_budget(&v),
        "rewrite still {} tokens (budget {BUDGET})",
        est_tokens(&v)
    );
}

/// Control: the plain WebFetch shape is groomed within the tolerance, keys kept
/// (calibrates `within_budget` so the REDs above are not tolerance artefacts).
#[test]
fn control_webfetch_groomed_within_budget() {
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
    let v = updated_tool_output(&out).expect("groomed");
    assert!(same_keys(&v, &input));
    assert!(within_budget(&v), "control {} tokens", est_tokens(&v));
}
