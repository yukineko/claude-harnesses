//! Two real `condukt` processes racing a PRIMARY-repo mutator (`worktree
//! cleanup`, which runs `git worktree prune` under the repo-scoped
//! `lock::REPO_PRIMARY_LOCK_KEY`) on the SAME repo must serialize on the one
//! repo-scoped lock instead of racing — both complete cleanly (exit 0), and the
//! shared lock is reused across processes without deadlocking.
//!
//! This is the cross-process companion to `lock.rs`'s in-process RMW
//! serialization unit test (`repo_primary_lock_serializes_concurrent_rmw_...`):
//! condukt is a bin crate, so the lock cannot be driven from an integration test
//! except through a real subcommand. `worktree cleanup` is the concrete
//! primary-repo mutator site (main.rs) that now takes
//! `lock::acquire_repo_primary`. Regression target: backlog c701e75f — two
//! condukt runs in one repo previously raced `main`/prune with NO repo-level
//! lock (only the upstream flow backlog lock).

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

static TRIAL_COUNTER: AtomicU64 = AtomicU64::new(0);

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_condukt"))
}

fn git(repo: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .current_dir(repo)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("spawn git")
        .success();
    assert!(ok, "git {args:?} failed in {}", repo.display());
}

fn init_repo(repo: &Path) {
    std::fs::create_dir_all(repo).unwrap();
    git(repo, &["init", "-b", "main"]);
    git(repo, &["config", "user.email", "test@example.com"]);
    git(repo, &["config", "user.name", "Test"]);
    std::fs::write(repo.join("base.txt"), "base\n").unwrap();
    git(repo, &["add", "."]);
    git(repo, &["commit", "-m", "init"]);
}

fn spawn_cleanup(cwd: &Path, home: &Path) -> std::process::Child {
    Command::new(bin())
        .current_dir(cwd)
        .env("HOME", home)
        .args(["worktree", "cleanup"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn condukt worktree cleanup")
}

#[test]
fn concurrent_worktree_prune_same_repo_serializes_no_deadlock() {
    const TRIALS: u64 = 6;

    for _ in 0..TRIALS {
        let n = TRIAL_COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp = std::env::temp_dir().join(format!(
            "condukt-repo-primary-proc-{}-{n}",
            std::process::id()
        ));
        // A shared HOME so both processes resolve the SAME `state_dir` and thus
        // the SAME repo-scoped lock file for this repo.
        let home = tmp.join("home");
        let repo = tmp.join("repo");
        std::fs::create_dir_all(&home).unwrap();
        init_repo(&repo);

        // Back-to-back spawn so both processes are alive and contend the
        // repo-scoped prune lock at the same time.
        let a = spawn_cleanup(&repo, &home);
        let b = spawn_cleanup(&repo, &home);

        // `wait_with_output` returns only when the process exits — a genuine
        // deadlock would hang here and trip the test-runner timeout, which is
        // itself the "no deadlock" assertion. The lock is bounded-wait +
        // fail-soft, so this always completes.
        let out_a = a.wait_with_output().expect("wait on A");
        let out_b = b.wait_with_output().expect("wait on B");

        assert_eq!(
            out_a.status.code(),
            Some(0),
            "trial {n}: process A must exit 0 (serialized cleanly under repo lock); stderr={:?}",
            String::from_utf8_lossy(&out_a.stderr)
        );
        assert_eq!(
            out_b.status.code(),
            Some(0),
            "trial {n}: process B must exit 0 (serialized cleanly under repo lock); stderr={:?}",
            String::from_utf8_lossy(&out_b.stderr)
        );

        std::fs::remove_dir_all(&tmp).ok();
    }
}
