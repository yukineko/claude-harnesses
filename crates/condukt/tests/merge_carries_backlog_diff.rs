// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! User ruling (2026-10-24): `.backlog` store changes made by `backlog
//! done/fail/add` are left UNCOMMITTED and ride along with the next work or
//! merge commit. `condukt worktree merge` therefore carries a PENDING
//! `.backlog` diff of the source branch's worktree into the merge on the
//! default branch, and must never produce a `.backlog`-only commit.
//!
//! Two variants: the branch is fast-forward-able, or main moved first so a
//! true merge is needed. In both: the merge succeeds, main's tree contains the
//! uncommitted `.backlog` change, and no commit reachable from the base to
//! main changes ONLY `.backlog/` paths (first-parent diff for merges; a
//! correct merge commit carries the work change AND the `.backlog` change, so
//! it is not a violation).
//!
//! Written by an independent test author (CLAUDE.md 2(a)); helpers mirror
//! `task_trailer_merge.rs`.

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
        .env("CLAUDE_CODE_SESSION_ID", "sess-merge-backlog")
        .env("CONDUKT_DEFAULT_BRANCH", "main")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("CONDUKT_DISABLE")
        .output()
        .expect("spawn condukt")
}

const STORE: &str = ".backlog/tasks.toml";
const STORE_BEFORE: &str = "# store v0\n";
const STORE_AFTER: &str = "# store v1: task closed (uncommitted)\n";

struct Fixture {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    home: PathBuf,
    wt: PathBuf,
    task_branch: String,
    base: String,
}

/// Temp repo on `main` with a TRACKED `.backlog/tasks.toml`, plus a task
/// worktree created through the CLI as run `runA`, topic `t1`.
fn setup() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    let home = tmp.path().join("home");
    std::fs::create_dir_all(repo.join(".backlog")).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    run_git(&repo, &["init", "-q", "-b", "main"]);
    run_git(&repo, &["config", "user.email", "t@t.t"]);
    run_git(&repo, &["config", "user.name", "t"]);
    std::fs::write(repo.join("base.txt"), "base\n").unwrap();
    std::fs::write(repo.join(STORE), STORE_BEFORE).unwrap();
    run_git(&repo, &["add", "."]);
    run_git(&repo, &["commit", "-q", "-m", "init"]);
    let base = run_git(&repo, &["rev-parse", "main"]);

    let out = condukt(
        &repo,
        &home,
        &[
            "worktree", "create", "--run", "runA", "--topic", "t1", "--branch", "t1",
        ],
    );
    assert!(out.status.success(), "worktree create: {out:?}");
    let wt = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim().to_string());
    let task_branch = run_git(&wt, &["branch", "--show-current"]);
    assert!(!task_branch.is_empty(), "task worktree must be on a branch");
    Fixture {
        _tmp: tmp,
        repo,
        home,
        wt,
        task_branch,
        base,
    }
}

/// Commit a work file in the worktree with the task trailer.
fn commit_work(wt: &Path, file: &str) {
    std::fs::write(wt.join(file), format!("{file}\n")).unwrap();
    run_git(wt, &["add", file]);
    run_git(
        wt,
        &[
            "commit",
            "-q",
            "-m",
            &format!("add {file}"),
            "--trailer",
            "Condukt-Task: runA/t1",
        ],
    );
}

/// Leave an UNCOMMITTED edit to the tracked store file in the worktree.
fn dirty_store(wt: &Path) {
    assert!(
        wt.join(STORE).exists(),
        "fixture precondition: the store file is tracked in the task worktree"
    );
    std::fs::write(wt.join(STORE), STORE_AFTER).unwrap();
    let st = run_git(wt, &["status", "--porcelain=v1"]);
    assert_eq!(
        st.trim(),
        format!("M {STORE}"),
        "fixture precondition: only the store is dirty"
    );
}

fn merge_and_check(f: &Fixture) {
    let out = condukt(
        &f.repo,
        &f.home,
        &["worktree", "merge", "--branch", f.task_branch.as_str()],
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    eprintln!(
        "merge exit={:?}\nstdout={stdout}\nstderr={stderr}",
        out.status.code()
    );
    assert!(
        out.status.success(),
        "merge must succeed; stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(
        !stderr.contains("branch not found"),
        "fixture precondition: merge must see the task branch; stderr={stderr:?}"
    );

    // The work landed (guards against a vacuous pass).
    assert!(
        git(&f.repo, &["cat-file", "-e", "main:w.txt"])
            .status
            .success(),
        "the work change must be on main"
    );

    // No commit since the base changes ONLY .backlog/ paths.
    let range = format!("{}..main", f.base);
    let revs = run_git(&f.repo, &["rev-list", &range]);
    let mut violations = Vec::new();
    for h in revs.lines() {
        let parent = format!("{h}^1");
        let names = run_git(&f.repo, &["diff", "--name-only", &parent, h]);
        let paths: Vec<&str> = names.lines().filter(|l| !l.is_empty()).collect();
        if !paths.is_empty() && paths.iter().all(|p| p.starts_with(".backlog/")) {
            violations.push(format!("{h}: {paths:?}"));
        }
    }
    assert!(
        violations.is_empty(),
        "no .backlog-only commit may exist on main: {violations:?}"
    );

    // The pending store change rode along into main's tree.
    let on_main = run_git(&f.repo, &["show", &format!("main:{STORE}")]);
    assert_eq!(
        on_main.trim(),
        STORE_AFTER.trim(),
        "main's tree must contain the carried .backlog change"
    );
}

#[test]
fn fast_forward_merge_carries_the_pending_backlog_diff() {
    let f = setup();
    commit_work(&f.wt, "w.txt");
    dirty_store(&f.wt);
    merge_and_check(&f);
}

#[test]
fn true_merge_carries_the_pending_backlog_diff() {
    let f = setup();
    commit_work(&f.wt, "w.txt");
    dirty_store(&f.wt);
    // main moves first: an unrelated commit forces a true merge.
    std::fs::write(f.repo.join("unrelated.txt"), "u\n").unwrap();
    run_git(&f.repo, &["add", "unrelated.txt"]);
    run_git(&f.repo, &["commit", "-q", "-m", "unrelated on main"]);
    merge_and_check(&f);
}
