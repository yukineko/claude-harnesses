//! Third adversarial verification (d0e467ee5, recursive `harness_core::shrink`)
//! through the real ctxrot binary with an isolated HOME / cwd.

use std::io::Write as _;
use std::process::{Command, Stdio};

/// Default `huge_tool_output_bytes`.
const LIMIT: usize = 50_000;

fn run_raw(payload: &str) -> String {
    let home = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
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
        .write_all(payload.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(0), "toolguard must exit 0: {out:?}");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn run_toolguard(tool_name: &str, tool_response: serde_json::Value) -> String {
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

fn lines(chars: usize) -> String {
    let line = "line of build output that is fairly long\n";
    let mut s = line.repeat(chars / line.len() + 1);
    s.truncate(chars);
    s
}

/// A STRING-shaped single-line 60k response must keep some actual content
/// (not just an omission header). Passes at d0e467ee5; kept as a check.
#[test]
fn string_shaped_single_line_keeps_content() {
    let big = "<div>x</div>".repeat(5_000);
    let out = run_toolguard("Bash", serde_json::Value::String(big));
    let v = updated(&out).unwrap_or_else(|| panic!("60k must trigger: {out}"));
    let s = v.as_str().expect("string shape kept");
    assert!(
        s.contains("<div>x</div><div>x</div>"),
        "no content kept: {s:?}"
    );
}

/// A Bash stdout dominated by escape-heavy chars (progress-bar `ESC[K` + `\r`
/// spam; JSON escaping > 2x) is rewritten to an EMPTY stdout with no marker.
#[test]
#[ignore = "RED(posttool) escape-heavy stdout rewritten to an empty string with no elision marker"]
fn escape_heavy_stdout_not_emptied() {
    let input = serde_json::json!({
        "stdout": "\u{1b}[K\r".repeat(20_000),
        "stderr": "",
        "interrupted": false,
        "isImage": false,
    });
    let out = run_toolguard("Bash", input);
    let v = updated(&out).unwrap_or_else(|| panic!("80k must trigger: {out}"));
    let s = v["stdout"].as_str().expect("stdout str");
    assert!(!s.is_empty(), "stdout emptied without any marker");
}

/// A response nested deeper than serde_json's default recursion limit (128)
/// makes `HookInput::parse` return None: the hook prints nothing at all, so a
/// 200k-char output passes through with no rewrite and no note.
#[test]
#[ignore = "RED(posttool) payload nested >128 deep fails to parse; 200k output passes silently (no rewrite, no note)"]
fn deeply_nested_response_is_not_silent() {
    let depth = 140;
    let inner = serde_json::Value::String(lines(200_000)).to_string();
    let resp = format!(
        "{}{{\"stdout\":{inner}}}{}",
        "[".repeat(depth),
        "]".repeat(depth)
    );
    let payload = format!(
        "{{\"session_id\":\"v3\",\"transcript_path\":\"\",\"cwd\":\"/tmp\",\
         \"hook_event_name\":\"PostToolUse\",\"tool_name\":\"Bash\",\"tool_input\":{{}},\
         \"tool_response\":{resp}}}"
    );
    let out = run_raw(&payload);
    assert!(
        hso(&out).is_some(),
        "200k nested output produced no rewrite and no note: {out:?}"
    );
}

/// Control: depth 100 (< parser limit) is walked and bounded.
#[test]
fn control_nested_depth_100_is_bounded() {
    let mut v = serde_json::json!({ "stdout": lines(200_000) });
    for _ in 0..100 {
        v = serde_json::json!([v]);
    }
    let out = run_toolguard("Bash", v);
    let u = updated(&out).unwrap_or_else(|| panic!("no rewrite: {out}"));
    assert!(u.to_string().chars().count() <= LIMIT);
}

/// An array of 5000 small objects (content-block-like `{type,text}`): items are
/// never dropped, every leaf is mangled, and the rewrite stays over the limit.
#[test]
#[ignore = "RED(posttool) array of many small objects cannot be brought within the limit (items never elided)"]
fn array_of_objects_within_limit() {
    let items: Vec<serde_json::Value> = (0..5000)
        .map(|i| serde_json::json!({"type": "text", "text": format!("match line {i:05} in some file")}))
        .collect();
    let out = run_toolguard("Grep", serde_json::Value::Array(items));
    let u = updated(&out).unwrap_or_else(|| panic!("no rewrite: {out}"));
    assert!(
        u.to_string().chars().count() <= LIMIT,
        "rewrite {} chars > {LIMIT}",
        u.to_string().chars().count()
    );
}

/// Control: numbers / booleans / null untouched, keys kept, bounded.
#[test]
fn control_scalars_untouched_and_bounded() {
    let input = serde_json::json!({
        "stdout": lines(200_000),
        "stderr": "warn",
        "interrupted": false,
        "isImage": true,
        "exitCode": 137,
        "returnCodeInterpretation": null,
    });
    let out = run_toolguard("Bash", input.clone());
    let u = updated(&out).unwrap_or_else(|| panic!("no rewrite: {out}"));
    for k in [
        "stderr",
        "interrupted",
        "isImage",
        "exitCode",
        "returnCodeInterpretation",
    ] {
        assert_eq!(u[k], input[k], "{k} changed");
    }
    assert!(u.to_string().chars().count() <= LIMIT);
}

/// Control: an array of 8 huge strings fits and keeps each head.
#[test]
fn control_eight_huge_strings_array() {
    let items: Vec<String> = (0..8)
        .map(|i| format!("item{i} ") + &lines(20_000))
        .collect();
    let out = run_toolguard("Bash", serde_json::json!(items));
    let u = updated(&out).unwrap_or_else(|| panic!("no rewrite: {out}"));
    assert!(u.to_string().chars().count() <= LIMIT);
    for (i, x) in u.as_array().unwrap().iter().enumerate() {
        assert!(x.as_str().unwrap().starts_with(&format!("item{i}")));
    }
}
