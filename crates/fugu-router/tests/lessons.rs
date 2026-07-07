//! End-to-end tests for `fugu-router lessons add|search`.
//!
//! The lessons store is project-INDEPENDENT: its path is `~/.lessons/lessons.jsonl`
//! unless the `LESSONS_STORE_DIR` env var points at an ABSOLUTE dir (the
//! only-when-absolute override in `harness_core::lessons`). Every test here sets
//! that override to a unique absolute temp dir so the real machine-global store is
//! never read or written, and tests don't race each other.

use std::path::PathBuf;
use std::process::{Command, Stdio};

/// A unique, isolated ABSOLUTE temp dir for one test's lessons store.
fn temp_store_dir(tag: &str) -> PathBuf {
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("fugu-router-lessons-{tag}-{pid}-{nanos}"));
    std::fs::create_dir_all(&dir).expect("create temp store dir");
    assert!(
        dir.is_absolute(),
        "store dir must be absolute for the override to apply"
    );
    dir
}

/// Run the binary with `args`, with `LESSONS_STORE_DIR` pointed at `store_dir`.
/// Returns (exit_code, stdout).
fn run(store_dir: &PathBuf, args: &[&str]) -> (i32, String) {
    let bin = env!("CARGO_BIN_EXE_fugu-router");
    let out = Command::new(bin)
        .args(args)
        .env("LESSONS_STORE_DIR", store_dir)
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

#[test]
fn lessons_search_empty_store_is_fail_soft_empty_array() {
    let dir = temp_store_dir("empty");
    let (code, stdout) = run(&dir, &["lessons", "search", "--query", "anything at all"]);
    assert_eq!(
        code, 0,
        "search on an empty store must exit 0, got {code}: {stdout}"
    );
    let v: serde_json::Value = serde_json::from_str(&stdout).expect("stdout is JSON");
    assert_eq!(
        v,
        serde_json::json!([]),
        "empty store must yield []: {stdout}"
    );
}

#[test]
fn lessons_add_then_search_round_trip() {
    let dir = temp_store_dir("roundtrip");

    // Add a lesson.
    let (code, stdout) = run(
        &dir,
        &[
            "lessons",
            "add",
            "--kind",
            "error-pattern",
            "--task-summary",
            "fix the login authentication flow",
            "--lesson-text",
            "session token must be refreshed before retry",
            "--source-run",
            "run-abc",
        ],
    );
    assert_eq!(code, 0, "add must exit 0, got {code}: {stdout}");
    let id = stdout.trim().to_string();
    assert!(!id.is_empty(), "add must print the stored id");

    // A related query finds it.
    let (code, stdout) = run(
        &dir,
        &["lessons", "search", "--query", "login authentication token"],
    );
    assert_eq!(code, 0, "search must exit 0, got {code}: {stdout}");
    let v: serde_json::Value = serde_json::from_str(&stdout).expect("stdout is JSON");
    let arr = v.as_array().expect("search prints a JSON array");
    assert!(
        !arr.is_empty(),
        "a related query must find the added lesson: {stdout}"
    );
    assert_eq!(
        arr[0]["id"].as_str(),
        Some(id.as_str()),
        "the found lesson must be the one we added: {stdout}"
    );

    // An unrelated query yields [].
    let (code, stdout) = run(
        &dir,
        &[
            "lessons",
            "search",
            "--query",
            "quarterly billing invoice currency",
        ],
    );
    assert_eq!(
        code, 0,
        "unrelated search must exit 0, got {code}: {stdout}"
    );
    let v: serde_json::Value = serde_json::from_str(&stdout).expect("stdout is JSON");
    assert_eq!(
        v,
        serde_json::json!([]),
        "an unrelated query must yield []: {stdout}"
    );
}

#[test]
fn lessons_add_is_idempotent_by_content() {
    let dir = temp_store_dir("idem");
    let args = [
        "lessons",
        "add",
        "--kind",
        "convention",
        "--task-summary",
        "repo uses rustup toolchain",
        "--lesson-text",
        "source cargo env before invoking cargo commands",
        "--source-run",
        "run-1",
    ];
    let (code1, out1) = run(&dir, &args);
    let (code2, out2) = run(&dir, &args);
    assert_eq!(code1, 0, "first add exits 0: {out1}");
    assert_eq!(code2, 0, "second add exits 0: {out2}");
    assert_eq!(
        out1.trim(),
        out2.trim(),
        "same content → same content-derived id"
    );

    // The store must contain exactly one line (idempotent by id).
    let lessons = std::fs::read_to_string(dir.join("lessons.jsonl")).expect("store file exists");
    let count = lessons.lines().filter(|l| !l.trim().is_empty()).count();
    assert_eq!(
        count, 1,
        "re-adding identical content must not grow the store"
    );
}
