// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! backlog abba6f0d — `daily session-start` when the `backlog` binary's
//! presence cannot be determined.
//!
//! With the default `skip_when_driver_active = true`, daily must not run its
//! tasks while a `/flow` driver may be active. An unreadable plugin-cache
//! `backlog` dir means we cannot tell whether backlog (and so a driver) exists:
//! that must resolve to "driver possibly active" — skip — not to "backlog not
//! installed" — run. Observed RED at `82ee4a78` (bare-name spawn failed →
//! "not installed" → the task ran) and GREEN after `43780aa1`. The control
//! pins that an observed absence still runs the task.
#![cfg(unix)]

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

struct RestoreMode(PathBuf);
impl Drop for RestoreMode {
    fn drop(&mut self) {
        let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
    }
}

fn daily(home: &Path, args: &[&str], stdin: &str) -> (i32, String, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_daily"))
        .args(args)
        .env("HOME", home)
        .env("PATH", "/usr/bin:/bin")
        .current_dir(home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("daily spawns");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Register a task that touches `marker`, then fire session-start.
fn session_start_with_toucher(home: &Path, marker: &Path) -> (i32, String, String) {
    let cmd = format!("touch {}", marker.display());
    let (code, _, err) = daily(home, &["add", "--name", "toucher", "--command", &cmd], "");
    assert_eq!(code, 0, "add failed: {err}");
    let payload = format!(
        r#"{{"hook_event_name":"SessionStart","cwd":"{}"}}"#,
        home.display()
    );
    daily(home, &["session-start"], &payload)
}

#[test]
fn unreadable_backlog_cache_dir_stands_daily_down() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let bl = home.join(".claude/plugins/cache/yukineko/backlog");
    std::fs::create_dir_all(bl.join("0.3.0/bin")).unwrap();
    std::fs::set_permissions(&bl, std::fs::Permissions::from_mode(0o000)).unwrap();
    let _restore = RestoreMode(bl.clone());
    if std::fs::read_dir(&bl).is_ok() {
        eprintln!("SKIP: chmod 000 does not deny access here (root?)");
        return;
    }

    let marker = home.join("ran.marker");
    let (code, stdout, stderr) = session_start_with_toucher(&home, &marker);
    assert_eq!(
        code, 0,
        "a SessionStart hook still exits 0; stderr={stderr}"
    );
    assert!(
        !marker.exists(),
        "backlog's presence was undeterminable, so a driver may be active: the task must NOT run \
         (stdout={stdout} stderr={stderr})"
    );
    assert!(
        stderr.contains("could not locate `backlog`"),
        "the stand-down must be explained on stderr: {stderr}"
    );
}

#[test]
fn absent_backlog_still_runs_the_task() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();

    let marker = home.join("ran.marker");
    let (code, stdout, stderr) = session_start_with_toucher(&home, &marker);
    assert_eq!(code, 0, "stderr={stderr}");
    assert!(
        marker.exists(),
        "backlog observed absent: no driver can exist, the task runs (stdout={stdout} stderr={stderr})"
    );
}
