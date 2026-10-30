#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro (B1): `backlog list --status <unknown>` exits 0 with stdout
//! "no tasks"; only stderr warns. A script reading the exit code and stdout
//! sees a genuinely empty queue. An unknown filter value is undetermined and
//! must exit non-zero.

mod common;

use std::path::PathBuf;
use std::process::{Command, Stdio};

fn temp_repo(tag: &str) -> (PathBuf, PathBuf) {
    let home = std::env::temp_dir().join(format!(
        "backlog-repro-{}-{}-{}",
        tag,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&home).unwrap();
    let repo = home.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    common::linked_checkout(&repo);
    (home, repo)
}

fn list(home: &PathBuf, repo: &PathBuf, status: &str) -> (i32, String, String) {
    let out = harness_core::git_env::scrub(&mut Command::new(env!("CARGO_BIN_EXE_backlog")))
        .args(["list", "--status", status])
        .env("HOME", home)
        .current_dir(repo)
        .stdin(Stdio::null())
        .output()
        .expect("binary runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn list_with_unknown_status_exits_nonzero() {
    let (home, repo) = temp_repo("b1");
    let (ccode, cout, cerr) = list(&home, &repo, "pending");
    assert_eq!(
        ccode, 0,
        "control: a valid status must exit 0. stdout={cout:?} stderr={cerr:?}"
    );

    let (code, stdout, stderr) = list(&home, &repo, "open");
    assert_ne!(
        code, 0,
        "an unknown --status value is undetermined, not an empty queue. exit={code} stdout={stdout:?} stderr={stderr:?}"
    );
    let _ = std::fs::remove_dir_all(&home);
}
