// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `condukt worktree merge` must only land commits that carry a
//! `Condukt-Task: <run>/<task>` trailer naming the task that owns the branch.
//!
//! The task identity is recorded at `worktree create` time in git config
//! `branch.<b>.conduktTask` (or given via `--run/--task` on merge, which must
//! agree with the record). A commit lacking the trailer, or naming another
//! task, is treated as foreign (e.g. made by a parallel session sharing the
//! branch) and the merge refuses: non-zero exit, main does not move, and
//! stderr lists the full SHA of every offending commit. An undeterminable
//! identity also refuses (fail-closed).

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
        .env("CLAUDE_CODE_SESSION_ID", "sess-task-trailer")
        .env("CONDUKT_DEFAULT_BRANCH", "main")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("CONDUKT_DISABLE")
        .output()
        .expect("spawn condukt")
}

struct Fixture {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    home: PathBuf,
    wt: PathBuf,
    task_branch: String,
    main_before: String,
}

/// Temp repo on `main` with one commit, plus a task worktree created through
/// the CLI as run `runA`, topic `t1`.
fn setup() -> Fixture {
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
        main_before,
    }
}

/// Commit a new file in the worktree; `trailer` = None makes a trailerless
/// commit. Returns the full SHA.
fn commit(wt: &Path, file: &str, trailer: Option<&str>) -> String {
    std::fs::write(wt.join(file), format!("{file}\n")).unwrap();
    run_git(wt, &["add", file]);
    let msg = format!("add {file}");
    match trailer {
        Some(t) => run_git(
            wt,
            &[
                "commit",
                "-q",
                "-m",
                &msg,
                "--trailer",
                &format!("Condukt-Task: {t}"),
            ],
        ),
        None => run_git(wt, &["commit", "-q", "-m", &msg]),
    };
    let sha = run_git(wt, &["rev-parse", "HEAD"]);
    assert_eq!(sha.len(), 40, "full sha expected: {sha}");
    sha
}

struct MergeResult {
    ok: bool,
    stdout: String,
    stderr: String,
    main_after: String,
}

fn merge(f: &Fixture, extra: &[&str]) -> MergeResult {
    let mut args = vec!["worktree", "merge", "--branch", f.task_branch.as_str()];
    args.extend_from_slice(extra);
    let out = condukt(&f.repo, &f.home, &args);
    let r = MergeResult {
        ok: out.status.success(),
        stdout: String::from_utf8_lossy(&out.stdout).to_string(),
        stderr: String::from_utf8_lossy(&out.stderr).to_string(),
        main_after: run_git(&f.repo, &["rev-parse", "main"]),
    };
    eprintln!(
        "merge {extra:?} exit={:?} main {} -> {}\nstdout={}\nstderr={}",
        out.status.code(),
        f.main_before,
        r.main_after,
        r.stdout,
        r.stderr
    );
    r
}

fn assert_refused(f: &Fixture, r: &MergeResult) {
    assert!(
        !r.ok,
        "merge must refuse; stdout={:?} stderr={:?}",
        r.stdout, r.stderr
    );
    assert!(
        !r.stderr.contains("branch not found"),
        "fixture precondition: merge must see the task branch; stderr={:?}",
        r.stderr
    );
    assert_eq!(
        r.main_after, f.main_before,
        "a refused merge must leave main where it was"
    );
}

fn assert_merged(f: &Fixture, r: &MergeResult, tip: &str) {
    assert!(
        r.ok,
        "merge must succeed; stdout={:?} stderr={:?}",
        r.stdout, r.stderr
    );
    assert!(
        git(&f.repo, &["merge-base", "--is-ancestor", tip, "main"])
            .status
            .success(),
        "branch tip {tip} must be reachable from main after merge (main={})",
        r.main_after
    );
}

#[test]
fn foreign_trailerless_commit_is_refused() {
    let f = setup();
    let own = commit(&f.wt, "a.txt", Some("runA/t1"));
    let foreign = commit(&f.wt, "b.txt", None);
    let r = merge(&f, &[]);
    assert_refused(&f, &r);
    assert!(
        r.stderr.contains(&foreign),
        "stderr must list the trailerless commit {foreign}; stderr={:?}",
        r.stderr
    );
    assert!(
        !r.stderr.contains(&own),
        "stderr must NOT list the correctly trailered commit {own}; stderr={:?}",
        r.stderr
    );
}

#[test]
fn wrong_task_trailer_is_refused() {
    let f = setup();
    let own = commit(&f.wt, "a.txt", Some("runA/t1"));
    let foreign = commit(&f.wt, "b.txt", Some("runB/t9"));
    let r = merge(&f, &[]);
    assert_refused(&f, &r);
    assert!(
        r.stderr.contains(&foreign),
        "stderr must list the wrong-task commit {foreign}; stderr={:?}",
        r.stderr
    );
    assert!(
        !r.stderr.contains(&own),
        "stderr must NOT list the correctly trailered commit {own}; stderr={:?}",
        r.stderr
    );
}

#[test]
fn all_commits_trailered_merges() {
    let f = setup();
    commit(&f.wt, "a.txt", Some("runA/t1"));
    let tip = commit(&f.wt, "b.txt", Some("runA/t1"));
    let r = merge(&f, &[]);
    assert_merged(&f, &r, &tip);
}

#[test]
fn identity_undetermined_is_refused() {
    let f = setup();
    // Remove the recorded task identity (may already be absent: ignore failure).
    let _ = git(
        &f.repo,
        &[
            "config",
            "--unset",
            &format!("branch.{}.conduktTask", f.task_branch),
        ],
    );
    assert!(
        !git(
            &f.repo,
            &[
                "config",
                "--get",
                &format!("branch.{}.conduktTask", f.task_branch)
            ],
        )
        .status
        .success(),
        "fixture precondition: conduktTask record must be absent"
    );
    commit(&f.wt, "a.txt", Some("runA/t1"));
    let r = merge(&f, &[]);
    assert_refused(&f, &r);
}

#[test]
fn cli_identity_mismatch_is_refused() {
    let f = setup();
    commit(&f.wt, "a.txt", Some("runA/t1"));
    commit(&f.wt, "b.txt", Some("runA/t1"));
    let r = merge(&f, &["--run", "runA", "--task", "t2"]);
    assert_refused(&f, &r);
}

#[test]
fn cli_identity_matching_merges() {
    let f = setup();
    commit(&f.wt, "a.txt", Some("runA/t1"));
    let tip = commit(&f.wt, "b.txt", Some("runA/t1"));
    let r = merge(&f, &["--run", "runA", "--task", "t1"]);
    assert_merged(&f, &r, &tip);
}
