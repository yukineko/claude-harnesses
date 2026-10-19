//! RED repro: the record and stop hooks exit 0 with nothing on stderr when the
//! payload is unparseable (integration.rs only pins exit 0).
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
fn garbage_payload_is_reported_on_stderr() {
    for hook in ["record", "stop"] {
        let h = home(hook);
        let (code, stdout, stderr) = run3(&h, &[hook], "not json");
        let _ = std::fs::remove_dir_all(&h);
        assert!(
            code != 0 || !stderr.trim().is_empty(),
            "hook {hook}: unparseable payload dropped with no diagnostic. exit={code} stdout={stdout:?} stderr={stderr:?}"
        );
    }
}
