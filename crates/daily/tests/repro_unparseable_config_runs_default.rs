//! RED repro: an unparseable ~/.daily/config.toml falls back to defaults and
//! the built-in security task runs, with only a stderr warning.
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
fn unparseable_config_does_not_run_default_task() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(home.join(".daily")).unwrap();
    std::fs::write(
        home.join(".daily/config.toml"),
        "this is not = valid toml ][",
    )
    .unwrap();
    let payload = format!(
        r#"{{"hook_event_name":"SessionStart","cwd":"{}"}}"#,
        home.display()
    );
    let (code, stdout, stderr) = daily(&home, &home, &["session-start"], &payload);
    assert!(
        !stdout.contains("daily: ran security") || code != 0,
        "default security task ran on unparseable config. exit={code} stdout={stdout} stderr={stderr}"
    );
}
