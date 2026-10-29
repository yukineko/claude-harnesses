// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! backlog 135af7af: the 51595c41 merge-base gate checks only the recorded
//! creation base. A run created on main whose task branch later MERGES an
//! off-default feature branch still lands the feature-only commit in main via
//! `condukt worktree merge`, silently. Same observable as 51595c41: either the
//! merge refuses and main does not move, or main does not contain the
//! feature-only commit.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn git(dir: &Path, args: &[&str]) -> Output {
    harness_core::git_env::scrub(&mut Command::new("git"))
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
    harness_core::git_env::scrub(&mut Command::new(env!("CARGO_BIN_EXE_condukt")))
        .args(args)
        .current_dir(repo)
        .env("HOME", home)
        .env("CLAUDE_CODE_SESSION_ID", "sess-135af7af")
        .env("CONDUKT_DEFAULT_BRANCH", "main")
        .env_remove("CONDUKT_DISABLE")
        .output()
        .expect("spawn condukt")
}

#[test]
fn feature_branch_merged_into_task_branch_after_create_does_not_silently_land_in_main() {
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
    run_git(&repo, &["checkout", "-q", "main"]);

    // The run is started on main (creation base is fine).
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
    run_git(&wt, &["merge", "-q", "--no-edit", "feature"]);
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
