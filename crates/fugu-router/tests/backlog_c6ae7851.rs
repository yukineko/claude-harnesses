#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro for backlog c6ae7851: PDO hypothesis f5f9522a needs to know, per
//! episode, which model the router SUGGESTED vs which model actually ran
//! (override-vs-followed). `fugu-router record` has no way to receive that
//! value and the episode schema has no field for it, so the measurement can
//! never accumulate — it is a missing write path, not missing data.
//!
//! (The ticket's tokens_input/tokens_output half has since landed:
//! `--tokens-input/--tokens-output`. This pins the remaining half.)

use std::path::{Path, PathBuf};
use std::process::Command;

fn temp_home(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let d = std::env::temp_dir().join(format!(
        "fugu-backlog-c6ae7851-{tag}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn record(home: &Path, extra: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_fugu-router"))
        .env("HOME", home)
        .env_remove("FUGU_ROUTER_DISABLED")
        .args([
            "record", "--title", "add auth", "--model", "opus", "--status", "verified",
        ])
        .args(extra)
        .output()
        .unwrap()
}

fn store(home: &Path) -> String {
    std::fs::read_to_string(home.join(".fugu-router/episodes.jsonl")).unwrap_or_default()
}

#[test]
fn backlog_c6ae7851_control_record_writes_the_store() {
    let home = temp_home("control");
    let out = record(&home, &[]);
    let s = store(&home);
    let _ = std::fs::remove_dir_all(&home);
    assert!(out.status.success(), "control record failed: {out:?}");
    assert!(
        s.contains("\"add auth\""),
        "control: episode not stored: {s:?}"
    );
}

#[test]
#[ignore = "backlog c6ae7851: open defect, remove ignore when fixed"]
fn backlog_c6ae7851_record_persists_suggested_model() {
    let home = temp_home("suggested");
    let out = record(&home, &["--suggested-model", "sonnet"]);
    let s = store(&home);
    let _ = std::fs::remove_dir_all(&home);
    assert!(
        out.status.success(),
        "record cannot receive the router's suggested model: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        s.contains("\"suggested_model\":\"sonnet\""),
        "episode does not carry suggested_model: {s:?}"
    );
}
