//! Ruling (user): taskprog hooks print NOTHING, user-visible or model-injected.
//! Stop and SessionStart must leave stdout and stderr empty and exit 0.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

fn scratch(tag: &str) -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let id = N.fetch_add(1, Ordering::Relaxed);
    let d = std::env::temp_dir().join(format!("taskprog-silent-{tag}-{}-{id}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d.canonicalize().unwrap()
}

fn git_root() -> PathBuf {
    let d = scratch("repo");
    std::fs::create_dir_all(d.join(".git")).unwrap();
    d
}

fn run(args: &[&str], cwd: &Path, home: &Path) -> (i32, String, String) {
    let payload = serde_json::json!({"cwd": cwd.to_string_lossy(), "session_id": "s1"}).to_string();
    let mut child = Command::new(env!("CARGO_BIN_EXE_taskprog"))
        .args(args)
        .current_dir(cwd)
        .env("HOME", home)
        .env_remove("TASKPROG_DISABLED")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.as_bytes())
        .unwrap();
    let o = child.wait_with_output().unwrap();
    (
        o.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&o.stdout).into_owned(),
        String::from_utf8_lossy(&o.stderr).into_owned(),
    )
}

fn assert_silent(label: &str, r: &(i32, String, String)) {
    assert_eq!(r.0, 0, "{label}: exit code");
    assert!(r.1.trim().is_empty(), "{label}: stdout was {:?}", r.1);
    assert!(r.2.trim().is_empty(), "{label}: stderr was {:?}", r.2);
}

/// A directory with no `.git` in any ancestor (asserted, not assumed).
fn no_root_dir() -> PathBuf {
    let d = scratch("noroot");
    let mut p = d.clone();
    while let Some(par) = p.parent() {
        assert!(
            !par.join(".git").exists(),
            "precondition: .git above {}",
            par.display()
        );
        p = par.to_path_buf();
    }
    d
}

#[test]
fn stop_is_silent_when_progress_file_exists() {
    let root = git_root();
    let home = scratch("home");
    std::fs::create_dir_all(root.join(".claude")).unwrap();
    std::fs::write(root.join(".claude/progress.md"), "# Progress\nreal notes\n").unwrap();
    assert_silent("stop/existing", &run(&["stop"], &root, &home));
}

#[test]
fn stop_is_silent_but_still_seeds_skeleton_when_file_missing() {
    let root = git_root();
    let home = scratch("home");
    let path = root.join(".claude/progress.md");
    assert!(!path.exists(), "precondition");
    assert_silent("stop/missing", &run(&["stop"], &root, &home));
    let got = std::fs::read_to_string(&path).expect("skeleton must still be seeded");
    assert!(got.contains("## Done"), "skeleton lacks ## Done: {got}");
}

#[test]
fn stop_is_silent_when_not_anchored_and_writes_nothing() {
    let dir = no_root_dir();
    let home = scratch("home");
    assert_silent("stop/not-anchored", &run(&["stop"], &dir, &home));
    assert!(!dir.join(".claude").exists(), "wrote into unanchored cwd");
}

#[test]
fn session_start_is_silent_when_progress_file_exists() {
    let root = git_root();
    let home = scratch("home");
    std::fs::create_dir_all(root.join(".claude")).unwrap();
    std::fs::write(root.join(".claude/progress.md"), "# Progress\nMARKER-ABC\n").unwrap();
    assert_silent(
        "session-start/existing",
        &run(&["session-start"], &root, &home),
    );
}

#[test]
fn session_start_is_silent_when_root_undetermined() {
    let dir = no_root_dir();
    let home = scratch("home");
    assert_silent(
        "session-start/undetermined",
        &run(&["session-start"], &dir, &home),
    );
}
