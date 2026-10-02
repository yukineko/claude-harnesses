// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! backlog 51595c41: `condukt worktree create` cuts the task branch from the
//! CURRENT HEAD (`git worktree add -b` with no start-point), while
//! `condukt worktree merge` always integrates into `default_branch`. Started
//! from a feature branch, a run therefore carries the feature branch's whole
//! history into main, silently.
//!
//! Which fix is right (a: cut from default_branch, b: merge back to the
//! starting branch, c: refuse when the base is not an ancestor of
//! default_branch) is open on the ticket. All three share one observable:
//! after create-on-feature + merge, a commit that exists ONLY on the feature
//! branch must not have become reachable from main without the operator
//! being told. That is what this pins: either the merge refuses (non-zero)
//! and main does not move, or main does not contain the feature-only commit.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn git(dir: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("spawn git")
}

fn run_git(dir: &Path, args: &[&str]) -> String {
    let out = git(dir, args);
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn condukt(repo: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_condukt"))
        .args(args)
        .current_dir(repo)
        .env("HOME", home)
        .env("CLAUDE_CODE_SESSION_ID", "sess-51595c41")
        .env("CONDUKT_DEFAULT_BRANCH", "main")
        .env_remove("CONDUKT_DISABLE")
        .output()
        .expect("spawn condukt")
}

#[test]
#[ignore = "backlog 51595c41: open defect, remove ignore when fixed"]
fn run_started_on_a_feature_branch_does_not_silently_land_feature_history_in_main() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    run_git(&repo, &["init", "-q", "-b", "main"]);
    run_git(&repo, &["config", "user.email", "t@t.t"]);
    run_git(&repo, &["config", "user.name", "t"]);
    std::fs::write(repo.join("base.txt"), "base\n").unwrap();
    run_git(&repo, &["add", "."]);
    run_git(&repo, &["commit", "-q", "-m", "init"]);
    let main_before = run_git(&repo, &["rev-parse", "main"]);

    // Unreviewed work that lives only on a feature branch.
    run_git(&repo, &["checkout", "-q", "-b", "feature"]);
    std::fs::write(repo.join("feature_only.txt"), "not for main\n").unwrap();
    run_git(&repo, &["add", "feature_only.txt"]);
    run_git(&repo, &["commit", "-q", "-m", "feature-only"]);
    let feature_only = run_git(&repo, &["rev-parse", "HEAD"]);

    // The run is started while the primary tree is on `feature`.
    let out = condukt(
        &repo,
        &home,
        &[
            "worktree", "create", "--run", "runF", "--topic", "t1", "--branch", "b1",
        ],
    );
    assert!(out.status.success(), "worktree create: {out:?}");
    let wt = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim().to_string());
    std::fs::write(wt.join("task.txt"), "task\n").unwrap();
    run_git(&wt, &["add", "task.txt"]);
    run_git(&wt, &["commit", "-q", "-m", "task"]);
    // The task branch as condukt actually named it (it may be run-namespaced).
    // (Not asserted to contain the feature commit: fix (a) cuts it from main.)
    let task_branch = run_git(&wt, &["rev-parse", "--abbrev-ref", "HEAD"]);

    let out = condukt(
        &repo,
        &home,
        &["worktree", "merge", "--branch", &task_branch],
    );
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    let main_after = run_git(&repo, &["rev-parse", "main"]);
    let feature_in_main = git(
        &repo,
        &["merge-base", "--is-ancestor", &feature_only, "main"],
    )
    .status
    .success();
    eprintln!(
        "merge exit={:?} main {main_before}->{main_after} feature_in_main={feature_in_main}\nstdout={stdout}\nstderr={stderr}",
        out.status.code()
    );

    if !out.status.success() {
        assert!(
            !stderr.contains("branch not found"),
            "fixture precondition: the merge must see the task branch; stderr={stderr:?}"
        );
        assert_eq!(
            main_after, main_before,
            "a refused merge must leave main where it was; stdout={stdout:?} stderr={stderr:?}"
        );
        return;
    }
    assert!(
        !feature_in_main,
        "the feature-only commit {feature_only} became reachable from main through a \
         run merge that never said so (main {main_before} -> {main_after}); \
         stdout={stdout:?} stderr={stderr:?}"
    );
}
