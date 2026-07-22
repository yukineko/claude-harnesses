// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED wiring test for backlog 7d3db473 (propguard's share of it).
//!
//! `propguard::git::is_git_repo` is `run_git(root, &["rev-parse",
//! "--is-inside-work-tree"]).is_some()` (git.rs:140) — a spawn failure, a
//! non-zero exit and a timeout all collapse into "not a git repo" — and
//! `gate::evaluate` has `ChangeScan::NotRepo => return allow("no-git", st)`
//! (gate.rs:143). So a Stop hook running without `git` on PATH lets every
//! unchecked change through, in a real repo.
//!
//! After the fix the probe must answer `Undetermined` there, `changed_files`
//! must return its BLOCKING variant `ChangeScan::Failed`, and `evaluate` must
//! return `Decision::Block { tag: "git-scan-failed", .. }`.
//!
//! The binary is driven in a CHILD process so the stripped `PATH` is scoped to
//! that child — no process-global env mutation, so no race with the in-crate
//! tests that spawn git/checkers. (propguard is bin-only: no lib target, so an
//! integration test cannot call `git::changed_files` directly; the `Failed`
//! mapping is pinned through its observable consequence, the distinct git-scan
//! block reason, which is textually different from the ordinary
//! properties-unverified block.)

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

fn unique_dir(tag: &str) -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let p = std::env::temp_dir().join(format!(
        "propguard-probe-{}-{}-{}",
        tag,
        std::process::id(),
        n
    ));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).expect("create scratch dir");
    p
}

fn hook_payload(session: &str, cwd: &Path) -> String {
    serde_json::json!({
        "hook_event_name": "Stop",
        "session_id": session,
        "stop_hook_active": false,
        "cwd": cwd.to_string_lossy(),
    })
    .to_string()
}

/// Run `propguard check` with `payload` on stdin, isolated HOME/state dir, and
/// `path` as the child's PATH. `criteria` is fed via `PROPGUARD_CRITERIA` so the
/// gate gets past its `no-criteria` allow and actually reaches the git scan.
fn run_check(cwd: &Path, home: &Path, path: &Path, criteria: &str, payload: &str) -> (i32, String) {
    let bin = env!("CARGO_BIN_EXE_propguard");
    let mut cmd = Command::new(bin);
    cmd.arg("check")
        .current_dir(cwd)
        .env("HOME", home)
        .env("PROPGUARD_STATE_DIR", home.join(".propguard-state"))
        .env("PROPGUARD_CRITERIA", criteria)
        .env_remove("PROPGUARD_DISABLE")
        .env("PATH", path);
    let mut child = cmd
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

const CRITERIA: &str = "idempotent; never panic; stable output schema";

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
        "git must be unreachable under PATH={}, or the assertions below could pass for the \
         wrong reason",
        empty_bin.display()
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// THE fail-open: git unreachable inside a directory that HAS a `.git`.
#[test]
fn unspawnable_git_in_a_repo_blocks_with_the_scan_failed_reason() {
    let root = unique_dir("unspawnable");
    let empty_bin = root.join("empty-bin");
    std::fs::create_dir_all(&empty_bin).unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let work = root.join("work");
    std::fs::create_dir_all(work.join(".git")).unwrap();
    std::fs::write(work.join("a.rs"), "fn f() { panic!() }\n").unwrap();

    let (code, stdout) = run_check(
        &work,
        &home,
        &empty_bin,
        CRITERIA,
        &hook_payload("probe-1", &work),
    );

    assert_eq!(
        code, 0,
        "the Stop hook must always exit 0; stdout: {stdout}"
    );
    assert!(
        stdout.contains("\"decision\":\"block\"") || stdout.contains("\"decision\": \"block\""),
        "git unreachable in a real repo ⇒ the change set is UNDETERMINED ⇒ the properties are \
         UNVERIFIED. Allowing here is gate.rs:143's `NotRepo => allow(\"no-git\")` fail-open. \
         stdout: {stdout:?}"
    );
    assert!(
        stdout.contains("変更内容を特定できませんでした"),
        "the block must be the git-scan-undetermined block (tag `git-scan-failed`), NOT the \
         ordinary properties-unverified block and NOT the panic barrier's generic fail-closed \
         block — otherwise this test would be satisfied by a gate that merely blocks for some \
         other reason. stdout: {stdout:?}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// Over-block guard: no git AND no `.git` anywhere → still allow (`no-git`).
#[test]
fn unspawnable_git_without_a_dot_git_still_allows() {
    let root = unique_dir("unspawnable-bare");
    let empty_bin = root.join("empty-bin");
    std::fs::create_dir_all(&empty_bin).unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let work = root.join("work");
    std::fs::create_dir_all(&work).unwrap();
    std::fs::write(work.join("a.rs"), "fn f() {}\n").unwrap();
    if has_dot_git_ancestor(&work) {
        eprintln!("SKIPPED unspawnable_git_without_a_dot_git_still_allows: .git ancestor present");
        let _ = std::fs::remove_dir_all(&root);
        return;
    }

    let (code, stdout) = run_check(
        &work,
        &home,
        &empty_bin,
        CRITERIA,
        &hook_payload("probe-2", &work),
    );

    assert_eq!(code, 0, "Stop hook must exit 0");
    assert!(
        !stdout.contains("変更内容を特定できませんでした"),
        "with no git AND no .git anywhere the directory is genuinely out of scope; raising the \
         git-scan block here would trap every non-repo session. stdout: {stdout:?}"
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
    std::fs::write(work.join("a.rs"), "fn f() {}\n").unwrap();
    if has_dot_git_ancestor(&work) {
        eprintln!("SKIPPED genuine_non_repo_with_real_git_still_allows: .git ancestor present");
        let _ = std::fs::remove_dir_all(&root);
        return;
    }

    let (code, stdout) = run_check(
        &work,
        &home,
        Path::new(&path),
        CRITERIA,
        &hook_payload("probe-3", &work),
    );

    assert_eq!(code, 0, "Stop hook must exit 0");
    assert!(
        !stdout.contains("\"decision\":\"block\""),
        "an authoritative 'not a work tree' from a working git must keep allowing (`no-git`); \
         the fix must not turn every non-repo session into a block. stdout: {stdout:?}"
    );
    let _ = std::fs::remove_dir_all(&root);
}
