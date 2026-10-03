//! Backlog 1e6f00ae, hook side: the Stop `breadcrumb` hook writes the charter's
//! next_action; in the primary working tree it declines VISIBLY (stderr) and
//! exits 0. (Written by the implementer, not independently reviewed.)

use std::io::Write;
use std::process::{Command, Stdio};

fn git(cwd: &std::path::Path, args: &[&str]) {
    let o = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args)
        .current_dir(cwd)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
}

#[test]
fn breadcrumb_in_primary_declines_visibly_and_writes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let primary = std::fs::canonicalize(tmp.path()).unwrap().join("primary");
    std::fs::create_dir_all(&primary).unwrap();
    git(&primary, &["init", "-q", "-b", "main"]);
    git(&primary, &["commit", "-q", "--allow-empty", "-m", "i"]);
    let transcript = tmp.path().join("t.jsonl");
    let line = serde_json::json!({"type":"assistant","message":{"content":[{"type":"text","text":"x\n```compass-next\ndo the thing\n```\n"}]}});
    std::fs::write(&transcript, format!("{line}\n")).unwrap();
    let payload = serde_json::json!({
        "hook_event_name":"Stop","session_id":"s",
        "cwd": primary.to_str().unwrap(),
        "transcript_path": transcript.to_str().unwrap(),
    })
    .to_string();
    let mut c = Command::new(env!("CARGO_BIN_EXE_compass"))
        .arg("breadcrumb")
        .env("HOME", tmp.path())
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .current_dir(&primary)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    c.stdin
        .take()
        .unwrap()
        .write_all(payload.as_bytes())
        .unwrap();
    let o = c.wait_with_output().unwrap();
    assert_eq!(o.status.code(), Some(0));
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(
        err.contains("NOT recorded"),
        "must say it declined: {err:?}"
    );
    assert!(
        !primary.join(".compass").exists(),
        "nothing written in the primary tree"
    );
}
