// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro (audit 2026-11-13 C4): a detached-HEAD worktree of the run is not
//! counted by the per-run cap.
//!
//! Defect: `run_cap::enforce_run_cap` (crates/condukt/src/run_cap.rs) counts
//! only worktrees whose `worktree::list` branch is `Some(branch)` namespaced
//! under the run (`b.as_deref().and_then(...)`); a worktree with a detached
//! HEAD has `None` and is dropped. After `git checkout --detach` inside run
//! R's worktree A, the cap sees zero live worktrees and a second create for R
//! succeeds under `CONDUKT_MAX_PARALLEL=1`.
//!
//! The run_cap.rs docstring defines live as "branch namespaced under run", so
//! at HEAD this is by-definition behaviour; the audit's reading is that a
//! detached worktree is still attributable to the run: its path is
//! `<worktree_base>/<run>-<topic>` (`namespaced_topic`), so the run can be
//! recovered from the path even when the branch is gone. The RED asserts that
//! reading; if the maintainers rule the docstring definition intended, close
//! as won't-do rather than "fixing" the test.
//!
//! Measurement point: rev 09dd0c8c, 2026-11-13.
//!
//! What this test does NOT prove: it does not prove path-based attribution is
//! the right fix, nor that other worktree states (prunable/locked) are
//! counted. Control first: the cap refuses a second create while A is still on
//! its branch, on the same fixture.
//!
//! Runs the real binary against a per-test `$HOME`, state dir and worktree base
//! in a throwaway repo with `GIT_CONFIG_GLOBAL=/dev/null`; nothing under the
//! real `~/.condukt`, `~/.budgetguard` or `~/.overwatch` is touched.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Fixture {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    home: PathBuf,
    wt_base: PathBuf,
}

fn run_git(dir: &Path, args: &[&str]) -> String {
    let out = harness_core::git_env::scrub(&mut Command::new("git"))
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

impl Fixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let repo = base.join("repo");
        let home = base.join("home");
        let wt_base = base.join("wts");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        run_git(&repo, &["init", "-q", "-b", "main"]);
        run_git(&repo, &["config", "user.email", "t@t.t"]);
        run_git(&repo, &["config", "user.name", "t"]);
        run_git(&repo, &["config", "commit.gpgsign", "false"]);
        std::fs::write(repo.join("base.txt"), "base\n").unwrap();
        run_git(&repo, &["add", "base.txt"]);
        run_git(&repo, &["commit", "-q", "-m", "init"]);
        Self {
            _tmp: tmp,
            repo,
            home,
            wt_base,
        }
    }

    fn create(&self, run: &str, topic: &str, branch: &str) -> Output {
        harness_core::git_env::scrub(&mut Command::new(env!("CARGO_BIN_EXE_condukt")))
            .args([
                "worktree", "create", "--run", run, "--topic", topic, "--branch", branch,
            ])
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env("CONDUKT_WORKTREE_BASE", &self.wt_base)
            .env("CONDUKT_DEFAULT_BRANCH", "main")
            .env("CONDUKT_MAX_PARALLEL", "1")
            .env("CLAUDE_CODE_SESSION_ID", "sess-reds-20261113")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("CONDUKT_DISABLE")
            .output()
            .expect("spawn condukt")
    }
}

fn show(out: &Output) -> String {
    format!(
        "exit={:?} stdout={:?} stderr={:?}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// C4: a detached worktree of run R still occupies R's cap slot.
#[test]
fn detached_worktree_of_the_run_still_counts_against_the_cap() {
    let fx = Fixture::new();
    let run = "run-c4";

    let a = fx.create(run, "a", "b-a");
    assert!(a.status.success(), "CONTROL: first create: {}", show(&a));
    let wt_a = PathBuf::from(String::from_utf8_lossy(&a.stdout).trim());
    assert!(
        wt_a.is_dir(),
        "CONTROL: worktree a live at {}",
        wt_a.display()
    );

    // CONTROL: while A is on its branch the cap refuses a second create.
    let ctl = fx.create(run, "ctl", "b-ctl");
    assert!(
        !ctl.status.success(),
        "CONTROL: second create with A on its branch must be refused: {}",
        show(&ctl)
    );

    // RED: detach A's HEAD; it is still run R's worktree at <base>/<run>-a.
    run_git(&wt_a, &["checkout", "--detach"]);
    assert!(
        wt_a.is_dir(),
        "worktree a must still be live after detaching"
    );
    let b = fx.create(run, "b", "b-b");
    eprintln!("observation (create with A detached): {}", show(&b));
    assert!(
        !b.status.success(),
        "CONDUKT_MAX_PARALLEL=1 must refuse a second worktree of {run} while A \
         ({}) is live but detached; got {}",
        wt_a.display(),
        show(&b)
    );
}
