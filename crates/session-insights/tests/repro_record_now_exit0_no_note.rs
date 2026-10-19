//! RED repro: `record-now` exits 0 with empty stdout when no note was written
//! because the vault is missing.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

fn run3(home: &Path, args: &[&str], payload: &str) -> (i32, String, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_session-insights"))
        .args(args)
        .current_dir(home)
        .env("HOME", home)
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("binary spawns");
    if let Some(mut si) = child.stdin.take() {
        let _ = si.write_all(payload.as_bytes());
    }
    let out = child.wait_with_output().expect("binary runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn home(tag: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!("si-repro-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    p
}

#[test]
#[ignore = "RED repro: record-now exits 0 when it wrote no note"]
fn record_now_without_vault_does_not_exit_zero() {
    let h = home("recnow");
    let (code, stdout, stderr) = run3(&h, &["record-now", "--session", "s1"], "");
    let _ = std::fs::remove_dir_all(&h);
    assert!(
        code != 0 || !stdout.trim().is_empty(),
        "no note written yet exit 0 with empty stdout. stderr={stderr:?}"
    );
}
