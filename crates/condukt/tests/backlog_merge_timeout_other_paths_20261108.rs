// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repros (2026-11-08, backlog 8ada7ad9 / GH #1605 follow-up): the
//! timed-out-merge hazard on the merge paths that the fix for the final real
//! merge in `worktree::merge` (`recover_timed_out_merge`) does NOT cover:
//!
//! - `merge_carrying_store_diff` (the `.backlog` store-diff path of
//!   `condukt worktree merge`), whose `git merge --no-commit --no-ff` is killed
//!   on the hard-coded 45s `GIT_TIMEOUT`;
//! - `resolve_merge`'s `git merge -X theirs` (`condukt worktree resolve-merge`
//!   after a recorded `theirs` resolution).
//!
//! Each test takes ~50s because the timeout is a non-injectable const in the
//! binary. Written without a fix (RED observed on bfd504a7); un-ignored with the
//! fix for GH #1638 / #1639.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::Mutex;

/// The overwatch store resolves under the process `$HOME`; tests in this file
/// that write it in-process serialize the swap.
static HOME_LOCK: Mutex<()> = Mutex::new(());

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
        .env("CLAUDE_CODE_SESSION_ID", "sess-merge-timeout-other")
        .env("CONDUKT_DEFAULT_BRANCH", "main")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("CONDUKT_DISABLE")
        .output()
        .expect("spawn condukt")
}

struct Fixture {
    _tmp: tempfile::TempDir,
    base: PathBuf,
    repo: PathBuf,
    home: PathBuf,
    wt: PathBuf,
    task_branch: String,
}

/// main with `shared.txt`; a task worktree (`runT/t1`) that edits it and
/// commits with the task trailer; then main edits it differently, so a merge
/// is a real 3-way content merge of `shared.txt`.
fn fixture(main_line: &str, task_line: &str) -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().canonicalize().unwrap();
    let repo = base.join("repo");
    let home = base.join("home");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    run_git(&repo, &["init", "-q", "-b", "main"]);
    run_git(&repo, &["config", "user.email", "t@t.t"]);
    run_git(&repo, &["config", "user.name", "t"]);
    run_git(&repo, &["config", "commit.gpgsign", "false"]);
    std::fs::write(repo.join("shared.txt"), "a\nb\nc\n").unwrap();
    run_git(&repo, &["add", "."]);
    run_git(&repo, &["commit", "-q", "-m", "init"]);

    let out = condukt(
        &repo,
        &home,
        &[
            "worktree", "create", "--run", "runT", "--topic", "t1", "--branch", "b1",
        ],
    );
    assert!(out.status.success(), "worktree create: {out:?}");
    let wt = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
    std::fs::write(wt.join("shared.txt"), task_line).unwrap();
    std::fs::write(wt.join("task.txt"), "task\n").unwrap();
    run_git(&wt, &["add", "shared.txt", "task.txt"]);
    run_git(
        &wt,
        &["commit", "-q", "-m", "task", "-m", "Condukt-Task: runT/t1"],
    );
    let task_branch = run_git(&wt, &["rev-parse", "--abbrev-ref", "HEAD"]);

    std::fs::write(repo.join("shared.txt"), main_line).unwrap();
    run_git(&repo, &["add", "shared.txt"]);
    run_git(&repo, &["commit", "-q", "-m", "main moves"]);
    Fixture {
        _tmp: tmp,
        base,
        repo,
        home,
        wt,
        task_branch,
    }
}

/// Snapshot of the target tree after a timed-out merge, plus the contract
/// check shared by every path: a timed-out merge must leave the tree clean, or
/// an explicit merge in progress, or say in its output that it is half-applied
/// / name the recovery; and it must never leave a stale `index.lock` without
/// naming it.
fn assert_not_silently_half_applied(repo: &Path, head_before: &str, out: &Output) {
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    let index_lock = repo.join(".git/index.lock").exists();
    let merge_head = repo.join(".git/MERGE_HEAD").exists();
    eprintln!(
        "exit={:?}\nstdout={stdout}\nstderr={stderr}\nobservation: .git/index.lock present = \
         {index_lock}, .git/MERGE_HEAD present = {merge_head}",
        out.status.code()
    );
    assert!(
        stderr.contains("timed out") || stdout.contains("timed out"),
        "fixture precondition: the merge must have timed out; stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(
        !out.status.success(),
        "a timed-out merge must exit non-zero"
    );
    let said = format!("{stdout}{stderr}").to_lowercase();
    assert!(
        !index_lock || said.contains("index.lock"),
        "timed-out merge left a stale .git/index.lock in the target tree without naming it \
         (every later git command there fails); stderr={stderr:?}"
    );
    if index_lock {
        // Nothing below can be probed with the lock in place; the lock check
        // above is the whole verdict for that state.
        return;
    }
    let head_after = run_git(repo, &["rev-parse", "HEAD"]);
    assert_eq!(head_after, head_before, "a failed merge must not move HEAD");
    let staged = run_git(repo, &["diff", "--cached", "--name-only"]);
    let reported =
        said.contains("merge --abort") || said.contains("reset --merge") || said.contains("half");
    assert!(
        staged.is_empty() || merge_head || reported,
        "timed-out merge left the target tree HALF-APPLIED and unannounced: staged={staged:?}, \
         MERGE_HEAD present={merge_head}; stderr={stderr:?}"
    );
}

