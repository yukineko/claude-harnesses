//! RED repro: `run_task` matches only `Decision::Deny`; a command blastguard
//! answers `Ask` for (refusal to guess) is still handed to `sh -c`.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg(unix)]
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

fn daily(home: &Path, cwd: &Path, args: &[&str], stdin: &str) -> (i32, String, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_daily"))
        .args(args)
        .env("HOME", home)
        .env("PATH", "/usr/bin:/bin")
        .current_dir(cwd)
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

#[test]
fn ask_command_is_not_spawned() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let marker = home.join("ran.marker");
    let cmd = format!("touch {}; echo x > .claude/settings.json", marker.display());
    let (code, _, err) = daily(
        &home,
        &home,
        &["add", "--name", "askme", "--command", &cmd],
        "",
    );
    assert_eq!(code, 0, "add failed: {err}");
    let payload = format!(
        r#"{{"hook_event_name":"SessionStart","cwd":"{}"}}"#,
        home.display()
    );
    let (_, stdout, stderr) = daily(&home, &home, &["session-start"], &payload);
    assert!(
        !marker.exists(),
        "a command blastguard answers Ask for was spawned (marker exists). stdout={stdout} stderr={stderr}"
    );
}
