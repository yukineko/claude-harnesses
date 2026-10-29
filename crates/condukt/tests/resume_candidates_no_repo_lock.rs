// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `condukt worktree resume-candidates` is a READ-ONLY discovery command and
//! must not take the repo-primary lock (operator ruling).
//!
//! # What this pins
//!
//! While another LIVE process holds `__repo_primary__.lock` for the repo,
//! `resume-candidates --json` must still exit 0 and print its JSON report, and
//! do so well within `RunLock::DEADLINE` (10s, `crates/condukt/src/lock.rs`) —
//! rather than waiting out the deadline and refusing.
//!
//! # Anti-vacuity: the control
//!
//! "resume-candidates succeeded while the lock was held" proves nothing unless
//! the lock really WAS held as condukt sees it. So the same held lock is
//! checked against `worktree reconcile --json`, which still takes
//! `lock::acquire_repo_primary`: it must be REFUSED (non-zero, naming the
//! repo-primary lock). A baseline run of the same control BEFORE the lock is
//! published must succeed, so the refusal is attributable to the lock and
//! nothing else in the fixture.
//!
//! # How the lock is held
//!
//! No CLI path holds the repo-primary lock for a controllable duration, so the
//! lock file is published exactly as `RunLock::acquire_at` writes it: JSON
//! `{pid, run_id, acquired_at}` at
//! `<state_dir>/<project_key(main worktree root)>/<safe_session("__repo_primary__")>.lock`,
//! computed with the very same `harness_core` functions condukt uses. The
//! owner pid is a real, live `sleep` child, so condukt's reap rule (reap only
//! on a positive "pid is dead") never fires; the child is killed on drop.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output};
use std::time::{Duration, Instant};

/// Mirror of `RunLock::DEADLINE` (`pub(crate)`, so not importable here).
/// Value read from `crates/condukt/src/lock.rs`:
/// `pub(crate) const DEADLINE: Duration = Duration::from_secs(10);`
const DEADLINE: Duration = Duration::from_secs(10);

/// Mirror of `lock::REPO_PRIMARY_LOCK_KEY`.
const REPO_PRIMARY_LOCK_KEY: &str = "__repo_primary__";

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_condukt")
}

fn run_git(dir: &Path, args: &[&str]) {
    let out = harness_core::git_env::scrub(&mut Command::new("git"))
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?} in {} failed: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A live process that owns the published lock; killed (and the lock file
/// removed) on drop, on every exit path including a failed assertion.
struct HeldLock {
    child: Child,
    path: PathBuf,
}

impl Drop for HeldLock {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.path);
    }
}

struct Fixture {
    base: PathBuf,
    repo: PathBuf,
    home: PathBuf,
    wt_base: PathBuf,
    state_dir: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let mut base = std::env::temp_dir();
        base.push(format!("condukt-rc-nolock-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let repo = base.join("repo");
        let wt_base = base.join("worktrees");
        let home = base.join("home");
        // condukt's state_dir = `$HOME/.condukt/state` (config::base_dir).
        let state_dir = home.join(".condukt").join("state");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&wt_base).unwrap();
        std::fs::create_dir_all(&state_dir).unwrap();
        std::fs::create_dir_all(home.join(".claude").join("projects")).unwrap();

        run_git(&repo, &["init", "-q", "-b", "main"]);
        run_git(&repo, &["config", "user.email", "t@t.t"]);
        run_git(&repo, &["config", "user.name", "t"]);
        std::fs::write(repo.join("seed.txt"), "seed\n").unwrap();
        run_git(&repo, &["add", "seed.txt"]);
        run_git(&repo, &["commit", "-q", "-m", "seed"]);

        Self {
            base,
            repo,
            home,
            wt_base,
            state_dir,
        }
    }

    fn condukt(&self, args: &[&str]) -> (Output, Duration) {
        let start = Instant::now();
        let out = harness_core::git_env::scrub(&mut Command::new(bin()))
            .args(args)
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env("CONDUKT_WORKTREE_BASE", &self.wt_base)
            .env("HARNESS_PROGRESS_WINDOW_SECS", "0")
            .env_remove("CONDUKT_DISABLE")
            .env_remove("CONDUKT_SINGLE_WORKTREE")
            .output()
            .expect("condukt runs");
        (out, start.elapsed())
    }

