#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Backlog 7d56f1e3: an unknown/misspelled `mode` in the reviewgate config must
//! be a config error that BLOCKS the Stop hook (visible on stdout), never a
//! silent downgrade to `Inject`. Mirrors propguard 35f2ef54.
//!
//! Written by a disinterested test author (CLAUDE.md §2(a)). Drives the real
//! binary with a sandboxed HOME.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

fn project(tag: &str, with_change: bool) -> PathBuf {
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("rg-7d56f1e3-{tag}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for args in [
        vec!["init", "-q"],
        vec!["config", "user.email", "t@t"],
        vec!["config", "user.name", "t"],
    ] {
        let _ = Command::new("git").current_dir(&dir).args(&args).output();
    }
    std::fs::write(dir.join("seed.rs"), "fn a() {}\n").unwrap();
    for args in [vec!["add", "-A"], vec!["commit", "-qm", "seed"]] {
        let _ = Command::new("git").current_dir(&dir).args(&args).output();
    }
    if with_change {
        std::fs::write(
            dir.join("changed.rs"),
            "fn f() {\n    let x = 1;\n    println!(\"{}\", x);\n}\n",
        )
        .unwrap();
    }
    dir
}

fn write_home_mode(home: &Path, mode: &str) {
    let d = home.join(".reviewgate");
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("config.toml"), format!("mode = \"{mode}\"\n")).unwrap();
}

fn stop(dir: &Path) -> (i32, String, String) {
    let payload = serde_json::json!({
        "session_id": "s-7d56f1e3",
        "cwd": dir.to_string_lossy(),
        "hook_event_name": "Stop",
    })
    .to_string();
    let mut child = Command::new(env!("CARGO_BIN_EXE_reviewgate"))
        .arg("review")
        .current_dir(dir)
        .env("HOME", dir)
        .env("CLAUDE_CODE_SESSION_ID", "s-7d56f1e3")
        .env_remove("REVIEWGATE_DISABLE")
        .env_remove("HARNESS_TRUST_ALL")
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
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Assert stdout is block JSON naming the value, valid values, and the tag.
fn assert_config_invalid_block(dir: &Path, bad: &str) {
    let (code, stdout, stderr) = stop(dir);
    assert_eq!(
        code, 0,
        "hook exits 0; decision field blocks. stderr={stderr:?}"
    );
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!("stdout must be block JSON, got {stdout:?} (stderr={stderr:?}): {e}")
    });
    assert_eq!(v["decision"], "block", "stdout={stdout} stderr={stderr}");
    let visible = format!(
        "{} {}",
        v["reason"].as_str().unwrap_or_default(),
        v["systemMessage"].as_str().unwrap_or_default()
    );
    assert!(visible.contains(bad), "must name {bad:?}: {visible}");
    assert!(
        visible.contains("config-invalid"),
        "must carry config-invalid tag: {visible}"
    );
    assert!(
        visible.contains("inject") && visible.contains("subprocess"),
        "must list valid modes: {visible}"
    );
}

#[test]
fn unknown_mode_blocks_config_invalid_on_clean_repo() {
    let dir = project("clean", false);
    write_home_mode(&dir, "subproces");
    assert_config_invalid_block(&dir, "subproces");
}

#[test]
fn unknown_mode_blocks_config_invalid_with_reviewable_change() {
    let dir = project("changed", true);
    write_home_mode(&dir, "subproces");
    assert_config_invalid_block(&dir, "subproces");
}

/// Anti-vacuity controls: known modes never yield config-invalid.
#[test]
fn known_modes_do_not_produce_config_invalid() {
    for mode in ["subprocess", "Inject", " REVIEWER ", "independent"] {
        let dir = project("known", true);
        write_home_mode(&dir, mode);
        let (code, stdout, stderr) = stop(&dir);
        assert_eq!(code, 0);
        assert!(
            !stdout.contains("config-invalid") && !stderr.contains("config-invalid"),
            "known mode {mode:?} must not be config-invalid: {stdout} / {stderr}"
        );
    }
}

/// Control for the clean-repo case: a known mode on a clean repo allows.
#[test]
fn known_mode_on_clean_repo_allows() {
    let dir = project("known-clean", false);
    write_home_mode(&dir, "inject");
    let (code, stdout, _) = stop(&dir);
    assert_eq!(code, 0);
    assert!(
        !stdout.contains("block"),
        "clean repo + known mode allows: {stdout}"
    );
}
