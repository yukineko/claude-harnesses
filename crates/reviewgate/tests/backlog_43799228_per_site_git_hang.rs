#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Audit probe for backlog 43799228: `git_timeout_boundary.rs` hangs the three
//! diff-content fetches (`diff --`, `diff --cached --`, `ls-files ... --`)
//! TOGETHER, so deleting the Undetermined arm of any ONE site is not observed
//! by the hang tests. These cases hang exactly one site each and require the
//! bounded `git-scan-failed` block, so each site's timeout path is pinned alone.
//!
//! Written by an auditor who did not write the production code (CLAUDE.md 2(a)).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

const SCAN_FAILED: &str = "変更内容を特定できませんでした";

fn unique_dir(tag: &str) -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let p = std::env::temp_dir().join(format!("rg-persite-{}-{}-{}", tag, std::process::id(), n));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn real_git() -> String {
    let out = Command::new("sh")
        .args(["-c", "command -v git"])
        .output()
        .unwrap();
    assert!(out.status.success());
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn git(dir: &Path, args: &[&str]) {
    assert!(Command::new("git")
        .current_dir(dir)
        .args(args)
        .status()
        .unwrap()
        .success());
}

/// A repo with: one tracked-and-modified file (unstaged diff), one staged
/// change, and one untracked file — so every one of the three content fetches
/// has something to fetch.
fn make_repo(root: &Path) -> PathBuf {
    let work = root.join("work");
    std::fs::create_dir_all(&work).unwrap();
    git(&work, &["init", "-q"]);
    git(&work, &["config", "user.email", "t@t.com"]);
    git(&work, &["config", "user.name", "t"]);
    std::fs::write(work.join("a.rs"), "fn a() {}\n").unwrap();
    std::fs::write(work.join("b.rs"), "fn b() {}\n").unwrap();
    git(&work, &["add", "."]);
    git(&work, &["commit", "-qm", "init"]);
    std::fs::write(work.join("a.rs"), "fn a() -> u8 { 1 }\n").unwrap();
    std::fs::write(work.join("b.rs"), "fn b() -> u8 { 2 }\n").unwrap();
    git(&work, &["add", "b.rs"]);
    std::fs::write(work.join("c.rs"), "fn c() {}\n").unwrap();
    work
}

/// fake git: hang iff `cond` (a POSIX `[ ]` test body over "$@") holds.
fn fake_git(dir: &Path, cond: &str) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let body = format!(
        "#!/bin/sh\nhas_dd=0\nfor a in \"$@\"; do [ \"$a\" = \"--\" ] && has_dd=1; done\n\
         has_cached=0\nfor a in \"$@\"; do [ \"$a\" = \"--cached\" ] && has_cached=1; done\n\
         if {cond}; then exec /bin/sleep 20; fi\nexec '{real}' \"$@\"\n",
        real = real_git()
    );
    let p = dir.join("git");
    std::fs::write(&p, body).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    dir.to_path_buf()
}

fn review(work: &Path, home: &Path, fake: &Path) -> (String, String, bool) {
    let ambient = std::env::var_os("PATH").unwrap_or_default();
    let mut paths = vec![fake.to_path_buf()];
    paths.extend(std::env::split_paths(&ambient));
    let payload = format!(
        r#"{{"hook_event_name":"Stop","session_id":"persite","stop_hook_active":false,"cwd":{}}}"#,
        serde_json::to_string(&work.to_string_lossy()).unwrap()
    );
    let mut child = Command::new(env!("CARGO_BIN_EXE_reviewgate"))
        .arg("review")
        .current_dir(work)
        .env("HOME", home)
        .env("PATH", std::env::join_paths(paths).unwrap())
        .env("REVIEWGATE_GIT_TIMEOUT_MS", "300")
        .env_remove("REVIEWGATE_DISABLE")
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
    let start = Instant::now();
    let mut timed_out = false;
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if start.elapsed() > Duration::from_secs(15) {
            timed_out = true;
            let _ = child.kill();
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let o = child.wait_with_output().unwrap();
    (
        String::from_utf8_lossy(&o.stdout).into_owned(),
        String::from_utf8_lossy(&o.stderr).into_owned(),
        timed_out,
    )
}

fn run_case(tag: &str, cond: &str, needle: &str) {
    let root = unique_dir(tag);
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let work = make_repo(&root);
    let fake = fake_git(&root.join("fakebin"), cond);
    let (stdout, stderr, timed_out) = review(&work, &home, &fake);
    assert!(
        !timed_out,
        "hung {tag}: gate did not return. stderr: {stderr}"
    );
    assert!(
        stdout.contains(SCAN_FAILED),
        "{tag} hang must resolve to the git-scan-failed block. stdout: {stdout:?} stderr: {stderr:?}"
    );
    assert!(
        stderr.contains("timed out after") && stderr.contains(needle),
        "{tag}: the failure must name the timeout AND this site ({needle:?}). stderr: {stderr:?}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// Observation for backlog 4f81182f (NOT a verdict on whether it is right): a
/// tracked TEXT file whose edit contains invalid UTF-8 (no NUL, so git diffs it
/// as text) currently makes reviewgate answer the git-scan-failed block instead
/// of reviewing the diff.
#[test]
fn observe_non_utf8_tracked_text_diff_current_behaviour() {
    let root = unique_dir("nonutf8");
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let work = make_repo(&root);
    std::fs::write(work.join("a.rs"), b"fn a() { // caf\xe9 \xff\xfe\n}\n").unwrap();
    let fake = fake_git(&root.join("fakebin"), "false");
    let (stdout, stderr, timed_out) = review(&work, &home, &fake);
    assert!(!timed_out);
    assert!(
        stdout.contains(SCAN_FAILED),
        "OBSERVED current behaviour changed: stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(
        stderr.contains("UTF-8") || stderr.contains("utf-8") || stderr.contains("utf8"),
        "failure reason should name the decode problem. stderr={stderr:?}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn only_unstaged_diff_fetch_hangs() {
    run_case(
        "diff",
        "[ \"$1\" = diff ] && [ $has_dd = 1 ] && [ $has_cached = 0 ]",
        "git diff --:",
    );
}

#[test]
fn only_staged_diff_fetch_hangs() {
    run_case(
        "diffcached",
        "[ \"$1\" = diff ] && [ $has_dd = 1 ] && [ $has_cached = 1 ]",
        "git diff --cached --:",
    );
}

#[test]
fn only_untracked_ls_files_fetch_hangs() {
    run_case(
        "lsfiles",
        "[ \"$1\" = ls-files ] && [ $has_dd = 1 ]",
        "git ls-files --others:",
    );
}
