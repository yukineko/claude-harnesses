// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Audit B item 13 (memory `condukt-operator-gotchas`, section "`condukt
//! worktree merge` targets main, not your feature branch"; originally
//! `condukt-worktree-merge-targets-main`), 2026-11-07.
//!
//! `worktree create` cuts the task branch from the CURRENT HEAD. When that
//! HEAD is a feature branch ahead of the default branch, `worktree merge`
//! still integrates into the default branch, and `default..task-branch`
//! carries every not-yet-merged feature commit along with the task's own
//! work. Observed 2026-07-16: the whole feature stack was dragged onto
//! local main. RED: after `worktree merge`, no commit from before the task's
//! base (the feature commit) may be reachable from the default branch.
//! Control: a task cut from the default branch itself merges and lands.
//!
//! Status 2026-11-07: this PASSES at HEAD — `worktree::ensure_base_on_default_branch`
//! (backlog 51595c41) already refuses. Kept as class-A evidence that the
//! memory's `[to be mechanized]` note is stale.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

fn git_out(dir: &Path, args: &[&str]) -> Output {
    harness_core::git_env::scrub(&mut Command::new("git"))
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("git")
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = git_out(dir, args);
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn condukt(dir: &Path, home: &Path, args: &[&str]) -> Output {
    harness_core::git_env::scrub(&mut Command::new(env!("CARGO_BIN_EXE_condukt")))
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("CLAUDE_CODE_SESSION_ID", "sess-b13")
        .env("CONDUKT_DEFAULT_BRANCH", "main")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("CONDUKT_DISABLE")
        .stdin(Stdio::null())
        .output()
        .expect("condukt")
}

fn commit(dir: &Path, file: &str, trailer: Option<&str>) -> String {
    std::fs::write(dir.join(file), format!("{file}\n")).unwrap();
    git(dir, &["add", file]);
    let mut args = vec!["commit", "-q", "-m", file];
    if let Some(t) = trailer {
        args.extend(["--trailer", t]);
    }
    git(dir, &args);
    git(dir, &["rev-parse", "HEAD"])
}

fn is_ancestor(repo: &Path, a: &str, b: &str) -> bool {
    git_out(repo, &["merge-base", "--is-ancestor", a, b])
        .status
        .success()
}

/// Repo on `main`; when `stacked`, HEAD is a feature branch one commit ahead.
/// Returns (repo, feature-commit-or-empty, task-commit, merge output).
fn scenario(tmp: &Path, stacked: bool) -> (PathBuf, String, String, Output) {
    let repo = tmp.join("repo");
    let home = tmp.join("home");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.email", "t@t.t"]);
    git(&repo, &["config", "user.name", "t"]);
    commit(&repo, "base.txt", None);
    let mut feature = String::new();
    if stacked {
        git(&repo, &["checkout", "-q", "-b", "feat"]);
        feature = commit(&repo, "feat.txt", None);
    }
    let out = condukt(
        &repo,
        &home,
        &[
            "worktree", "create", "--run", "runB", "--topic", "t1", "--branch", "t1",
        ],
    );
    assert!(out.status.success(), "worktree create: {out:?}");
    let wt = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim().to_string());
    let branch = git(&wt, &["branch", "--show-current"]);
    let task = commit(&wt, "task.txt", Some("Condukt-Task: runB/t1"));
    let merged = condukt(&repo, &home, &["worktree", "merge", "--branch", &branch]);
    (repo, feature, task, merged)
}

/// Control: a task cut from main merges, and main gains the task commit.
#[test]
fn task_cut_from_default_branch_merges() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, _f, task, out) = scenario(tmp.path(), false);
    assert!(
        out.status.success(),
        "control merge: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(is_ancestor(&repo, &task, "main"), "control: task landed");
}

/// RED: the feature commit (before the task base) must not reach main.
#[test]
fn merge_does_not_drag_commits_from_before_the_task_base_onto_default() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, feature, _task, out) = scenario(tmp.path(), true);
    // Observed 2026-11-07: already enforced (backlog 51595c41, 2fc83c2a) — the
    // merge refuses with this rule id and leaves main untouched.
    assert!(
        !out.status.success()
            && String::from_utf8_lossy(&out.stderr).contains("merge-base-not-on-default-branch"),
        "expected the creation-base refusal; exit {:?}, stderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !is_ancestor(&repo, &feature, "main"),
        "condukt worktree merge integrated the task branch into main and dragged the \
         unmerged feature commit {feature} (cut before the task base) along with it. \
         merge exit {:?}, stderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
}
