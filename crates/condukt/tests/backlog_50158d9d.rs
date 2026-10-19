// Integration test: unwrap/expect/panic are allowed (the workspace lint denies
// them for production code only).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! backlog 50158d9d: `condukt worktree merge` reads the task branch's creation
//! base (`branch.<b>.conduktBase`, backlog 51595c41) with `git config --get`
//! and no `--local`, so a value in the user's GLOBAL gitconfig is honoured as if
//! `condukt worktree create` had recorded it in the repository.
//!
//! The 51595c41 contract (crates/condukt/src/worktree.rs, doc of
//! `creation_base_key`): the record lives in the repository's COMMON git
//! config, written by `worktree create`; a branch "cut by hand" has no record
//! and the merge refuses with `merge-base-unrecorded`. This pins that a
//! global-only value is not such a record: a hand-cut branch from a feature
//! branch, with a global `conduktBase` naming a commit on main, must still be
//! refused and must not land the feature-only history on main.

use std::path::Path;
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
        .env_remove("GIT_CONFIG_GLOBAL")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("CLAUDE_CODE_SESSION_ID", "sess-50158d9d")
        .env("CONDUKT_DEFAULT_BRANCH", "main")
        .env_remove("CONDUKT_DISABLE")
        .output()
        .expect("spawn condukt")
}

#[test]
fn a_global_gitconfig_conduktbase_is_not_a_creation_base_record() {
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

    // Feature-only history, and a task branch cut BY HAND from it (no
    // `condukt worktree create`, so no repository-local conduktBase record).
    run_git(&repo, &["checkout", "-q", "-b", "feature"]);
    std::fs::write(repo.join("feature_only.txt"), "not for main\n").unwrap();
    run_git(&repo, &["add", "feature_only.txt"]);
    run_git(&repo, &["commit", "-q", "-m", "feature-only"]);
    let feature_only = run_git(&repo, &["rev-parse", "HEAD"]);
    run_git(&repo, &["checkout", "-q", "-b", "handcut"]);
    std::fs::write(repo.join("task.txt"), "task\n").unwrap();
    run_git(&repo, &["add", "task.txt"]);
    run_git(&repo, &["commit", "-q", "-m", "task"]);
    run_git(&repo, &["checkout", "-q", "main"]);

    // Precondition: nothing in the repository records a creation base.
    let local = git(
        &repo,
        &["config", "--local", "--get", "branch.handcut.conduktBase"],
    );
    assert!(
        !local.status.success(),
        "fixture: the repository must hold no conduktBase record"
    );

    // The user's GLOBAL gitconfig names a commit on main as the "base".
    std::fs::write(
        home.join(".gitconfig"),
        format!("[branch \"handcut\"]\n\tconduktBase = {main_before}\n"),
    )
    .unwrap();

    let out = condukt(&repo, &home, &["worktree", "merge", "--branch", "handcut"]);
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    let main_after = run_git(&repo, &["rev-parse", "main"]);
    let feature_in_main = git(
        &repo,
        &["merge-base", "--is-ancestor", &feature_only, "main"],
    )
    .status
    .success();

    assert!(
        !out.status.success() && stderr.contains("merge-base-unrecorded"),
        "a conduktBase found only in the GLOBAL gitconfig was honoured as the \
         branch's creation base: merge exit={:?}, main {main_before} -> {main_after}, \
         feature-only commit reachable from main = {feature_in_main}; \
         stdout={stdout:?} stderr={stderr:?}",
        out.status.code()
    );
    assert_eq!(main_after, main_before, "a refused merge must leave main");
}
