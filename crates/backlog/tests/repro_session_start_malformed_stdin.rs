#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro (B2): the SessionStart hook exits 0 with empty stdout on malformed
//! stdin (stderr only). The injected backlog context silently vanishes and the
//! session cannot tell "nothing to inject" from "could not read the payload".
//!
//! Fixed (f580e5ef): tests/integration.rs::session_start_with_malformed_stdin_exits_nonzero
//! now pins the non-zero exit; it formerly pinned exit 0 with empty stdout.

mod common;

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

fn run_hook(payload: &str, home: &PathBuf, cwd: &PathBuf) -> (i32, String, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_backlog"))
        .arg("session-start")
        .env("HOME", home)
        .current_dir(cwd)
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

#[test]
fn session_start_malformed_stdin_is_a_visible_failure() {
    let home = std::env::temp_dir().join(format!("backlog-repro-b2-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    let repo = home.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    common::linked_checkout(&repo);

    let (ccode, cout, cerr) = run_hook("{}", &home, &repo);
    assert_eq!(
        ccode, 0,
        "control: a well-formed payload must exit 0. stdout={cout:?} stderr={cerr:?}"
    );

    let (code, stdout, stderr) = run_hook("not json", &home, &repo);
    assert!(
        code != 0 || !stdout.trim().is_empty(),
        "malformed stdin produced exit 0 and empty stdout: indistinguishable from 'nothing to inject'. \
         exit={code} stdout={stdout:?} stderr={stderr:?}"
    );
    let _ = std::fs::remove_dir_all(&home);
}
