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

// ── `tier` subcommand: end-to-end via the real committed example config ────────
//
// These drive the actual `tier` CLI subcommand (built args, real process, real
// exit code + stdout) against the real, committed
// `examples/tier-config.json` allowlist — not a synthetic one-off fixture — so
// the shipped example is itself exercised by the test suite.

/// Absolute path to the crate's committed example tier config.
fn example_tier_config() -> String {
    // CARGO_MANIFEST_DIR is the trajectoryeval crate root at compile time.
    concat!(env!("CARGO_MANIFEST_DIR"), "/examples/tier-config.json").to_string()
}

#[test]
fn tier_core_flow_matching_snapshot_passes_exit_zero() {
    let dir = unique_dir();
    let baseline = dir.join("baseline.json");
    let snapshot = dir.join("snapshot.json");
    std::fs::write(&baseline, r#"{"total": 42, "items": ["a", "b"]}"#).unwrap();
    std::fs::write(&snapshot, r#"{"items": ["a", "b"], "total": 42}"#).unwrap();

    let (code, stdout) = run(&[
        "tier",
        "--config",
        &example_tier_config(),
        // "checkout" is on the committed example's core allowlist.
        "--flow",
        "checkout",
        "--baseline",
        baseline.to_str().unwrap(),
        "--snapshot",
        snapshot.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "matching core-flow diff passes with exit 0");
    assert!(stdout.contains("tier: core"), "stdout: {stdout}");
    assert!(
        stdout.contains("diff: match"),
        "expected a match report, got: {stdout}"
    );
    assert!(stdout.contains("result: pass"), "stdout: {stdout}");
}

#[test]
fn tier_core_flow_mismatched_snapshot_exits_one() {
    let dir = unique_dir();
    let baseline = dir.join("baseline.json");
    let snapshot = dir.join("snapshot.json");
    std::fs::write(&baseline, r#"{"total": 42}"#).unwrap();
    std::fs::write(&snapshot, r#"{"total": 99}"#).unwrap();

    let (code, stdout) = run(&[
        "tier",
        "--config",
        &example_tier_config(),
        // "payment" is also on the committed example's core allowlist.
        "--flow",
        "payment",
        "--baseline",
        baseline.to_str().unwrap(),
        "--snapshot",
        snapshot.to_str().unwrap(),
    ]);
    assert_eq!(code, 1, "a real core-flow mismatch is a deviation (exit 1)");
    assert!(
        stdout.contains("MISMATCH"),
        "expected a mismatch report, got: {stdout}"
    );
    assert!(stdout.contains("result: fail"), "stdout: {stdout}");
}

#[test]
fn tier_non_core_existing_flow_passes_exit_zero() {
    let (code, stdout) = run(&[
        "tier",
        "--config",
        &example_tier_config(),
        // "settings" is NOT on the example's core allowlist → non-core path.
        "--flow",
        "settings",
        "--exists",
        "true",
        "--seed",
        "42",
        "--run-index",
        "1",
    ]);
    assert_eq!(code, 0, "a present non-core flow passes with exit 0");
    assert!(stdout.contains("tier: non-core"), "stdout: {stdout}");
    assert!(stdout.contains("result: pass"), "stdout: {stdout}");
}

#[test]
fn tier_non_core_absent_flow_exits_one() {
    let (code, stdout) = run(&[
        "tier",
        "--config",
        &example_tier_config(),
        "--flow",
        "settings",
        "--exists",
        "false",
    ]);
    assert_eq!(
        code, 1,
        "a non-core flow that fails its existence check exits 1"
    );
    assert!(stdout.contains("ABSENT"), "stdout: {stdout}");
    assert!(stdout.contains("result: fail"), "stdout: {stdout}");
}

#[test]
fn tier_core_flow_screenshot_stub_is_needs_human_not_silently_red() {
    // A core flow configured with the (stub) screenshot diff strategy must NOT
    // masquerade as a hard diff failure — it gets its own distinct exit code
    // (3) and an explicit "needs-human" label, not exit 1 / "fail".
    let dir = unique_dir();
    let config = dir.join("screenshot-tier-config.json");
    let baseline = dir.join("baseline.json");
    let snapshot = dir.join("snapshot.json");
    std::fs::write(
        &config,
        r#"{"core": ["checkout"], "diff_strategy": "screenshot", "sample_one_in": 0}"#,
    )
    .unwrap();
    std::fs::write(&baseline, r#"{"a": 1}"#).unwrap();
    std::fs::write(&snapshot, r#"{"a": 1}"#).unwrap();

    let (code, stdout) = run(&[
        "tier",
        "--config",
        config.to_str().unwrap(),
        "--flow",
        "checkout",
        "--baseline",
        baseline.to_str().unwrap(),
        "--snapshot",
        snapshot.to_str().unwrap(),
    ]);
    assert_eq!(
        code, 3,
        "an unimplemented (screenshot) diff strategy must NOT exit 1 (hard fail); \
         it gets its own needs-human exit code, got stdout: {stdout}"
    );
    assert_ne!(code, 1, "must not masquerade as a real diff failure");
    assert!(
        stdout.contains("NEEDS-HUMAN"),
        "expected an explicit needs-human label, got: {stdout}"
    );
    assert!(
        stdout.contains("result: needs-human"),
        "expected the result line to say needs-human, not fail, got: {stdout}"
    );
    assert!(
        !stdout.contains("result: fail"),
        "must not be reported as a plain fail, got: {stdout}"
    );
}

#[test]
fn tier_json_output_reports_verdict_field() {
    let dir = unique_dir();
    let baseline = dir.join("baseline.json");
    let snapshot = dir.join("snapshot.json");
    std::fs::write(&baseline, r#"{"a": 1}"#).unwrap();
    std::fs::write(&snapshot, r#"{"a": 1}"#).unwrap();

    let (code, stdout) = run(&[
        "tier",
        "--config",
        &example_tier_config(),
        "--flow",
        "checkout",
        "--baseline",
        baseline.to_str().unwrap(),
        "--snapshot",
        snapshot.to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(code, 0);
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("valid JSON output");
    assert_eq!(v["verdict"], "pass");
    assert_eq!(v["tier"], "core");
}
