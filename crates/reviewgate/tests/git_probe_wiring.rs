//! RED wiring test for backlog 7d3db473 (reviewgate's share of it).
//!
//! `reviewgate::git::is_git_repo` collapses "git could not be run" into "not a
//! git repo", and `review::evaluate` has
//! `ChangeScan::NotRepo => return allow("no-git", st)` (review.rs:98). So a
//! Stop hook running without `git` on PATH lets every unreviewed diff through,
//! in a real repo.
//!
//! After the fix the probe must answer `Undetermined` there, `changed_files`
//! must return its BLOCKING variant `ChangeScan::Failed`, and `evaluate` must
//! return `Decision::Block { tag: "git-scan-failed", .. }`.
//!
//! The binary is driven in a CHILD process so the stripped `PATH` is scoped to
//! that child — no process-global env mutation and therefore no race with the
//! in-crate tests that spawn git. (reviewgate is bin-only: no lib target, so an
//! integration test cannot call `git::changed_files` directly; the `Failed`
//! mapping is pinned through its observable consequence, the distinct
//! git-scan block reason.)

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

fn unique_dir(tag: &str) -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let p = std::env::temp_dir().join(format!(
        "reviewgate-probe-{}-{}-{}",
        tag,
        std::process::id(),
        n
    ));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).expect("create scratch dir");
    p
}

fn run_review(cwd: &Path, home: &Path, path: &Path, payload: &str) -> (i32, String) {
    let bin = env!("CARGO_BIN_EXE_reviewgate");
    let mut child = Command::new(bin)
        .arg("review")
        .current_dir(cwd)
        .env("HOME", home)
        .env("PATH", path)
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
    )
}

fn stop_payload(cwd: &Path) -> String {
    format!(
        r#"{{"hook_event_name":"Stop","session_id":"probe-wiring","stop_hook_active":false,"cwd":{}}}"#,
        serde_json::to_string(&cwd.to_string_lossy()).unwrap()
    )
}

fn has_dot_git_ancestor(dir: &Path) -> bool {
    let mut cur = Some(dir);
    while let Some(d) = cur {
        if std::fs::symlink_metadata(d.join(".git")).is_ok() {
            return true;
        }
        cur = d.parent();
    }
    false
}

/// Apparatus: the empty PATH really makes git unreachable for the child.
#[test]
fn empty_path_dir_has_no_git() {
    let root = unique_dir("apparatus");
    let empty_bin = root.join("empty-bin");
    std::fs::create_dir_all(&empty_bin).unwrap();
    assert!(!empty_bin.join("git").exists(), "PATH dir must have no git");
    let probe = Command::new("/usr/bin/env")
        .arg("git")
        .arg("--version")
        .env("PATH", &empty_bin)
        .output();
    let unreachable = match probe {
        Ok(o) => !o.status.success(),
        Err(_) => true,
    };
    assert!(
        unreachable,
        "git must be unreachable under PATH={}, or the block below could pass for the wrong \
         reason",
        empty_bin.display()
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// THE fail-open: git unreachable inside a directory that HAS a `.git`.
#[test]
fn unspawnable_git_in_a_repo_blocks_the_stop() {
    let root = unique_dir("unspawnable");
    let empty_bin = root.join("empty-bin");
    std::fs::create_dir_all(&empty_bin).unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let work = root.join("work");
    std::fs::create_dir_all(work.join(".git")).unwrap();
    std::fs::create_dir_all(work.join("src")).unwrap();
    std::fs::write(work.join("src").join("lib.rs"), "pub fn f() -> u8 { 1 }\n").unwrap();

    let (code, stdout) = run_review(&work, &home, &empty_bin, &stop_payload(&work));

    assert_eq!(
        code, 0,
        "the Stop hook must always exit 0; stdout: {stdout}"
    );
    assert!(
        stdout.contains("\"decision\":\"block\"") || stdout.contains("\"decision\": \"block\""),
        "git unreachable in a real repo ⇒ the change set is UNDETERMINED ⇒ the diff is \
         unreviewed. Allowing here is review.rs:98's `NotRepo => allow(\"no-git\")` fail-open: \
         every stop passes unreviewed whenever the hook runs without git on PATH. stdout: \
         {stdout:?}"
    );
    assert!(
        stdout.contains("変更内容を特定できませんでした"),
        "the block must be the git-scan-undetermined block (tag `git-scan-failed`), not the \
         panic barrier's generic fail-closed block — otherwise a crashing gate would satisfy \
         this test. stdout: {stdout:?}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// Over-block guard: no git AND no `.git` anywhere → still allow.
#[test]
fn unspawnable_git_without_a_dot_git_still_allows() {
    let root = unique_dir("unspawnable-bare");
    let empty_bin = root.join("empty-bin");
    std::fs::create_dir_all(&empty_bin).unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let work = root.join("work");
    std::fs::create_dir_all(&work).unwrap();
    if has_dot_git_ancestor(&work) {
        eprintln!("SKIPPED unspawnable_git_without_a_dot_git_still_allows: .git ancestor present");
        let _ = std::fs::remove_dir_all(&root);
        return;
    }

    let (code, stdout) = run_review(&work, &home, &empty_bin, &stop_payload(&work));

    assert_eq!(code, 0, "Stop hook must exit 0");
    assert!(
        !stdout.contains("\"decision\":\"block\""),
        "nothing corroborates a repo here, so the gate legitimately has no scope; blocking \
         would trap every non-repo session. stdout: {stdout:?}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// Preserved behaviour with a REAL git: a genuine non-repo directory allows.
#[test]
fn genuine_non_repo_with_real_git_still_allows() {
    let Some(path) = std::env::var_os("PATH") else {
        eprintln!("SKIPPED genuine_non_repo_with_real_git_still_allows: no PATH");
        return;
    };
    let root = unique_dir("real-nonrepo");
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let work = root.join("work");
    std::fs::create_dir_all(&work).unwrap();
    if has_dot_git_ancestor(&work) {
        eprintln!("SKIPPED genuine_non_repo_with_real_git_still_allows: .git ancestor present");
        let _ = std::fs::remove_dir_all(&root);
        return;
    }

    let (code, stdout) = run_review(&work, &home, Path::new(&path), &stop_payload(&work));

    assert_eq!(code, 0, "Stop hook must exit 0");
    assert!(
        !stdout.contains("\"decision\":\"block\""),
        "an authoritative 'not a work tree' from a working git must keep allowing. stdout: \
         {stdout:?}"
    );
    let _ = std::fs::remove_dir_all(&root);
}
