// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro (2026-11-07): `condukt worktree merge` bounds its final
//! `git merge` by the hard-coded 45s `GIT_TIMEOUT` and, on timeout, kills git
//! mid-merge and just reports the failure. The target worktree (main) is then
//! left HALF-APPLIED: the incoming changes are STAGED in the index while HEAD
//! did not move and there is no MERGE_HEAD, so nothing says "merge in
//! progress" and `git merge --abort` has nothing to abort. The next
//! `git commit` in that tree silently commits someone else's merge content.
//!
//! Observed 2026-11-07 at main e6fc7abf, run run-20261107-114313-25739 task
//! t3: `condukt: merge of condukt/.../t3 into main failed: git ["merge",
//! "--no-edit", "-m", "Merge branch '...'", "3087c5e6..."] in
//! /Users/yuki/src/harness timed out after 45s and was killed`; afterwards
//! main showed `M  scripts/lint-changed-crates.sh` / `M  scripts/test-changed-crates.sh`
//! staged, no MERGE_HEAD. Same merge later succeeded in seconds.
//!
//! Reproduction here: a `pre-merge-commit` hook that sleeps past the timeout
//! (git has by then applied the merge result to index + work tree). The test
//! takes ~50s because the timeout is a non-injectable const in the binary.
//!
//! Contract asserted (either is acceptable): after a TIMED-OUT merge,
//! (a) the target tree is clean (index == HEAD, no staged changes), or
//! (b) the half-merge is explicitly recoverable/reported: a MERGE_HEAD exists
//!     or stderr names the half-applied state with recovery instructions.
//! Silent staged-without-MERGE_HEAD is neither.
//!
//! Written without a fix (RED observed 2026-11-08: `staged="task.txt",
//! MERGE_HEAD present=false`). Fixed by `worktree::recover_timed_out_merge`
//! (backlog 8ada7ad9): the timed-out real merge is rolled back and the
//! rollback is reported; the extra assertions at the end pin that stricter
//! contract. Same fixture observed no `.git/index.lock` left after the kill.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
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
        .env("CLAUDE_CODE_SESSION_ID", "sess-merge-timeout")
        .env("CONDUKT_DEFAULT_BRANCH", "main")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("CONDUKT_DISABLE")
        .output()
        .expect("spawn condukt")
}

#[test]
fn timed_out_merge_does_not_leave_a_silent_half_applied_index() {
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
    std::fs::write(repo.join("base.txt"), "base\n").unwrap();
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
    let wt = std::path::PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
    std::fs::write(wt.join("task.txt"), "task\n").unwrap();
    run_git(&wt, &["add", "task.txt"]);
    run_git(
        &wt,
        &["commit", "-q", "-m", "task", "-m", "Condukt-Task: runT/t1"],
    );
    let task_branch = run_git(&wt, &["rev-parse", "--abbrev-ref", "HEAD"]);

    // main moves on, so the real merge is a true merge commit (not a ff).
    std::fs::write(repo.join("main_only.txt"), "main\n").unwrap();
    run_git(&repo, &["add", "main_only.txt"]);
    run_git(&repo, &["commit", "-q", "-m", "main moves"]);
    let head_before = run_git(&repo, &["rev-parse", "HEAD"]);

    // Slow the merge-commit step past the 45s GIT_TIMEOUT. git has applied the
    // merge result to index + work tree before this hook runs.
    let hook = repo.join(".git/hooks/pre-merge-commit");
    std::fs::write(&hook, "#!/bin/sh\nsleep 120\n").unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();

    let out = condukt(
        &repo,
        &home,
        &[
            "worktree",
            "merge",
            "--branch",
            &task_branch,
            "--run",
            "runT",
            "--task",
            "t1",
        ],
    );
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    eprintln!(
        "merge exit={:?}\nstdout={stdout}\nstderr={stderr}",
        out.status.code()
    );
    assert!(
        stderr.contains("timed out") || stdout.contains("timed out"),
        "fixture precondition: the merge must have timed out (not been refused \
         for another reason); stdout={stdout:?} stderr={stderr:?}"
    );

    // Observation (2026-11-08): is the killed git's `.git/index.lock` left
    // behind? Printed so the run output records it.
    eprintln!(
        "observation: .git/index.lock present after merge = {}, .git/MERGE_HEAD present = {}",
        repo.join(".git/index.lock").exists(),
        repo.join(".git/MERGE_HEAD").exists()
    );

    let head_after = run_git(&repo, &["rev-parse", "HEAD"]);
    assert_eq!(head_after, head_before, "a failed merge must not move HEAD");
    let staged = run_git(&repo, &["diff", "--cached", "--name-only"]);
    let merge_head = repo.join(".git/MERGE_HEAD").exists();
    let reported = {
        let s = format!("{stdout}{stderr}").to_lowercase();
        s.contains("merge --abort") || s.contains("reset --hard") || s.contains("half")
    };
    let clean = staged.is_empty();
    assert!(
        clean || merge_head || reported,
        "timed-out merge left main HALF-APPLIED and unannounced: staged={staged:?}, \
         MERGE_HEAD present={merge_head}, recovery named in output={reported}; \
         stderr={stderr:?}"
    );

    // Fix contract (backlog 8ada7ad9, 2026-11-08): the rollback actually ran
    // and the failure is still reported as a failure.
    assert!(
        !out.status.success(),
        "a timed-out merge must exit non-zero"
    );
    assert!(clean, "the rollback must leave nothing staged: {staged:?}");
    assert!(!merge_head, "the rollback must leave no MERGE_HEAD");
    assert!(
        !repo.join(".git/index.lock").exists(),
        "no index.lock may remain after the rollback"
    );
    assert!(
        !repo.join("task.txt").exists(),
        "the half-applied incoming file must be gone from main's work tree"
    );
    assert!(
        stderr.contains("rolled back"),
        "stderr must state the half-applied merge was rolled back: {stderr:?}"
    );
}

