// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `condukt worktree merge` with a task association must only land commits
//! that carry a `Condukt-Task: <run>/<task>` trailer naming the task that owns
//! the branch.
//!
//! Contract (user rulings 2026-11-15 backlog 3a45a57d, 2026-11-16 scope):
//! the guard applies only when the merge NAMES the task (`--run` and `--task`
//! together, which must agree with any `branch.<b>.conduktTask` record written
//! at `worktree create`). A merge with neither flag has no task association:
//! the guard does not apply, even when a record exists. A half-given pair
//! (only one of the two) is undeterminable and refuses with
//! `merge-task-undetermined`. With the guard on, a commit lacking the trailer
//! or naming another task is foreign: non-zero exit, main does not move, and
//! stderr lists the full SHA of every offending commit.
//!
//! DOES NOT PROVE: `resolve-merge` behaviour (see
//! `backlog_20261116_resolve_merge_task_named_flag.rs`).

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

    // Re-anchored per user ruling 2026-11-20 (spec
    // condukt-merge-proof-contract-7768aeaf R8, round 4 Q-B): a merge naming
    // `--run runA --task t1` now resolves the task in run state (an absent run
    // is `merge-proof-contract(undetermined-task)`). The run holds `t1` as
    // kind=chore, which is outside the proof contract, so these tests keep
    // pinning only the trailer guard; every trailer assert is unchanged.
    let dec = tmp.path().join("dec.json");
    std::fs::write(
        &dec,
        r#"{"goal":"g","tasks":[{"id":"t1","title":"t","kind":"chore"}]}"#,
    )
    .unwrap();
    let out = condukt(
        &repo,
        &home,
        &[
            "state",
            "init",
            "--run",
            "runA",
            "--file",
            dec.to_str().unwrap(),
        ],
    );
    assert!(out.status.success(), "state init: {out:?}");

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

// test-weakening-justified: user ruling 2026-11-15 (backlog 3a45a57d): a merge with no --run/--task has no task association and must not apply the trailer guard; per the 2026-11-16 scope ruling this test now names the task via --run/--task so the guard property stays tested.
#[test]
fn foreign_trailerless_commit_is_refused() {
    let f = setup();
    let own = commit(&f.wt, "a.txt", Some("runA/t1"));
    let foreign = commit(&f.wt, "b.txt", None);
    let r = merge(&f, &["--run", "runA", "--task", "t1"]);
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

// test-weakening-justified: user ruling 2026-11-15 (backlog 3a45a57d): a merge with no --run/--task has no task association and must not apply the trailer guard; per the 2026-11-16 scope ruling this test now names the task via --run/--task so the guard property stays tested.
#[test]
fn wrong_task_trailer_is_refused() {
    let f = setup();
    let own = commit(&f.wt, "a.txt", Some("runA/t1"));
    let foreign = commit(&f.wt, "b.txt", Some("runB/t9"));
    let r = merge(&f, &["--run", "runA", "--task", "t1"]);
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

// test-weakening-justified: user ruling 2026-11-15 (backlog 3a45a57d): a merge with no --run/--task has no task association and must not apply the trailer guard; per the 2026-11-16 scope ruling this test now names the task via --run/--task so the guard property stays tested.
#[test]
fn all_commits_trailered_merges() {
    let f = setup();
    commit(&f.wt, "a.txt", Some("runA/t1"));
    let tip = commit(&f.wt, "b.txt", Some("runA/t1"));
    let r = merge(&f, &["--run", "runA", "--task", "t1"]);
    assert_merged(&f, &r, &tip);
}

// test-weakening-justified: user ruling 2026-11-15 (backlog 3a45a57d) and the 2026-11-16 scope ruling: with no record and no --run/--task there is no task association, so the merge proceeds without a trailer check (the old "refused" pin was the defect); a half-given pair still refuses (see identity_half_given_pair_is_refused).
#[test]
fn identity_undetermined_merges_without_trailer_check() {
    let f = setup();
    unset_record(&f);
    commit(&f.wt, "a.txt", Some("runA/t1"));
    let tip = commit(&f.wt, "b.txt", None);
    let r = merge(&f, &[]);
    assert_merged(&f, &r, &tip);
}

// test-weakening-justified: user ruling 2026-11-15 (backlog 3a45a57d) and the 2026-11-16 scope ruling: the old identity_undetermined_is_refused pin is repinned; the undeterminable case that remains is a half-given --run/--task pair, which must still refuse with merge-task-undetermined.
#[test]
fn identity_half_given_pair_is_refused() {
    for half in [&["--run", "runA"][..], &["--task", "t1"][..]] {
        let f = setup();
        unset_record(&f);
        commit(&f.wt, "a.txt", Some("runA/t1"));
        let r = merge(&f, half);
        assert_refused(&f, &r);
        assert!(
            r.stderr.contains("merge-task-undetermined"),
            "half-given pair {half:?} must refuse with merge-task-undetermined; stderr={:?}",
            r.stderr
        );
    }
}

/// Remove the recorded task identity and assert it is gone.
fn unset_record(f: &Fixture) {
    // May already be absent: ignore failure.
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
