// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Regression tests written by the independent closure verifier (audit batch
//! b1_1, 2026-10-02). Both pin a STILL-OPEN failure, so both are ignored and
//! RED today: a backlog command run with its cwd in a checkout writes that
//! checkout's TRACKED `.backlog/tasks.toml`, leaving the working tree dirty —
//! which on the main tree trips `stop-verify-worktree.py` (CLAUDE.md §8).

mod common;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn unique(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "backlog-audit-v11-{}-{}-{}",
        tag,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn backlog(args: &[&str], home: &Path, cwd: &Path, stdin: &str) -> (i32, String, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_backlog"))
        .env("PATH", common::path_with_condukt_shim())
        .args(args)
        .env("HOME", home)
        .env_remove("BACKLOG_STORE_DIR")
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("backlog spawns");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A git repo whose `.backlog/tasks.toml` holds one task and is COMMITTED.
fn repo_with_committed_store(tag: &str) -> (PathBuf, PathBuf) {
    let base = unique(tag);
    let repo = base.join("repo");
    let home = base.join("home");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.email", "t@t.t"]);
    git(&repo, &["config", "user.name", "t"]);
    let repo_s = repo.to_str().unwrap().to_string();
    let (code, _o, e) = backlog(
        &["add", "--title", "seed task", "--project", &repo_s],
        &home,
        &repo,
        "",
    );
    assert_eq!(code, 0, "seed add: {e}");
    assert!(
        repo.join(".backlog").join("tasks.toml").exists(),
        "store not in repo"
    );
    (repo, home)
}

fn commit_all(repo: &Path) {
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "store"]);
    assert_eq!(
        git(repo, &["status", "--porcelain"]),
        "",
        "fixture must start clean"
    );
}

/// backlog 582c2ac2 (closed as DUPLICATE of 28b6df2c): `backlog add` run from
/// a checkout writes that checkout's tracked store, so following flow's
/// "always file it" rule on the main tree dirties main and trips the §8 Stop
/// gate. Pins the shared failure of both ids.
#[test]
#[ignore = "backlog 28b6df2c / 582c2ac2 OPEN: backlog add dirties the tracked store of the cwd's checkout"]
fn backlog_28b6df2c_582c2ac2_add_does_not_dirty_the_tracked_store() {
    let (repo, home) = repo_with_committed_store("28b6df2c");
    commit_all(&repo);
    let repo_s = repo.to_str().unwrap().to_string();
    let (code, _o, e) = backlog(
        &[
            "add",
            "--title",
            "a finding filed mid-session",
            "--project",
            &repo_s,
        ],
        &home,
        &repo,
        "",
    );
    assert_eq!(code, 0, "add: {e}");
    let status = git(&repo, &["status", "--porcelain"]);
    assert_eq!(status, "", "backlog add left the checkout dirty:\n{status}");
}

/// backlog bf2de0ad: the SessionStart requeue wrote the tracked store of the
/// session's checkout (main), leaving main dirty. f09db5ce moved CLAIMS out of
/// the tracked store, but SessionStart still calls `store::requeue_expired`,
/// which rewrites the tracked file for an expired `fail` deferral. The
/// observable the item names ("main is dirty after SessionStart, through no
/// action of the session") is therefore still reachable.
#[test]
#[ignore = "backlog bf2de0ad OPEN: SessionStart requeue of an expired deferral rewrites the tracked store"]
fn backlog_bf2de0ad_session_start_does_not_dirty_the_tracked_store() {
    let (repo, home) = repo_with_committed_store("bf2de0ad");
    // Turn the seed task into an expired deferral (status failed, defer_until
    // in the past) — the state `fail`-with-defer leaves behind.
    let p = repo.join(".backlog").join("tasks.toml");
    let text = std::fs::read_to_string(&p).unwrap();
    assert!(
        text.contains("status = \"pending\""),
        "unexpected store shape:\n{text}"
    );
    let text = text.replacen(
        "status = \"pending\"",
        "status = \"failed\"\ndefer_until = 1",
        1,
    );
    std::fs::write(&p, text).unwrap();
    commit_all(&repo);

    let input = format!(
        "{{\"session_id\":\"s\",\"cwd\":\"{}\",\"hook_event_name\":\"SessionStart\"}}",
        repo.display()
    );
    let (code, out, err) = backlog(&["session-start"], &home, &repo, &input);
    assert_eq!(code, 0, "session-start: {err}");
    let status = git(&repo, &["status", "--porcelain"]);
    assert_eq!(
        status, "",
        "SessionStart left the checkout dirty:\n{status}\nstdout: {out}\nstderr: {err}"
    );
}
