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
//! Written without a fix; run with `-- --ignored`.

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
#[ignore = "RED: merge timeout leaves staged half-merge, no MERGE_HEAD 2026-11-07"]
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
}
