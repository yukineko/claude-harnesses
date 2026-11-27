//! Second adversarial verification of the shape-preserving PostToolUse rewrite
//! (8cc17e1fc + 419987f78). Probes holes left after the proportional-budget
//! fix. Runs the real binary over stdin with an isolated HOME / cwd.
//!
//! The Read `tool_response` shape used below (`{type, file: {filePath, content,
//! numLines, startLine, totalLines}}`) is Claude Code's Read output shape as
//! known to the verifier; it is not taken from a fixture in this repository.

use std::io::Write as _;
use std::process::{Command, Stdio};

/// Default `huge_tool_output_bytes`.
const LIMIT: usize = 50_000;

fn run_toolguard(tool_name: &str, tool_response: serde_json::Value) -> String {
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

fn lines(chars: usize) -> String {
    let line = "line of build output that is fairly long\n"; // 41 chars
    let mut s = line.repeat(chars / line.len() + 1);
    s.truncate(chars);
    s
}

fn str_len(v: &serde_json::Value, k: &str) -> usize {
    v.get(k)
        .and_then(|x| x.as_str())
        .map(|s| s.chars().count())
        .unwrap_or(0)
}

/// Read output keeps its text in the NESTED `file.content`. An oversized Read
/// must be truncated (task requirement: works for Read content), with the
/// nested shape kept.
#[test]
#[ignore = "RED(posttool) nested Read file.content is never truncated: updated_value finds no top-level TEXT_KEYS and returns None"]
fn read_nested_file_content_is_truncated_with_shape_kept() {
    let big = lines(200_000);
    let input = serde_json::json!({
        "type": "text",
        "file": {
            "filePath": "/tmp/x.log",
            "content": big,
            "numLines": 4879,
            "startLine": 1,
            "totalLines": 4879,
        }
    });
    let out = run_toolguard("Read", input);
    let v = updated_tool_output(&out)
        .unwrap_or_else(|| panic!("oversized Read content was not truncated: {out}"));
    let content = v
        .get("file")
        .and_then(|f| f.get("content"))
        .and_then(|c| c.as_str())
        .unwrap_or_else(|| panic!("shape lost: {v}"));
    assert!(
        content.chars().count() <= LIMIT,
        "content {} > {LIMIT}",
        content.chars().count()
    );
}

/// A short single-line stderr next to a large stdout: proportional sharing
/// gives stderr a share smaller than the omission header, so it is replaced by
/// a header that is LONGER than the original field. No field may grow.
#[test]
#[ignore = "RED(posttool) short field replaced by an omission header longer than the field itself"]
fn short_field_is_never_grown_by_truncation() {
    let stderr = "error: build failed at step 3!".to_string(); // 30 chars, 1 line
    assert_eq!(stderr.chars().count(), 30);
    let input = serde_json::json!({
        "stdout": lines(49_975),
        "stderr": stderr,
        "interrupted": false,
        "isImage": false,
    });
    let out = run_toolguard("Bash", input.clone());
    let v = updated_tool_output(&out).unwrap_or_else(|| panic!("50005 chars must trigger: {out}"));
    for k in ["stdout", "stderr"] {
        assert!(
            str_len(&v, k) <= str_len(&input, k),
            "{k} grew: {} > {} ({:?})",
            str_len(&v, k),
            str_len(&input, k),
            v.get(k)
        );
    }
    assert!(str_len(&v, "stdout") + str_len(&v, "stderr") <= LIMIT);
}

/// A single-line WebFetch `result` (minified HTML/JSON) over the limit: the
/// line-based truncation keeps zero lines, so the whole result becomes just the
/// header `[toolguard: 59KB → 49KB, ...]`, which claims ~49KB were kept while
/// none were. The rewrite must keep some of the actual content.
#[test]
#[ignore = "RED(posttool) single-line result truncated to header only; header claims 49KB kept"]
fn single_line_webfetch_result_keeps_some_content() {
    let big = "<div>x</div>".repeat(5_000); // 60k chars, one line
    let input = serde_json::json!({
        "bytes": big.len(),
        "code": 200,
        "codeText": "OK",
        "result": big,
        "durationMs": 120,
        "url": "https://example.invalid/",
    });
    let out = run_toolguard("WebFetch", input);
    let v = updated_tool_output(&out).unwrap_or_else(|| panic!("60k must trigger: {out}"));
    let r = v
        .get("result")
        .and_then(|x| x.as_str())
        .expect("result str");
    assert!(
        r.contains("<div>x</div><div>x</div>"),
        "no original content kept in result: {r:?}"
    );
}

/// Control: a multi-line WebFetch result is truncated, bounded, shape kept.
#[test]
fn control_multiline_webfetch_result_bounded_shape_kept() {
    let big = lines(120_000);
    let input = serde_json::json!({
        "bytes": big.len(),
        "code": 200,
        "codeText": "OK",
        "result": big,
        "durationMs": 120,
        "url": "https://example.invalid/",
    });
    let out = run_toolguard("WebFetch", input.clone());
    let v = updated_tool_output(&out).expect("rewrite");
    let mut ik: Vec<_> = input.as_object().unwrap().keys().collect();
    let mut ok: Vec<_> = v.as_object().unwrap().keys().collect();
    ik.sort();
    ok.sort();
    assert_eq!(ik, ok);
    assert!(str_len(&v, "result") <= LIMIT);
    assert_eq!(v["code"], 200);
}
