//! End-to-end tests for the real built `trajectoryeval` binary.
//!
//! trajectoryeval is a plain clap CLI gate (NOT a lifecycle hook). Its exit codes
//! are load-bearing: 0 = trajectory matched, 1 = deviation, 2 = harness error
//! (unreadable/unparseable input). We feed it spec + actual JSON files in an
//! isolated temp dir so the result is fully deterministic.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

/// Run the binary with `args`. Returns (exit_code, stdout).
fn run(args: &[&str]) -> (i32, String) {
    let bin = env!("CARGO_BIN_EXE_trajectoryeval");
    let out = Command::new(bin)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("binary runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

fn unique_dir() -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let id = N.fetch_add(1, Ordering::Relaxed);
    let d = std::env::temp_dir().join(format!("trajectoryeval-it-{}-{}", std::process::id(), id));
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn help_describes_the_verifier() {
    let (code, stdout) = run(&["--help"]);
    assert_eq!(code, 0, "--help must exit 0");
    assert!(
        stdout.contains("Trajectory-match verifier"),
        "expected the about string, got: {stdout}"
    );
}

#[test]
fn check_matching_trajectory_passes_exit_zero() {
    let dir = unique_dir();
    let expected = dir.join("expected.json");
    let actual = dir.join("actual.json");
    std::fs::write(
        &expected,
        r#"{"mode":"strict","steps":[{"tool":"Read"},{"tool":"Edit"}]}"#,
    )
    .unwrap();
    std::fs::write(&actual, r#"["Read","Edit"]"#).unwrap();

    let (code, stdout) = run(&[
        "check",
        "--expected",
        expected.to_str().unwrap(),
        "--actual",
        actual.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "a matching trajectory passes with exit 0");
    assert!(
        stdout.contains("trajectory matched"),
        "expected the pass report, got: {stdout}"
    );
}

#[test]
fn check_deviating_trajectory_exits_one() {
    let dir = unique_dir();
    let expected = dir.join("expected.json");
    let actual = dir.join("actual.json");
    std::fs::write(
        &expected,
        r#"{"mode":"strict","steps":[{"tool":"Read"},{"tool":"Edit"}]}"#,
    )
    .unwrap();
    // Missing the required "Edit" step → a deviation.
    std::fs::write(&actual, r#"["Read"]"#).unwrap();

    let (code, stdout) = run(&[
        "check",
        "--expected",
        expected.to_str().unwrap(),
        "--actual",
        actual.to_str().unwrap(),
    ]);
    assert_eq!(code, 1, "a deviating trajectory exits 1");
    assert!(
        stdout.contains("trajectory deviated"),
        "expected the fail report, got: {stdout}"
    );
}

/// Write a minimal JSONL transcript fixture whose assistant message contains one
/// `tool_use` block per name in `tools`, in order — matches the shape
/// `extract::collect_from_content` understands (`message.content` array).
fn write_transcript(path: &std::path::Path, tools: &[&str]) {
    let blocks: Vec<serde_json::Value> = tools
        .iter()
        .map(|t| serde_json::json!({"type": "tool_use", "name": t, "input": {}}))
        .collect();
    let event = serde_json::json!({
        "type": "assistant",
        "message": { "content": blocks },
    });
    std::fs::write(path, format!("{}\n", event)).unwrap();
}

/// End-to-end: `extract` streams a real transcript into the actual-trajectory
/// JSON, and THAT output (not a hand-written fixture) is fed into `check`. This
/// is the real Phase-6 pipeline (`trajectoryeval extract | trajectoryeval check`)
/// that the condukt SKILL.md trajectory block wires up.
#[test]
fn extract_then_check_pipeline_passes_on_matching_trajectory() {
    let dir = unique_dir();
    let transcript = dir.join("transcript.jsonl");
    let expected = dir.join("expected.json");
    let actual = dir.join("actual.json");

    write_transcript(&transcript, &["Read", "Edit"]);
    std::fs::write(
        &expected,
        r#"{"mode":"strict","steps":[{"tool":"Read"},{"tool":"Edit"}]}"#,
    )
    .unwrap();

    // Step 1: extract the actual trajectory from the real transcript.
    let (extract_code, extract_stdout) =
        run(&["extract", "--transcript", transcript.to_str().unwrap()]);
    assert_eq!(
        extract_code, 0,
        "extract must succeed on a valid transcript"
    );
    // extract's stdout is the actual-trajectory JSON array; write it straight to
    // the file `check --actual` reads, exactly as the SKILL.md pipeline does.
    std::fs::write(&actual, extract_stdout.trim()).unwrap();

    // Step 2: feed the extracted trajectory into check.
    let (check_code, check_stdout) = run(&[
        "check",
        "--expected",
        expected.to_str().unwrap(),
        "--actual",
        actual.to_str().unwrap(),
    ]);
    assert_eq!(
        check_code, 0,
        "a matching extracted trajectory passes with exit 0"
    );
    assert!(
        check_stdout.contains("trajectory matched"),
        "expected the pass report, got: {check_stdout}"
    );
}

/// Same pipeline, but the transcript's real tool sequence deviates from the
/// declared `expected_trajectory` — must exit 1, not 0.
#[test]
fn extract_then_check_pipeline_exits_one_on_deviating_trajectory() {
    let dir = unique_dir();
    let transcript = dir.join("transcript.jsonl");
    let expected = dir.join("expected.json");
    let actual = dir.join("actual.json");

    // Worker only Read — never Edited — but the spec requires both.
    write_transcript(&transcript, &["Read"]);
    std::fs::write(
        &expected,
        r#"{"mode":"strict","steps":[{"tool":"Read"},{"tool":"Edit"}]}"#,
    )
    .unwrap();

    let (extract_code, extract_stdout) =
        run(&["extract", "--transcript", transcript.to_str().unwrap()]);
    assert_eq!(
        extract_code, 0,
        "extract must succeed on a valid transcript"
    );
    std::fs::write(&actual, extract_stdout.trim()).unwrap();

    let (check_code, check_stdout) = run(&[
        "check",
        "--expected",
        expected.to_str().unwrap(),
        "--actual",
        actual.to_str().unwrap(),
    ]);
    assert_eq!(check_code, 1, "a deviating extracted trajectory exits 1");
    assert!(
        check_stdout.contains("trajectory deviated"),
        "expected the fail report, got: {check_stdout}"
    );
}

#[test]
fn check_unreadable_input_exits_two() {
    // A missing expected-spec path is a harness error → exit 2.
    let dir = unique_dir();
    let missing = dir.join("does-not-exist.json");
    let actual = dir.join("actual.json");
    std::fs::write(&actual, r#"["Read"]"#).unwrap();

    let (code, _stdout) = run(&[
        "check",
        "--expected",
        missing.to_str().unwrap(),
        "--actual",
        actual.to_str().unwrap(),
    ]);
    assert_eq!(code, 2, "unreadable input is a harness error (exit 2)");
}
