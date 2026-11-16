// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! A SUCCESSFUL `fugu-router sync` must clear SessionEnd pending markers whose
//! process is dead.
//!
//! Observed 2026-11-16: a marker from 2026-11-06 (pid 278032, dead) kept
//! printing "a SessionEnd sync of the record store did not finish ... Run
//! `fugu-router sync` to finish it" on every prompt, even after a successful
//! manual `fugu-router sync`. Only the SessionEnd worker removed markers, so
//! the notice's own instruction did not work.
//!
//! Pinned here (CLAUDE.md §3: undetermined resolves to the restrictive side):
//!   1. dead-pid marker + successful sync  -> marker gone, notice absent.
//!   2. alive-pid marker + successful sync -> marker kept (sync in flight).
//!   3. dead-pid marker + FAILING sync     -> marker kept, notice present.
//!
//! The notice is read through the real surface: the `prompt`
//! (UserPromptSubmit) hook's JSON output.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

const EXE: &str = env!("CARGO_BIN_EXE_fugu-router");
const NOTICE: &str = "a SessionEnd sync of the record store did not finish";

fn git(dir: &Path, args: &[&str]) {
    let out = harness_core::git_env::scrub(&mut Command::new("git"))
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn scratch(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("fugu-marker-test-{name}-{}", std::process::id()));
    if root.exists() {
        std::fs::remove_dir_all(&root).unwrap();
    }
    std::fs::create_dir_all(&root).unwrap();
    root
}

fn run(home: &Path, args: &[&str], stdin: &str) -> std::process::Output {
    use std::io::Write;
    let mut child = harness_core::git_env::scrub(&mut Command::new(EXE))
        .args(args)
        .env("HOME", home)
        .env_remove("FUGU_ROUTER_DISABLED")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    drop(child.stdin.take());
    child.wait_with_output().unwrap()
}

/// Fake home whose `sync_repo` is `remote` (a path).
fn home_with_sync_repo(root: &Path, remote: &Path) -> PathBuf {
    let home = root.join("home");
    let cfg_dir = home.join(".fugu-router");
    std::fs::create_dir_all(&cfg_dir).unwrap();
    std::fs::write(
        cfg_dir.join("config.toml"),
        format!("sync_repo = \"{}\"\n", remote.to_str().unwrap()),
    )
    .unwrap();
    home
}

/// Home whose sync succeeds offline: a local bare repo, cloned on first sync.
fn home_sync_ok(name: &str) -> PathBuf {
    let root = scratch(name);
    let remote = root.join("remote.git");
    std::fs::create_dir_all(&remote).unwrap();
    git(&remote, &["init", "--bare", "--initial-branch=main", "."]);
    home_with_sync_repo(&root, &remote)
}

/// Home whose sync fails deterministically offline: the remote does not exist.
fn home_sync_fails(name: &str) -> PathBuf {
    let root = scratch(name);
    home_with_sync_repo(&root, &root.join("no-such-remote.git"))
}

fn dead_pid() -> i64 {
    let mut c = Command::new("true").spawn().unwrap();
    let pid = i64::from(c.id());
    c.wait().unwrap();
    pid
}

fn write_marker(home: &Path, pid: i64) -> PathBuf {
    let dir = home.join(".fugu-router").join("session-end-pending");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("1000000000-{pid}-1.json"));
    std::fs::write(
        &path,
        format!(r#"{{"pid":{pid},"at":1000000000,"phase":"running"}}"#),
    )
    .unwrap();
    path
}

/// Whether the UserPromptSubmit hook currently surfaces the SessionEnd notice.
fn notice_shown(home: &Path) -> bool {
    let out = run(home, &["prompt"], r#"{"prompt":"hello","session_id":"s"}"#);
    String::from_utf8_lossy(&out.stdout).contains(NOTICE)
}

fn spawn_alive() -> Child {
    Command::new("sleep").arg("30").spawn().unwrap()
}

#[test]
fn dead_marker_is_cleared_by_successful_sync() {
    let home = home_sync_ok("dead-ok");
    let marker = write_marker(&home, dead_pid());
    assert!(
        notice_shown(&home),
        "precondition: dead marker must be reported"
    );

    let out = run(&home, &["sync"], "");
    assert!(
        out.status.success(),
        "precondition: sync must succeed. stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    assert!(
        !marker.exists(),
        "a successful `fugu-router sync` left the dead-pid marker {} in place",
        marker.display()
    );
    assert!(
        !notice_shown(&home),
        "the SessionEnd 'did not finish' notice persists after a successful \
         `fugu-router sync`, contradicting its own instruction"
    );
}

#[test]
fn alive_marker_is_kept_by_successful_sync() {
    let home = home_sync_ok("alive-ok");
    let mut child = spawn_alive();
    let marker = write_marker(&home, i64::from(child.id()));

    let out = run(&home, &["sync"], "");
    let kept = marker.exists();
    let _ = child.kill();
    let _ = child.wait();
    assert!(out.status.success(), "precondition: sync must succeed");
    assert!(kept, "sync removed a marker whose process is still alive");
}

#[test]
fn dead_marker_is_kept_when_sync_fails() {
    let home = home_sync_fails("dead-fail");
    let marker = write_marker(&home, dead_pid());

    let out = run(&home, &["sync"], "");
    assert!(
        !out.status.success(),
        "precondition: sync must fail against a nonexistent remote"
    );

    assert!(marker.exists(), "a FAILED sync removed the dead-pid marker");
    assert!(notice_shown(&home), "notice must stay after a failed sync");
}
