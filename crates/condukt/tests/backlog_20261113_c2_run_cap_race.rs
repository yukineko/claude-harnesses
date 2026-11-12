// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro (audit 2026-11-13 C2): the per-run worktree cap is a
//! check-then-act race.
//!
//! Defect: `run_worktree` (crates/condukt/src/main.rs, `WtAction::Create`) calls
//! `run_cap::enforce_run_cap` (lists live worktrees, compares with
//! `max_parallel`) and then `worktree::create_namespaced` with no lock held
//! across the two. Two concurrent
//! `condukt worktree create --run R --topic a/b --branch ...` under
//! `CONDUKT_MAX_PARALLEL=1` can both observe zero live worktrees and both
//! create one, exceeding the cap.
//!
//! Measurement point: rev 09dd0c8c, 2026-11-13.
//!
//! What this test does: `ATTEMPTS` fresh repos; per attempt two threads wait on
//! a `Barrier` and then each spawn the real binary with distinct topic/branch
//! for the same run. FAILS if ANY attempt lets both succeed (or leaves two
//! live worktrees of the run).
//!
//! What this test does NOT prove: it is a race test. A pass (no attempt
//! observed the overlap) does NOT prove the race is absent; it only means it
//! was not observed in `ATTEMPTS` tries on this machine. A failure is a
//! genuine observation of the over-cap state. Process-spawn jitter means the
//! two creates are only approximately simultaneous.
//!
//! The control (a sequential second create is refused) runs first so the RED
//! assertion cannot fail on plumbing. Everything runs the real binary against
//! a per-test `$HOME`, state dir and worktree base inside throwaway git repos
//! with `GIT_CONFIG_GLOBAL=/dev/null`; nothing under the real `~/.condukt`,
//! `~/.budgetguard` or `~/.overwatch` is touched.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{Arc, Barrier};

const ATTEMPTS: usize = 20;

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

    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_condukt"));
        harness_core::git_env::scrub(&mut cmd);
        cmd.args(args)
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env("CONDUKT_WORKTREE_BASE", &self.wt_base)
            .env("CONDUKT_DEFAULT_BRANCH", "main")
            .env("CONDUKT_MAX_PARALLEL", "1")
            .env("CLAUDE_CODE_SESSION_ID", "sess-reds-20261113")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("CONDUKT_DISABLE")
            .env_remove("CONDUKT_STUCK_TTL_SECS");
        cmd
    }

    fn live_worktrees_of(&self, run: &str) -> usize {
        run_git(&self.repo, &["worktree", "list", "--porcelain"])
            .lines()
            .filter(|l| l.starts_with("branch ") && l.contains(&format!("/{run}/")))
            .count()
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

fn create_args<'a>(run: &'a str, topic: &'a str, branch: &'a str) -> [&'a str; 8] {
    [
        "worktree", "create", "--run", run, "--topic", topic, "--branch", branch,
    ]
}

/// CONTROL (not ignored): sequentially, the second create under cap 1 is
/// refused, so the cap works when there is no race.
#[test]
fn control_sequential_second_create_is_refused_under_cap_one() {
    let fx = Fixture::new();
    let a = fx
        .command(&create_args("run-c2", "a", "b-a"))
        .output()
        .unwrap();
    assert!(a.status.success(), "CONTROL: first create: {}", show(&a));
    let b = fx
        .command(&create_args("run-c2", "b", "b-b"))
        .output()
        .unwrap();
    assert!(
        !b.status.success(),
        "CONTROL: sequential second create must be refused: {}",
        show(&b)
    );
    assert_eq!(fx.live_worktrees_of("run-c2"), 1);
}

/// C2: two concurrent creates must not both succeed under cap 1.
#[test]
#[ignore = "RED: audit 2026-11-13 C2 — remove the ignore when fixed"]
fn concurrent_creates_under_cap_one_do_not_both_succeed() {
    for attempt in 0..ATTEMPTS {
        let fx = Fixture::new();
        let run = "run-c2";
        let barrier = Arc::new(Barrier::new(2));
        let mut handles = Vec::new();
        for (topic, branch) in [("a", "b-a"), ("b", "b-b")] {
            let mut cmd = fx.command(&create_args(run, topic, branch));
            let barrier = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                cmd.output().expect("spawn condukt")
            }));
        }
        let outs: Vec<Output> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        let ok = outs.iter().filter(|o| o.status.success()).count();
        let live = fx.live_worktrees_of(run);
        eprintln!(
            "attempt {attempt}: successes={ok} live={live} a=[{}] b=[{}]",
            show(&outs[0]),
            show(&outs[1])
        );
        assert!(
            ok <= 1 && live <= 1,
            "attempt {attempt}: CONDUKT_MAX_PARALLEL=1 but two concurrent creates for {run} \
             produced successes={ok}, live worktrees={live} (cap 1); a={} b={}",
            show(&outs[0]),
            show(&outs[1])
        );
    }
}
