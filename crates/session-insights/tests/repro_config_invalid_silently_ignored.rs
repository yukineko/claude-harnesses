//! RED repro: config.rs ignores an unparseable config, and a size_thresholds
//! array of the wrong length, with no stderr and exit 0 (defaults used).
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
fn invalid_config_is_not_silently_defaulted() {
    for (tag, body) in [
        ("unparse", "enabled = [[[ not toml"),
        ("badlen", "size_thresholds = [1, 2, 3]\n"),
    ] {
        let h = home(tag);
        std::fs::write(h.join("session-insights.toml"), body).unwrap();
        let (code, stdout, stderr) = run3(&h, &["status"], "");
        let _ = std::fs::remove_dir_all(&h);
        assert!(
            code != 0 || !stderr.trim().is_empty(),
            "case {tag}: invalid config silently ignored. exit={code} stdout={stdout:?} stderr={stderr:?}"
        );
    }
}
