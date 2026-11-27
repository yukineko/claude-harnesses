//! Adversarial verification of 8cc17e1fc (`updated_value` shape-preserving
//! rewrite). `updated_value` truncates EACH text field to `limit` on its own,
//! while the trigger (`response_text_len`) is the SUM of the text fields. So:
//!
//! - two fields each under `limit` but summing over it: the hook fires, emits
//!   an `updatedToolOutput`, and that rewrite is byte-identical to the input
//!   (no size reduction, no note);
//! - two fields each over `limit`: the rewrite is ~2x `limit`, i.e. above the
//!   threshold the hook exists to enforce.
//!
//! Runs the real binary over stdin with an isolated HOME / cwd.

use std::io::Write as _;
use std::process::{Command, Stdio};

/// Default `huge_tool_output_bytes`.
const LIMIT: usize = 50_000;

fn run_toolguard(tool_response: serde_json::Value) -> String {
    let home = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let payload = serde_json::json!({
        "session_id": "posttool-shape-verify",
        "transcript_path": "",
        "cwd": cwd.path(),
        "hook_event_name": "PostToolUse",
        "tool_name": "Bash",
        "tool_input": { "command": "make" },
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

/// Multi-line text of exactly `chars` characters.
fn lines(chars: usize) -> String {
    let line = "line of build output that is fairly long\n"; // 41 chars
    let mut s = line.repeat(chars / line.len() + 1);
    s.truncate(chars);
    s
}

fn text_len(v: &serde_json::Value) -> usize {
    ["stdout", "stderr"]
        .iter()
        .filter_map(|k| v.get(*k).and_then(|x| x.as_str()))
        .map(|s| s.chars().count())
        .sum()
}

fn bash(stdout: String, stderr: String) -> serde_json::Value {
    serde_json::json!({
        "stdout": stdout,
        "stderr": stderr,
        "interrupted": false,
        "isImage": false,
    })
}

/// stdout 30k + stderr 30k = 60k >= 50k threshold: the hook fires, so the
/// rewrite it emits must actually be smaller than the input.
#[test]
#[ignore = "RED(posttool-verify): updated_value truncates each text field to limit separately; 30k+30k fires the hook but the rewrite is identical (60000 >= 60000)"]
fn split_fields_each_under_limit_rewrite_actually_shrinks() {
    let input = bash(lines(30_000), lines(30_000));
    let out = run_toolguard(input.clone());
    let v = updated_tool_output(&out)
        .unwrap_or_else(|| panic!("60k combined text must trigger a rewrite: {out}"));
    assert!(
        text_len(&v) < text_len(&input),
        "rewrite emitted but text is not reduced: {} >= {} (no-op rewrite)",
        text_len(&v),
        text_len(&input)
    );
}

/// stdout 200k + stderr 200k: the rewrite must be bounded by the threshold
/// (string-shaped responses are bounded by `limit`; the object path must be too).
#[test]
#[ignore = "RED(posttool-verify): updated_value truncates each text field to limit separately; 200k+200k yields 99956 chars > 50000 limit"]
fn split_fields_each_over_limit_rewrite_stays_within_limit() {
    let input = bash(lines(200_000), lines(200_000));
    let out = run_toolguard(input);
    let v = updated_tool_output(&out)
        .unwrap_or_else(|| panic!("400k text must trigger a rewrite: {out}"));
    assert!(
        text_len(&v) <= LIMIT,
        "rewrite text {} exceeds the {LIMIT} limit",
        text_len(&v)
    );
}

/// Control: a single huge multi-line stdout is bounded by `limit` (so the two
/// tests above are not failing for an unrelated reason).
#[test]
fn control_single_field_over_limit_is_bounded() {
    let input = bash(lines(200_000), String::new());
    let out = run_toolguard(input);
    let v = updated_tool_output(&out).expect("rewrite");
    assert!(text_len(&v) <= LIMIT, "got {}", text_len(&v));
}