    /// The repo-primary lock path exactly as `lock::acquire_repo_primary`
    /// resolves it for a cwd in this repo's main tree.
    fn repo_primary_lock_path(&self) -> PathBuf {
        let root = match harness_core::projkey::main_worktree_root(&self.repo) {
            harness_core::verdict::Determination::Known(r) => r,
            other => panic!("main worktree root of fixture repo not determined: {other:?}"),
        };
        self.state_dir
            .join(harness_core::projkey::project_key(&root))
            .join(format!(
                "{}.lock",
                harness_core::store::safe_session(REPO_PRIMARY_LOCK_KEY)
            ))
    }

    /// Publish the repo-primary lock owned by a live `sleep` child.
    fn hold_repo_primary(&self) -> HeldLock {
        let child = Command::new("sleep")
            .arg("600")
            .spawn()
            .expect("spawn sleep");
        let path = self.repo_primary_lock_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let info = serde_json::json!({
            "pid": child.id(),
            "run_id": REPO_PRIMARY_LOCK_KEY,
            "acquired_at": now,
        });
        std::fs::write(&path, serde_json::to_string(&info).unwrap()).unwrap();
        HeldLock { child, path }
    }
}

fn describe(out: &Output) -> String {
    format!(
        "exit {:?}\nstdout:\n{}\nstderr:\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn resume_candidates_succeeds_promptly_while_repo_primary_lock_is_held() {
    let fx = Fixture::new("held");

    // Baseline: with nothing holding the lock, the control succeeds — so a
    // later refusal is attributable to the lock alone.
    let (base_out, _) = fx.condukt(&["worktree", "reconcile", "--json"]);
    assert!(
        base_out.status.success(),
        "baseline `worktree reconcile --json` must succeed with no lock held:\n{}",
        describe(&base_out)
    );

    let held = fx.hold_repo_primary();
    assert!(
        held.path.exists(),
        "lock file not published at {}",
        held.path.display()
    );

    // CONTROL: a command that still takes `acquire_repo_primary` must observe
    // the lock as held and refuse. Without this, the assertion below proves
    // nothing.
    let (ctl_out, ctl_elapsed) = fx.condukt(&["worktree", "reconcile", "--json"]);
    assert!(
        !ctl_out.status.success(),
        "CONTROL: `worktree reconcile --json` must be refused while the \
         repo-primary lock is held by a live pid — otherwise the held lock is \
         not observed and this test is vacuous:\n{}",
        describe(&ctl_out)
    );
    let ctl_err = String::from_utf8_lossy(&ctl_out.stderr);
    assert!(
        ctl_err.contains("repo-primary lock"),
        "CONTROL refused for a reason other than the repo-primary lock:\n{}",
        describe(&ctl_out)
    );
    assert!(
        ctl_elapsed >= DEADLINE,
        "CONTROL refused after {ctl_elapsed:?}, before the {DEADLINE:?} wait — \
         not the contended-lock path:\n{}",
        describe(&ctl_out)
    );
    // The live holder was waited on, not reaped.
    assert!(
        held.path.exists(),
        "the live holder's lock was reaped by the control"
    );

    // SUBJECT: the read-only discovery command must not wait on that lock.
    let (out, elapsed) = fx.condukt(&["worktree", "resume-candidates", "--json"]);
    assert!(
        out.status.success(),
        "`worktree resume-candidates --json` must succeed while another live \
         process holds the repo-primary lock (it is read-only and must not take \
         it); took {elapsed:?}:\n{}",
        describe(&out)
    );
    let parsed: Result<serde_json::Value, _> = serde_json::from_slice(&out.stdout);
    assert!(
        parsed.is_ok(),
        "resume-candidates stdout is not valid JSON ({:?}):\n{}",
        parsed.err(),
        describe(&out)
    );
    assert!(
        elapsed < DEADLINE,
        "resume-candidates took {elapsed:?}, not well within the {DEADLINE:?} \
         lock deadline — it is still waiting on the repo-primary lock"
    );
    // It must not have disturbed the live holder's lock either.
    assert!(
        held.path.exists(),
        "resume-candidates removed a live holder's repo-primary lock"
    );

    drop(held);
}
