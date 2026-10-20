//! RED repro: `daily install` is unimplemented but exits 0.
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
fn unimplemented_install_does_not_exit_zero() {
    let tmp = tempfile::tempdir().unwrap();
    let (code, stdout, stderr) = daily(tmp.path(), tmp.path(), &["install"], "");
    let installed = tmp.path().join(".claude/settings.json").exists();
    assert!(
        code != 0 || installed,
        "install did nothing but exited 0. stdout={stdout} stderr={stderr}"
    );
}
