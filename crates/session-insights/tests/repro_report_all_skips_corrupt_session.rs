//! RED repro: metrics::load_all silently skips a corrupt <sid>.json, so
//! `report --all` presents a partial list as the complete one.
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
#[ignore = "RED repro: report --all hides a corrupt session file"]
fn report_all_does_not_present_partial_list_as_complete() {
    let h = home("corrupt");
    let payload = r#"{"hook_event_name":"PostToolUse","session_id":"good-one","tool_name":"Read","tool_input":{"file_path":"a.rs"}}"#;
    let (rc, _, _) = run3(&h, &["record"], payload);
    assert_eq!(rc, 0);
    let state = h.join(".session-insights/state");
    assert!(state.is_dir(), "apparatus: state dir should exist");
    std::fs::write(state.join("corrupt-one.json"), "{ this is not json").unwrap();
    let (code, stdout, stderr) = run3(&h, &["report", "--all"], "");
    let _ = std::fs::remove_dir_all(&h);
    assert!(stdout.contains("good-one"), "apparatus: good session listed: {stdout:?}");
    assert!(
        code != 0 || !stderr.trim().is_empty(),
        "corrupt session file skipped with no diagnostic. exit={code} stdout={stdout:?} stderr={stderr:?}"
    );
}
