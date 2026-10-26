// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Regression test (backlog e31b1b02): a write into a linked worktree that was
//! removed afterwards used to make `session_roots::resolve` Undetermined, so
//! donegate and tdd blocked EVERY later Stop of the session and the only exit
//! was `donegate skip` / `tdd skip`.
//!
//! Observed (2026-10-26, session 1a4ac77f), on Stop after a flow loop whose
//! condukt worktree had been cleaned up:
//!
//! ```text
//! donegate: BLOCKED - the session's roots cannot be determined (session roots:
//! the session wrote /Users/yuki/.condukt/worktrees/run-20261015-032632-69063-t1/
//! crates/backlog/src/evidence.rs but its directory does not exist, so its git
//! root cannot be determined).
//! ```
//!
//! Undetermined is fail-closed, so this is not a fail-open; it is a liveness
//! defect. The property pinned here: the repository a removed linked worktree
//! belonged to is still attributable, so the roots are Known and contain it.
//! User ruling (2026-10-26): keep attribution, option (a). The worktree to repo
//! mapping is recorded while the worktree still resolves; with no record the
//! result stays Undetermined (fail-closed).

use harness_core::session_roots::resolve;
use harness_core::verdict::Determination;
use std::path::{Path, PathBuf};
use std::process::Command;

fn git(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("git must be runnable for this test")
}

fn ok(out: std::process::Output) {
    assert!(out.status.success(), "git failed: {out:?}");
}

fn canon(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap()
}

/// A JSONL transcript recording a `Write` of `written`.
fn transcript(dir: &Path, written: &Path) -> String {
    let line = serde_json::json!({
        "message": {"role": "assistant", "content": [{
            "type": "tool_use", "id": "toolu_0", "name": "Write",
            "input": {"file_path": written.to_str().unwrap(), "content": "x"}
        }]}
    });
    let path = dir.join("session.jsonl");
    std::fs::write(&path, format!("{line}\n")).unwrap();
    path.to_string_lossy().into_owned()
}

#[test]
fn removed_worktree_write_stays_attributable() {
    let tmp = tempfile::tempdir().unwrap();
    let base = canon(tmp.path());
    let repo = base.join("repo");
    let wt = base.join("wt");
    let tdir = base.join("t");
    for d in [&repo, &tdir] {
        std::fs::create_dir_all(d).unwrap();
    }
    ok(git(&repo, &["init", "-q"]));
    ok(git(
        &repo,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.com",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "init",
        ],
    ));
    ok(git(
        &repo,
        &["worktree", "add", "-q", "-b", "w", wt.to_str().unwrap()],
    ));
    let written = wt.join("src").join("a.rs");
    std::fs::create_dir_all(written.parent().unwrap()).unwrap();
    std::fs::write(&written, "x").unwrap();
    let t = transcript(&tdir, &written);
    let repo_c = canon(&repo);

    // Control: while the worktree exists, the write resolves to a Known root.
    match resolve(&repo, &t) {
        Determination::Known(_) => {}
        Determination::Undetermined(why) => panic!(
            "control: expected Known roots while the worktree exists, got Undetermined: {}",
            why.as_str()
        ),
    }

    ok(git(
        &repo,
        &["worktree", "remove", "--force", wt.to_str().unwrap()],
    ));
    assert!(!wt.exists(), "precondition: the worktree dir must be gone");

    match resolve(&repo, &t) {
        Determination::Known(sr) => assert!(
            sr.roots.contains(&repo_c),
            "Known roots {:?} must contain the repository {repo_c:?}",
            sr.roots
        ),
        Determination::Undetermined(why) => panic!(
            "a write into a removed worktree of {repo_c:?} made the roots Undetermined: {}",
            why.as_str()
        ),
    }
}
