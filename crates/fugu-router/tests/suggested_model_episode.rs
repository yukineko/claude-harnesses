// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! hypothesis f5f9522a: episodes must record which model fugu-router
//! *suggested* (`Episode.suggested_model`, `record --suggested-model`).
//! An unknown value is `None` (key absent), never `""`; unmeasured
//! tokens / route_basis are never written as `0` / `""`.
//!
//! These tests observe the real binary and the raw JSONL on disk.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn temp_home(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "fugu-router-suggested-{tag}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("create temp home");
    dir
}

fn run_in(home: &Path, args: &[&str]) -> (i32, String, String) {
    let bin = env!("CARGO_BIN_EXE_fugu-router");
    let out = harness_core::git_env::scrub(&mut Command::new(bin))
        .args(args)
        .env("HOME", home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("binary runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn record(home: &Path, title: &str, extra: &[&str]) {
    let mut args = vec![
        "record", "--title", title, "--model", "sonnet", "--status", "verified",
    ];
    args.extend_from_slice(extra);
    let (code, out, err) = run_in(home, &args);
    assert_eq!(code, 0, "record `{title}` must exit 0: {out} {err}");
}

fn store_path(home: &Path) -> PathBuf {
    home.join(".fugu-router").join("episodes.jsonl")
}

fn raw_lines(home: &Path) -> Vec<String> {
    std::fs::read_to_string(store_path(home))
        .unwrap_or_else(|e| panic!("store must exist: {e}"))
        .lines()
        .map(str::to_string)
        .collect()
}

/// (a) RED today: `--suggested-model` is an unknown flag.
#[test]
fn record_suggested_model_flag_round_trips() {
    let home = temp_home("rt");
    record(&home, "t-rt", &["--suggested-model", "opus"]);
    let lines = raw_lines(&home);
    let v: serde_json::Value = serde_json::from_str(lines.last().unwrap()).unwrap();
    assert_eq!(v["suggested_model"], "opus", "episode JSON: {v}");
    // the executed model is independent of the suggestion
    assert_eq!(v["model"], "sonnet", "episode JSON: {v}");
}

/// (b) Without the flag: key absent or null, never "". Guard.
#[test]
fn record_without_suggested_model_is_none_never_empty_string() {
    let home = temp_home("none");
    record(&home, "t-none", &[]);
    let lines = raw_lines(&home);
    let v: serde_json::Value = serde_json::from_str(lines.last().unwrap()).unwrap();
    match v.get("suggested_model") {
        None | Some(serde_json::Value::Null) => {}
        Some(other) => panic!("unknown suggested_model must be absent/null, got {other}"),
    }
}

/// An empty `--suggested-model ""` must never be stored as "" (unknown is
/// None). The first record with a real value makes this RED today (unknown
/// flag), so it cannot pass vacuously on clap's rejection.
#[test]
fn record_empty_suggested_model_never_stored_as_empty_string() {
    let home = temp_home("empty");
    record(&home, "t-ok", &["--suggested-model", "haiku"]);
    let (code, _o, _e) = run_in(
        &home,
        &[
            "record",
            "--title",
            "t-empty",
            "--model",
            "sonnet",
            "--status",
            "verified",
            "--suggested-model",
            "",
        ],
    );
    for l in raw_lines(&home) {
        assert!(
            !l.contains("\"suggested_model\":\"\""),
            "empty string stored (exit {code}): {l}"
        );
    }
}

/// (c) Guard: a legacy line with no suggested_model still loads.
#[test]
fn legacy_episode_without_suggested_model_still_loads() {
    let home = temp_home("legacy");
    let dir = home.join(".fugu-router");
    std::fs::create_dir_all(&dir).unwrap();
    let legacy = r#"{"ts":1700000000,"title":"legacy","touched_files":[],"class":"impl","model":"sonnet","role":"worker","pass":true,"cost_usd":0.5}"#;
    std::fs::write(store_path(&home), format!("{legacy}\n")).unwrap();
    let (code, out, err) = run_in(&home, &["stats", "--json"]);
    assert_eq!(code, 0, "stats over legacy store must work: {out} {err}");
    assert!(
        out.contains("sonnet"),
        "legacy episode must be counted: {out}"
    );
    record(&home, "after-legacy", &[]);
    let (code, out, err) = run_in(&home, &["stats", "--json"]);
    assert_eq!(code, 0, "stats after append: {out} {err}");
}

/// (d) Guard: unmeasured tokens / route_basis are never written as 0 / "",
/// and the unknown suggested_model is not written as "" either.
#[test]
fn record_without_tokens_or_route_basis_suggested_model_unknown_never_zero_or_empty() {
    let home = temp_home("unmeasured");
    record(&home, "t-um", &[]);
    let line = raw_lines(&home).pop().unwrap();
    for bad in [
        "\"tokens_input\":0",
        "\"tokens_output\":0",
        "\"route_basis\":\"\"",
        "\"route_confidence\":\"\"",
        "\"route_rationale\":\"\"",
        "\"suggested_model\":\"\"",
    ] {
        assert!(!line.contains(bad), "fabricated `{bad}` in: {line}");
    }
    let v: serde_json::Value = serde_json::from_str(&line).unwrap();
    for k in ["tokens_input", "tokens_output", "route_basis"] {
        match v.get(k) {
            None | Some(serde_json::Value::Null) => {}
            Some(o) => panic!("{k} must be absent/null, got {o}"),
        }
    }
}
