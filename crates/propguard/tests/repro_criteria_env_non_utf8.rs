#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Repro (P1, bca9cabe): a non-UTF-8 PROPGUARD_CRITERIA value used to be
//! treated as unset, so with no other criteria source the stop was allowed as
//! `no-criteria`. A criteria source that is set but unreadable is configured-
//! but-undetermined, not "not configured": it must block like the UTF-8 control.

use std::ffi::OsString;
use std::io::Write;
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn git(work: &Path, args: &[&str]) {
    let st = Command::new("git")
        .current_dir(work)
        .args(args)
        .status()
        .expect("git runs");
    assert!(st.success(), "git {args:?} failed");
}

fn run_check(work: &Path, home: &Path, criteria: OsString) -> (i32, String, String) {
    let payload = serde_json::json!({
        "hook_event_name": "Stop",
        "session_id": "repro-p1",
        "stop_hook_active": false,
        "cwd": work.to_string_lossy(),
    })
    .to_string();
    let mut child = Command::new(env!("CARGO_BIN_EXE_propguard"))
        .arg("check")
        .current_dir(work)
        .env("HOME", home)
        .env("PROPGUARD_STATE_DIR", home.join(".propguard-state"))
        .env("PROPGUARD_CRITERIA", criteria)
        .env("CLAUDE_CODE_SESSION_ID", "repro-p1")
        .env_remove("PROPGUARD_DISABLE")
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

fn is_block(stdout: &str) -> bool {
    stdout.contains("\"decision\":\"block\"") || stdout.contains("\"decision\": \"block\"")
}

fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("propguard-repro-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let work = root.join("work");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&work).unwrap();
    git(&work, &["init", "-q"]);
    git(&work, &["config", "user.email", "t@t.com"]);
    git(&work, &["config", "user.name", "t"]);
    std::fs::write(work.join("a.rs"), "fn a() {}\n").unwrap();
    git(&work, &["add", "a.rs"]);
    git(&work, &["commit", "-qm", "init"]);
    std::fs::write(work.join("a.rs"), "fn a() { panic!() }\n").unwrap();
    (root, work)
}

#[test]
fn non_utf8_criteria_env_blocks_like_the_utf8_control() {
    let (root, work) = fixture("p1");
    let home = root.join("home");

    let utf8 = "idempotent; never panic; stable output schema";
    let (_, cout, cerr) = run_check(&work, &home, OsString::from(utf8));
    assert!(
        is_block(&cout),
        "control: the UTF-8 criteria value must block an unchecked change. stdout={cout:?} stderr={cerr:?}"
    );

    let mut bytes = b"idempotent; never panic; ".to_vec();
    bytes.extend_from_slice(&[0xff, 0xfe]);
    let (code, out, err) = run_check(&work, &home, OsString::from_vec(bytes));
    assert_eq!(code, 0);
    assert!(
        is_block(&out),
        "PROPGUARD_CRITERIA is SET but not valid UTF-8; that is undetermined, not 'no criteria'. \
         stdout={out:?} stderr={err:?}"
    );
    let _ = std::fs::remove_dir_all(&root);
}