/// Backlog 8ada7ad9, the `index.lock` arm. A kill INSIDE a merge driver
/// leaves the killed git's `.git/index.lock` behind (observed 2026-11-08 in
/// `backlog_merge_timeout_other_paths_20261108`). condukt cannot prove the
/// lock is the killed git's rather than a live git's, so the rollback must not
/// remove it and must fail loudly: non-zero exit, "timed out", the
/// half-applied state named, the lock named, the manual recovery named.
#[test]
fn timed_out_merge_with_a_left_index_lock_fails_loudly_and_keeps_the_lock() {
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
    let wt = std::path::PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
    std::fs::write(wt.join("shared.txt"), "a\nb\ntask\n").unwrap();
    run_git(&wt, &["add", "shared.txt"]);
    run_git(
        &wt,
        &["commit", "-q", "-m", "task", "-m", "Condukt-Task: runT/t1"],
    );
    let task_branch = run_git(&wt, &["rev-parse", "--abbrev-ref", "HEAD"]);
    std::fs::write(repo.join("shared.txt"), "a\nmain\nc\n").unwrap();
    run_git(&repo, &["add", "shared.txt"]);
    run_git(&repo, &["commit", "-q", "-m", "main moves"]);

    // Merge driver for shared.txt: instant on its first call (the trial
    // merge), sleeps past the 45s GIT_TIMEOUT on the second (the real merge).
    let marker = base.join("driver-called-once");
    let driver = base.join("slow-merge-driver.sh");
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
        &repo,
        &[
            "config",
            "merge.slow.driver",
            &format!("{} %O %A %B", driver.display()),
        ],
    );
    std::fs::write(repo.join(".git/info/attributes"), "shared.txt merge=slow\n").unwrap();

    let out = condukt(
        &repo,
        &home,
        &[
            "worktree",
            "merge",
            "--branch",
            &task_branch,
            "--run",
            "runT",
            "--task",
            "t1",
        ],
    );
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    let lock = repo.join(".git/index.lock");
    eprintln!(
        "merge exit={:?}\nstderr={stderr}\nobservation: .git/index.lock present = {}",
        out.status.code(),
        lock.exists()
    );
    assert!(
        marker.exists(),
        "fixture precondition: the trial merge called the driver"
    );
    assert!(
        stderr.contains("timed out"),
        "fixture precondition: the real merge must have timed out: {stderr:?}"
    );
    assert!(
        lock.exists(),
        "fixture precondition: the kill inside the driver left index.lock"
    );
    assert!(!out.status.success(), "must exit non-zero");
    assert!(
        stderr.contains("HALF-APPLIED"),
        "must name the state: {stderr:?}"
    );
    assert!(
        stderr.contains("index.lock"),
        "must name the lock: {stderr:?}"
    );
    assert!(
        stderr.contains("git reset --merge") && stderr.contains("git merge --abort"),
        "must name the manual recovery: {stderr:?}"
    );
    assert!(
        !stderr.contains("rolled back"),
        "must not claim a rollback that did not happen: {stderr:?}"
    );
}