/// `merge_carrying_store_diff`: the source worktree carries an uncommitted
/// `.backlog` diff, so `condukt worktree merge` takes the store-diff path. A
/// merge driver for `shared.txt` returns at once on its first call (the trial
/// merge) and sleeps past the timeout on the second (the store-diff path's own
/// `git merge --no-commit --no-ff`).
#[test]
fn timed_out_store_diff_merge_is_not_left_silently_half_applied() {
    let f = fixture("a\nmain\nc\n", "a\nb\ntask\n");
    let marker = f.base.join("driver-called-once");
    let driver = f.base.join("slow-merge-driver.sh");
    std::fs::write(
        &driver,
        format!(
            "#!/bin/sh\nif [ -e '{m}' ]; then sleep 120; fi\ntouch '{m}'\nexit 0\n",
            m = marker.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&driver, std::fs::Permissions::from_mode(0o755)).unwrap();
    run_git(
        &f.repo,
        &[
            "config",
            "merge.slow.driver",
            &format!("{} %O %A %B", driver.display()),
        ],
    );
    std::fs::write(
        f.repo.join(".git/info/attributes"),
        "shared.txt merge=slow\n",
    )
    .unwrap();

    std::fs::create_dir_all(f.wt.join(".backlog")).unwrap();
    std::fs::write(f.wt.join(".backlog/pending.jsonl"), "{\"x\":1}\n").unwrap();

    let head_before = run_git(&f.repo, &["rev-parse", "HEAD"]);
    let out = condukt(
        &f.repo,
        &f.home,
        &[
            "worktree",
            "merge",
            "--branch",
            &f.task_branch,
            "--run",
            "runT",
            "--task",
            "t1",
        ],
    );
    assert!(
        marker.exists(),
        "fixture precondition: the trial merge must have called the merge driver"
    );
    assert_not_silently_half_applied(&f.repo, &head_before, &out);
}

/// `resolve_merge`'s `git merge -X theirs`: a real conflict is recorded by
/// `condukt worktree merge`, a `theirs` resolution is recorded, then
/// `condukt worktree resolve-merge` runs the `-X theirs` merge, whose
/// merge-commit step is slowed past the timeout by a `pre-merge-commit` hook
/// (git has by then applied the merge result to index + work tree).
#[test]
fn timed_out_resolve_theirs_merge_is_not_left_silently_half_applied() {
    let f = fixture("a\nmain\nc\n", "a\ntask\nc\n");
    let out = condukt(
        &f.repo,
        &f.home,
        &[
            "worktree",
            "merge",
            "--branch",
            &f.task_branch,
            "--run",
            "runT",
            "--task",
            "t1",
        ],
    );
    let so = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        out.status.success(),
        "fixture precondition: the conflicting merge must be held for review (exit 0): {out:?}"
    );
    let conflict_id = format!("{}/merge-conflict", f.task_branch);
    {
        let _g = HOME_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let prev = std::env::var_os("HOME");
        std::env::set_var("HOME", &f.home);
        let recorded = overwatch::store::append_merge_conflict_resolution(
            &f.repo,
            &overwatch::merge_conflict::MergeConflictResolution {
                conflict_id: conflict_id.clone(),
                choice: overwatch::merge_conflict::ResolveChoice::Theirs,
                decided_by: overwatch::merge_conflict::DecidedBy::Human,
                note: None,
                ts: overwatch::store::now(),
            },
        );
        match prev {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        recorded.expect("record theirs resolution");
    }
    eprintln!("merge (held) stdout={so}");

    let hook = f.repo.join(".git/hooks/pre-merge-commit");
    std::fs::write(&hook, "#!/bin/sh\nsleep 120\n").unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();

    let head_before = run_git(&f.repo, &["rev-parse", "HEAD"]);
    let out = condukt(
        &f.repo,
        &f.home,
        &["worktree", "resolve-merge", "--id", &conflict_id],
    );
    assert_not_silently_half_applied(&f.repo, &head_before, &out);
}

// No RED for `resolve_merge`'s `git merge -s ours`: the same fixture with an
// `ours` resolution and the same `pre-merge-commit` sleep was run on
// 2026-11-08 and the contract above HELD (stderr "timed out", staged="", no
// MERGE_HEAD, no index.lock): `-s ours` stages HEAD's own tree, so the killed
// merge has nothing half-applied to leave, and it runs no merge driver (the
// lever that leaves index.lock on the store-diff path). Not committed as a
// test because it does not fail.
