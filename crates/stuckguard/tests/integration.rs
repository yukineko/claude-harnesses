//! End-to-end tests for the stuckguard binary.
//!
//! `watch` is the PostToolUse hook: it can only ADVISE (inject context); it can
//! never block a tool call or end a turn, so it must ALWAYS exit 0. `status` is
//! a read-only CLI subcommand.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_home() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("stuckguard-it-{}-{}", std::process::id(), n));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

/// Run `stuckguard <args>` with `payload` on stdin in an isolated HOME/CWD.
fn run(args: &[&str], payload: &str) -> (i32, String) {
    let bin = env!("CARGO_BIN_EXE_stuckguard");
    let home = temp_home();
    let mut child = Command::new(bin)
        .args(args)
        .current_dir(&home)
        .env("HOME", &home)
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

#[test]
fn help_describes_the_detector() {
    let (code, stdout) = run(&["--help"], "");
    assert_eq!(code, 0);
    assert!(
        stdout.contains("Stuck-loop detector"),
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
fn watch_hook_valid_payload_exits_zero() {
    // A single, non-repeating PostToolUse event: no stuck pattern → silent.
    let payload = r#"{"hook_event_name":"PostToolUse","session_id":"it","tool_name":"Read","tool_input":{"file_path":"a.rs"}}"#;
    let (code, stdout) = run(&["watch"], payload);
    assert_eq!(code, 0, "PostToolUse hook must always exit 0");
    assert!(
        stdout.trim().is_empty(),
        "a single event trips no nudge, got: {stdout}"
    );
}

#[test]
fn watch_hook_survives_garbage_stdin() {
    let (code, _stdout) = run(&["watch"], "not json");
    assert_eq!(
        code, 0,
        "fail-soft: garbage stdin must never break the turn"
    );
}

#[test]
fn watch_hook_survives_empty_stdin() {
    let (code, _stdout) = run(&["watch"], "");
    assert_eq!(code, 0, "fail-soft: empty stdin must never break the turn");
}

// --- lessons-store WRITE-on-escalation -----------------------------------
//
// stuckguard's escalation path (repeat_threshold=2, escalate_after=1 here so
// two identical Bash calls trip AND escalate on the same event) must append
// exactly one error-pattern lesson to the cross-project lessons store; a
// non-escalating run must append none. Each test isolates BOTH the per-session
// state (via HOME, as above) and the lessons store (via the absolute-only
// `LESSONS_STORE_DIR` override) to an independent temp dir, so tests never
// race each other or touch the real machine-global store.

/// Run `stuckguard <args>` with `payload` on stdin in an isolated HOME/CWD
/// and an isolated lessons store dir, using a `stuckguard.toml` that makes
/// escalation trip fast and deterministic (repeat_threshold=2,
/// escalate_after=1: the 2nd identical event both trips AND escalates).
fn run_with_lessons(
    home: &PathBuf,
    lessons_dir: &PathBuf,
    args: &[&str],
    payload: &str,
) -> (i32, String) {
    std::fs::write(
        home.join("stuckguard.toml"),
        "repeat_threshold = 2\noscillation_threshold = 1\nescalate_after = 1\ncooldown_events = 0\n",
    )
    .expect("write stuckguard.toml");

    let bin = env!("CARGO_BIN_EXE_stuckguard");
    let mut child = Command::new(bin)
        .args(args)
        .current_dir(home)
        .env("HOME", home)
        .env("LESSONS_STORE_DIR", lessons_dir)
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

fn bash_payload(session: &str, cmd: &str) -> String {
    format!(
        r#"{{"hook_event_name":"PostToolUse","session_id":"{session}","tool_name":"Bash","tool_input":{{"command":"{cmd}"}}}}"#
    )
}

fn count_lessons(lessons_dir: &Path) -> usize {
    let path = lessons_dir.join("lessons.jsonl");
    match std::fs::read_to_string(&path) {
        Ok(s) => s.lines().filter(|l| !l.trim().is_empty()).count(),
        Err(_) => 0,
    }
}

#[test]
fn escalation_appends_exactly_one_error_pattern_lesson() {
    let home = temp_home();
    let lessons_dir = temp_home();
    let session = "sess-escalate";

    // 1st Bash call: records the event, no repeat yet (window has 1 event).
    let (c1, out1) = run_with_lessons(
        &home,
        &lessons_dir,
        &["watch"],
        &bash_payload(session, "cargo test"),
    );
    assert_eq!(c1, 0);
    assert!(out1.trim().is_empty(), "first call trips nothing: {out1}");
    assert_eq!(
        count_lessons(&lessons_dir),
        0,
        "no lesson before escalation"
    );

    // 2nd identical Bash call: repeat_threshold=2 trips, escalate_after=1
    // means this single nudge already counts as escalated.
    let (c2, out2) = run_with_lessons(
        &home,
        &lessons_dir,
        &["watch"],
        &bash_payload(session, "cargo test"),
    );
    assert_eq!(c2, 0, "PostToolUse hook must always exit 0");
    assert!(
        out2.contains("🛑"),
        "2nd identical call must escalate, got: {out2}"
    );

    let n = count_lessons(&lessons_dir);
    assert_eq!(n, 1, "escalation must append exactly one lesson");

    let contents = std::fs::read_to_string(lessons_dir.join("lessons.jsonl")).unwrap();
    let line = contents.lines().next().unwrap();
    let v: serde_json::Value = serde_json::from_str(line).unwrap();
    assert_eq!(v["kind"], "error-pattern", "lesson kind: {v}");
    assert!(
        v["task_summary"]
            .as_str()
            .unwrap()
            .contains("stuck pattern"),
        "task_summary should describe the stuck pattern: {v}"
    );
    assert!(
        !v["lesson_text"].as_str().unwrap().is_empty(),
        "lesson_text must be non-empty: {v}"
    );
    assert!(
        v["source_run"].as_str().unwrap().contains("stuckguard"),
        "source_run should record provenance: {v}"
    );

    // A 3rd identical call re-trips (cooldown_events=0) and re-escalates. The
    // window's repeat count keeps growing (2 -> 3), so the distilled lesson
    // text legitimately differs and a SECOND, distinct lesson is appended —
    // this is not a violation of idempotency: re-adding a lesson with
    // byte-identical content (same kind/task_summary/lesson_text, hence the
    // same content-derived id) IS a no-op, which the store-level test below
    // exercises directly against `harness_core::lessons`.
    let (c3, _) = run_with_lessons(
        &home,
        &lessons_dir,
        &["watch"],
        &bash_payload(session, "cargo test"),
    );
    assert_eq!(c3, 0);
    assert_eq!(
        count_lessons(&lessons_dir),
        2,
        "a growing repeat streak distills into a new lesson text each escalation"
    );

    // But re-running the exact same trigger a SECOND time from a frozen
    // session (no new event, so `t.count`/text stays identical) must be a
    // true no-op: append the same content-derived id twice via the store
    // directly (mirrors how `record_lesson` builds a `Lesson`) and confirm
    // idempotency-by-id holds, which is the same contract `harness_core::lessons`
    // already guarantees.
    std::env::set_var("LESSONS_STORE_DIR", &lessons_dir);
    let before = harness_core::lessons::load().len();
    let lesson = harness_core::lessons::Lesson {
        id: "idempotency-probe".to_string(),
        kind: harness_core::lessons::Kind::ErrorPattern,
        task_summary: "stuck pattern (repeat): probe".to_string(),
        lesson_text: "probe text".to_string(),
        source_run: "stuckguard:probe".to_string(),
        ts: 0,
    };
    harness_core::lessons::append(&lesson);
    harness_core::lessons::append(&lesson);
    let after = harness_core::lessons::load().len();
    std::env::remove_var("LESSONS_STORE_DIR");
    assert_eq!(
        after,
        before + 1,
        "byte-identical re-append (same content-derived id) is a no-op"
    );
}

#[test]
fn non_escalating_nudge_appends_no_lesson() {
    let home = temp_home();
    let lessons_dir = temp_home();
    let session = "sess-non-escalate";

    // Same repeat_threshold=2 config but escalate_after left permissive by
    // overwriting the toml with a higher bar: the 2nd identical call trips a
    // nudge but does NOT escalate, so no lesson should be written.
    std::fs::write(
        home.join("stuckguard.toml"),
        "repeat_threshold = 2\noscillation_threshold = 1\nescalate_after = 5\ncooldown_events = 0\n",
    )
    .expect("write stuckguard.toml");

    let bin = env!("CARGO_BIN_EXE_stuckguard");
    let run_once = |cmd: &str| -> (i32, String) {
        let mut child = Command::new(bin)
            .args(["watch"])
            .current_dir(&home)
            .env("HOME", &home)
            .env("LESSONS_STORE_DIR", &lessons_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("binary spawns");
        if let Some(mut child_stdin) = child.stdin.take() {
            let _ = child_stdin.write_all(bash_payload(session, cmd).as_bytes());
        }
        let out = child.wait_with_output().expect("binary runs");
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
        )
    };

    let (c1, _) = run_once("cargo build");
    assert_eq!(c1, 0);
    let (c2, out2) = run_once("cargo build");
    assert_eq!(c2, 0);
    assert!(
        !out2.contains("🛑"),
        "escalate_after=5 must NOT escalate on the 2nd nudge: {out2}"
    );

    assert_eq!(
        count_lessons(&lessons_dir),
        0,
        "a non-escalating nudge must append zero lessons"
    );
}

#[test]
fn escalation_with_unwritable_lessons_store_never_panics_hook() {
    // Point LESSONS_STORE_DIR at a path whose PARENT is a regular file, so
    // `create_dir_all` for the store dir fails and every write is impossible —
    // the write path must stay fail-soft and the hook must still exit 0.
    let home = temp_home();
    let blocker = temp_home();
    std::fs::write(blocker.join("not-a-dir"), b"x").expect("write blocker file");
    let unwritable_lessons_dir = blocker.join("not-a-dir").join("lessons-store");
    let session = "sess-unwritable";

    let (c1, _) = run_with_lessons(
        &home,
        &unwritable_lessons_dir,
        &["watch"],
        &bash_payload(session, "cargo test"),
    );
    assert_eq!(c1, 0, "hook must exit 0 even with an unwritable store");

    let (c2, out2) = run_with_lessons(
        &home,
        &unwritable_lessons_dir,
        &["watch"],
        &bash_payload(session, "cargo test"),
    );
    assert_eq!(
        c2, 0,
        "escalation over an unwritable lessons store must not panic/break the hook"
    );
    assert!(
        out2.contains("🛑"),
        "escalation message must still be emitted even though the lesson write is dropped: {out2}"
    );
}
