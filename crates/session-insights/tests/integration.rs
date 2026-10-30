//! End-to-end tests for the session-insights binary.
//!
//! `record` (PostToolUse) and `stop` (Stop) are hooks: they only ever write to
//! their own state, never block a turn, and always exit 0. `report` and
//! `status` are read-only CLI subcommands.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_home() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir =
        std::env::temp_dir().join(format!("session-insights-it-{}-{}", std::process::id(), n));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

/// Run `session-insights <args>` with `payload` on stdin. HOME is isolated so
/// the state store (~/.session-insights) is never the real one. An optional
/// shared `home` keeps record→report in the same store.
fn run_in(home: &PathBuf, args: &[&str], payload: &str) -> (i32, String) {
    let bin = env!("CARGO_BIN_EXE_session-insights");
    let mut child = harness_core::git_env::scrub(&mut Command::new(bin))
        .args(args)
        .current_dir(home)
        .env("HOME", home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("binary spawns");
    if let Some(mut child_stdin) = child.stdin.take() {
        let _ = child_stdin.write_all(payload.as_bytes());
    }
    let out = child.wait_with_output().expect("binary runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

fn run(args: &[&str], payload: &str) -> (i32, String) {
    run_in(&temp_home(), args, payload)
}

#[test]
fn help_describes_session_metrics() {
    let (code, stdout) = run(&["--help"], "");
    assert_eq!(code, 0);
    assert!(
        stdout.contains("Per-session work metrics"),
        "expected about string, got: {stdout}"
    );
}

#[test]
fn status_reports_resolved_config() {
    let (code, stdout) = run(&["status"], "");
    assert_eq!(code, 0);
    assert!(
        stdout.contains("enabled:"),
        "expected status output, got: {stdout}"
    );
}

#[test]
fn report_on_empty_store_reports_no_sessions() {
    let (code, stdout) = run(&["report"], "");
    assert_eq!(code, 0);
    assert!(
        stdout.contains("no matching session"),
        "expected empty-store hint, got: {stdout}"
    );
}

#[test]
fn record_then_report_round_trips() {
    // Record a tool call, then report on the same session in the same HOME.
    let home = temp_home();
    let payload = r#"{"hook_event_name":"PostToolUse","session_id":"it-roundtrip","tool_name":"Read","tool_input":{"file_path":"a.rs"}}"#;
    let (rc, _) = run_in(&home, &["record"], payload);
    assert_eq!(rc, 0, "record hook must exit 0");
    let (code, stdout) = run_in(&home, &["report", "--session", "it-roundtrip"], "");
    assert_eq!(code, 0);
    assert!(
        stdout.contains("Read"),
        "expected the recorded tool in the report, got: {stdout}"
    );
}

/// `report --context` degrades gracefully when no context-governor ledger
/// exists: it still prints the session report plus a "no context ledger" line,
/// and never panics / exits non-zero.
#[test]
fn report_context_without_ledger_degrades_gracefully() {
    let home = temp_home();
    let payload = r#"{"hook_event_name":"PostToolUse","session_id":"it-ctx-empty","tool_name":"Read","tool_input":{"file_path":"a.rs"}}"#;
    let (rc, _) = run_in(&home, &["record"], payload);
    assert_eq!(rc, 0);
    let (code, stdout) = run_in(
        &home,
        &["report", "--session", "it-ctx-empty", "--context"],
        "",
    );
    assert_eq!(code, 0);
    assert!(stdout.contains("context health"), "got: {stdout}");
    assert!(stdout.contains("no context ledger"), "got: {stdout}");
}

/// `report --context` joins a synthetic context-governor ledger. The ledger path
/// is derived with the SAME shared helpers the writer uses (project_key /
/// safe_session), so this asserts the path convention is consistent end-to-end.
#[test]
fn report_context_joins_synthetic_ledger() {
    let home = temp_home();
    // The binary runs with current_dir(home) and CLAUDE_CODE_SESSION_ID set below,
    // so the ledger path is: <CG state>/<project_key(home)>/<safe_session(sid)>/ledger.jsonl
    // with <CG state> overridden to an isolated dir via CONTEXT_GOVERNOR_STATE_DIR.
    let cg_state = home.join("cg-state");
    let sid = "it-ctx-join";
    // The binary derives the key from a *canonicalized* cwd (it runs with
    // current_dir(home), which the OS canonicalizes). Mirror that here so the
    // synthetic ledger lands where the reader looks (macOS temp_home is under a
    // /var→/private/var symlink, so the raw vs canonical strings differ).
    let home_canonical = home.canonicalize().unwrap_or_else(|_| home.clone());
    let project = harness_core::store::project_key(&home_canonical);
    let session = harness_core::store::safe_session(sid);
    let ledger_dir = cg_state.join(&project).join(&session);
    std::fs::create_dir_all(&ledger_dir).expect("ledger dir");
    let jsonl = concat!(
        r#"{"session":"it-ctx-join","event":"groomed","saved_tokens":50,"resident_tokens":1200}"#,
        "\n",
        r#"{"session":"it-ctx-join","event":"injected","saved_tokens":0,"resident_tokens":1180}"#,
        "\n",
        r#"{"session":"it-ctx-join","event":"groomed","saved_tokens":30,"resident_tokens":900}"#,
        "\n",
    );
    std::fs::write(ledger_dir.join("ledger.jsonl"), jsonl).expect("write ledger");

    // Record a tool call for the session, then report with --context.
    let payload = r#"{"hook_event_name":"PostToolUse","session_id":"it-ctx-join","tool_name":"Read","tool_input":{"file_path":"a.rs"}}"#;

    let bin = env!("CARGO_BIN_EXE_session-insights");
    let rec = harness_core::git_env::scrub(&mut Command::new(bin))
        .args(["record"])
        .current_dir(&home)
        .env("HOME", &home)
        .env("CLAUDE_CODE_SESSION_ID", sid)
        .env("CONTEXT_GOVERNOR_STATE_DIR", &cg_state)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn record");
    {
        let mut child = rec;
        if let Some(mut s) = child.stdin.take() {
            let _ = s.write_all(payload.as_bytes());
        }
        let _ = child.wait_with_output().expect("record runs");
    }

    let out = harness_core::git_env::scrub(&mut Command::new(bin))
        .args(["report", "--session", sid, "--context"])
        .current_dir(&home)
        .env("HOME", &home)
        .env("CLAUDE_CODE_SESSION_ID", sid)
        .env("CONTEXT_GOVERNOR_STATE_DIR", &cg_state)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn report")
        .wait_with_output()
        .expect("report runs");
    let code = out.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(code, 0);
    assert!(stdout.contains("context health"), "got: {stdout}");
    assert!(stdout.contains("rows: 3"), "got: {stdout}");
    assert!(stdout.contains("saved tokens: 80"), "got: {stdout}");
    assert!(stdout.contains("groomed 2"), "got: {stdout}");
    assert!(stdout.contains("injected 1"), "got: {stdout}");
    assert!(stdout.contains("1200 peak"), "got: {stdout}");
    assert!(stdout.contains("900 last"), "got: {stdout}");
}

#[test]
fn record_hook_survives_garbage_stdin() {
    let (code, _stdout) = run(&["record"], "not json");
    assert_eq!(
        code, 0,
        "fail-soft: garbage stdin must never break the turn"
    );
}

/// An unparseable Stop payload prints exactly one short `systemMessage` line on
/// stdout (operator ruling 2026-10-27: results in one line, never logs) and is
/// also recorded as an `unparseable_payload` line in `<state_dir>/log.jsonl`.
#[test]
fn stop_hook_garbage_payload_prints_one_line_and_is_logged() {
    let home = temp_home();
    let (code, stdout) = run_in(&home, &["stop"], "not json");
    assert_eq!(code, 0, "stop must not block");
    assert_eq!(
        stdout.trim().lines().count(),
        1,
        "exactly one line: {stdout:?}"
    );
    let out: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("stdout not JSON ({e}): {stdout:?}"));
    assert!(out.get("decision").is_none(), "{out}");
    assert_eq!(
        out["systemMessage"], "session-insights: セッションを記録できませんでした",
        "stdout: {stdout:?}"
    );
    assert!(
        !stdout.contains("NOT counted"),
        "long message leaked: {stdout:?}"
    );
    assert!(
        !stdout.contains("not json"),
        "raw payload leaked: {stdout:?}"
    );
    let log = home.join(".session-insights/state/log.jsonl");
    let text = std::fs::read_to_string(&log).unwrap_or_default();
    let v: serde_json::Value = serde_json::from_str(text.lines().last().unwrap_or(""))
        .unwrap_or_else(|e| panic!("log line is not JSON ({e}): {text:?}"));
    assert_eq!(v["event"], "unparseable_payload", "{v}");
    let msg = v["message"].as_str().unwrap_or_default();
    assert!(msg.contains("NOT counted"), "message: {msg:?}");
}

/// SessionEnd discards stdout, so an unparseable payload must be on stderr.
#[test]
fn sessionend_hook_garbage_payload_is_reported_on_stderr() {
    let home = temp_home();
    let out =
        harness_core::git_env::scrub(&mut Command::new(env!("CARGO_BIN_EXE_session-insights")))
            .arg("sessionend")
            .current_dir(&home)
            .env("HOME", &home)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .and_then(|mut c| {
                if let Some(mut si) = c.stdin.take() {
                    let _ = si.write_all(b"not json");
                }
                c.wait_with_output()
            })
            .expect("binary runs");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0));
    assert!(
        stderr.contains("sessionend") && stderr.contains("NOT updated"),
        "stderr: {stderr:?}"
    );
}

#[test]
fn stop_hook_survives_empty_stdin() {
    let (code, _stdout) = run(&["stop"], "");
    assert_eq!(code, 0, "fail-soft: empty stdin must never break the turn");
}
