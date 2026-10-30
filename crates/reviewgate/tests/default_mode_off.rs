#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! reviewgate's default mode is `off`: with NO home or project config the Stop
//! hook performs no review — it allows the stop, prints no block decision and
//! no injected rubric (measured waste: inject mode blocked 232/3000 stops only
//! to make the author review its own diff).
//!
//! `off` is an explicit verdict-free path (the gate is disabled), not an error
//! swallowed into allow: a config that EXISTS but cannot be read/parsed still
//! blocks `config-invalid`, and an explicit `mode = "inject"` still blocks the
//! same reviewable change (anti-vacuity control for the default-off test).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// A git repo with one committed file and one uncommitted reviewable change.
/// The dir doubles as HOME, so no real home config can leak in.
fn project(tag: &str) -> PathBuf {
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("rg-default-off-{tag}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for args in [
        vec!["init", "-q"],
        vec!["config", "user.email", "t@t"],
        vec!["config", "user.name", "t"],
    ] {
        let _ = harness_core::git_env::scrub(&mut Command::new("git"))
            .current_dir(&dir)
            .args(&args)
            .output();
    }
    std::fs::write(dir.join("seed.rs"), "fn a() {}\n").unwrap();
    for args in [vec!["add", "-A"], vec!["commit", "-qm", "seed"]] {
        let _ = harness_core::git_env::scrub(&mut Command::new("git"))
            .current_dir(&dir)
            .args(&args)
            .output();
    }
    std::fs::write(
        dir.join("changed.rs"),
        "fn f() {\n    let x = 1;\n    println!(\"{}\", x);\n}\n",
    )
    .unwrap();
    dir
}

fn write_home_config(home: &Path, body: &str) {
    let d = home.join(".reviewgate");
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("config.toml"), body).unwrap();
}

fn stop(dir: &Path) -> (i32, String, String) {
    let payload = serde_json::json!({
        "session_id": "s-default-off",
        "cwd": dir.to_string_lossy(),
        "hook_event_name": "Stop",
    })
    .to_string();
    let mut child =
        harness_core::git_env::scrub(&mut Command::new(env!("CARGO_BIN_EXE_reviewgate")))
            .arg("review")
            .current_dir(dir)
            .env("HOME", dir)
            .env("CLAUDE_CODE_SESSION_ID", "s-default-off")
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

/// A fragment of the default rubric (`config::DEFAULT_RUBRIC`), present in
/// every inject-mode block reason.
const RUBRIC_MARK: &str = "正しさ";

#[test]
fn no_config_default_is_off_and_does_not_review() {
    let dir = project("noconfig");
    assert!(!dir.join(".reviewgate/config.toml").exists());
    assert!(!dir.join("reviewgate.toml").exists());
    let (code, stdout, stderr) = stop(&dir);
    assert_eq!(code, 0, "stderr={stderr}");
    assert!(
        !stdout.contains("\"decision\"") && !stdout.contains("block"),
        "default (no config) must not block: stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(
        !stdout.contains(RUBRIC_MARK) && !stderr.contains(RUBRIC_MARK),
        "default (no config) must not inject a review rubric: stdout={stdout:?}"
    );
}

/// Anti-vacuity control: the same fixture with an explicit `mode = "inject"`
/// blocks and injects the rubric, so the test above observes the default,
/// not a fixture with nothing reviewable.
#[test]
fn explicit_inject_still_blocks_same_change() {
    let dir = project("inject");
    write_home_config(&dir, "mode = \"inject\"\n");
    let (code, stdout, stderr) = stop(&dir);
    assert_eq!(code, 0, "stderr={stderr}");
    let v: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("expected block JSON, got {stdout:?} ({stderr}): {e}"));
    assert_eq!(v["decision"], "block", "stdout={stdout}");
    assert!(
        v["reason"]
            .as_str()
            .unwrap_or_default()
            .contains(RUBRIC_MARK),
        "inject block must carry the rubric: {stdout}"
    );
}

/// `mode = "off"` written explicitly parses and disables the review.
#[test]
fn explicit_off_parses_and_does_not_review() {
    let dir = project("explicit-off");
    write_home_config(&dir, "mode = \"off\"\n");
    let (code, stdout, stderr) = stop(&dir);
    assert_eq!(code, 0, "stderr={stderr}");
    assert!(
        !stdout.contains("block") && !stdout.contains("config-invalid"),
        "mode = off must allow without config-invalid: {stdout:?} / {stderr:?}"
    );
}

/// A config that EXISTS but cannot be parsed is undetermined, not "off":
/// it must still block `config-invalid` rather than fall back to the off
/// default (which would turn a typo into a silently disabled gate).
#[test]
fn unparseable_existing_config_blocks_config_invalid() {
    let dir = project("unparseable");
    write_home_config(&dir, "mode = [[[ not toml\n");
    let (code, stdout, stderr) = stop(&dir);
    assert_eq!(code, 0, "stderr={stderr}");
    let v: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("expected block JSON, got {stdout:?} ({stderr}): {e}"));
    assert_eq!(v["decision"], "block", "stdout={stdout}");
    assert!(
        v["reason"]
            .as_str()
            .unwrap_or_default()
            .contains("config-invalid"),
        "must carry config-invalid: {stdout}"
    );
}
