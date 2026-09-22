//! The repo-primary lock must be keyed by the REPO, not by the checkout.
//!
//! Regression target: backlog `bcfc5491`. `lock::acquire_repo_primary` used to
//! route through `RunLock::acquire_or_skip`, whose `lock_path` keys on
//! `store::repo_root(cwd)`. In a LINKED worktree `.git` is a FILE, so
//! `repo_root` stops there and returns the worktree itself — giving every
//! linked worktree its own `<state_dir>/<worktree-key>/__repo_primary__.lock`.
//! The lock therefore protected nothing it was written to protect: the one
//! primary repo (its shared index, its default branch, its worktree admin dir)
//! is common to ALL checkouts, so a merge running in the main tree and a
//! `worktree prune` running in a linked worktree took two DIFFERENT locks and
//! never contended.
//!
//! These are process-level tests for the same reason
//! `repo_primary_lock_concurrency.rs` is: condukt is a bin crate, so the lock
//! can only be driven from an integration test through a real subcommand. The
//! spawn/fixture shape (a temp `HOME` so `state_dir` is
//! `$HOME/.condukt/state`, a `git init`'d repo, `CARGO_BIN_EXE_condukt`) is
//! taken from that file deliberately — no new launch shape is invented here.
//! The subcommand is `worktree reconcile`, which is what the backlog's
//! reproduction used and which takes `lock::acquire_repo_primary` in `main.rs`.
//!
//! The holder is a hand-placed lock file naming a LIVE pid (a `sleep` child).
//! That is the same on-disk representation `RunLock` publishes, and it makes
//! the contention deterministic: no second condukt process has to be caught
//! mid-critical-section for the assertion to mean something.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
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

/// A main tree + one linked worktree, plus a shared `HOME` so both resolve the
/// same `state_dir`. Returns `(tmp, home, main_tree, linked_worktree)`.
fn fixture(tag: &str) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
    let n = TRIAL_COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp = std::env::temp_dir().join(format!(
        "condukt-repo-primary-scope-{tag}-{}-{n}",
        std::process::id()
    ));
    std::fs::remove_dir_all(&tmp).ok();
    let home = tmp.join("home");
    let repo = tmp.join("repo");
    let wt = tmp.join("wt1");
    std::fs::create_dir_all(&home).unwrap();
    init_repo(&repo);
    git(
        &repo,
        &["worktree", "add", wt.to_str().unwrap(), "-b", "topic1"],
    );
    (tmp, home, repo, wt)
}

/// `<home>/.condukt/state/<project-key of `root`>` — the directory
/// `lock_path_in_project` publishes into. `project_key` is the shared
/// harness-core derivation the binary itself uses, so the test names the same
/// directory without re-implementing the scheme. It is NOT the function under
/// test: what is under test is WHICH root the binary keys on.
fn project_dir(home: &Path, root: &Path) -> PathBuf {
    let canon = root.canonicalize().expect("canonicalize project root");
    home.join(".condukt")
        .join("state")
        .join(harness_core::projkey::project_key(&canon))
}

/// Publish a `__repo_primary__.lock` naming `pid` into `dir`, exactly as
/// `RunLock` would.
fn place_lock(dir: &Path, pid: u32) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let path = dir.join("__repo_primary__.lock");
    std::fs::write(
        &path,
        format!(r#"{{"pid":{pid},"run_id":"__repo_primary__","acquired_at":0}}"#),
    )
    .unwrap();
    path
}

/// A live process to own the hand-placed lock, so the acquire path takes the
/// "holder is alive → wait, then refuse" arm rather than reaping it as stale.
struct Holder(Child);
impl Holder {
    fn spawn() -> Self {
        Holder(
            Command::new("sleep")
                .arg("600")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("spawn sleep as lock holder"),
        )
    }
    fn pid(&self) -> u32 {
        self.0.id()
    }
}
impl Drop for Holder {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct Run {
    code: Option<i32>,
    stderr: String,
}

fn reconcile(cwd: &Path, home: &Path) -> Run {
    let out = Command::new(bin())
        .current_dir(cwd)
        .env("HOME", home)
        .args(["worktree", "reconcile"])
        .output()
        .expect("run condukt worktree reconcile");
    Run {
        code: out.status.code(),
        stderr: String::from_utf8_lossy(&out.stderr).to_string(),
    }
}

/// **The probe.** A live repo-primary lock published under the MAIN tree's
/// project key must be seen by a condukt process running in a LINKED worktree
/// of the same repo — they mutate the same primary repo, so they must contend
/// on the same lock.
///
/// Before the fix this exited 0: the linked-worktree process looked for its
/// lock under its OWN project key, found nothing, and proceeded to run a
/// primary-repo command while a peer held the lock.
#[test]
fn linked_worktree_contends_the_main_trees_repo_primary_lock() {
    let (tmp, home, repo, wt) = fixture("probe");
    let holder = Holder::spawn();
    let lock = place_lock(&project_dir(&home, &repo), holder.pid());
    assert!(lock.exists());

    let run = reconcile(&wt, &home);

    assert_ne!(
        run.code,
        Some(0),
        "a condukt process in the linked worktree {} must REFUSE while the \
         repo-primary lock for the same repo is held under the main tree's \
         project key ({}); it exited 0, so the two checkouts are taking \
         different locks and the shared primary repo is unguarded. stderr={:?}",
        wt.display(),
        lock.display(),
        run.stderr
    );
    assert!(
        run.stderr.contains("repo-primary lock"),
        "the refusal must name the repo-primary lock so it is not confused with \
         an unrelated failure; stderr={:?}",
        run.stderr
    );

    drop(holder);
    std::fs::remove_dir_all(&tmp).ok();
}

/// **Control (discriminating power).** Same fixture, same hand-placed lock,
/// same subcommand — but with the lock under the project key the process is
/// *known* to consult (the main tree's, run from the main tree). This must
/// refuse both before and after the fix.
///
/// Without it, the probe above proves nothing: a `worktree reconcile` that
/// exits 0 unconditionally would produce the same pre-fix observation. This
/// control shows the probe's mechanism (lock-file shape, live pid, the
/// subcommand's exit path) really does turn a held lock into a non-zero exit,
/// so the probe's pre-fix 0 was a genuine scope defect and not a vacuous test.
#[test]
fn control_held_lock_under_the_consulted_key_makes_reconcile_refuse() {
    let (tmp, home, repo, _wt) = fixture("control");
    let holder = Holder::spawn();
    let lock = place_lock(&project_dir(&home, &repo), holder.pid());

    let run = reconcile(&repo, &home);

    assert_ne!(
        run.code,
        Some(0),
        "control: a held repo-primary lock at {} must make `worktree reconcile` \
         in the main tree refuse; stderr={:?}",
        lock.display(),
        run.stderr
    );
    assert!(
        run.stderr.contains("repo-primary lock"),
        "control: the refusal must name the repo-primary lock; stderr={:?}",
        run.stderr
    );

    drop(holder);
    std::fs::remove_dir_all(&tmp).ok();
}

/// **Control (the refusal is caused by the lock).** With NO lock anywhere, the
/// same command in the same linked worktree exits 0. Pairs with the probe: it
/// rules out "reconcile just fails from a linked worktree" as the explanation
/// for the probe's post-fix non-zero exit.
#[test]
fn control_without_any_lock_reconcile_from_linked_worktree_succeeds() {
    let (tmp, home, _repo, wt) = fixture("nolock");

    let run = reconcile(&wt, &home);

    assert_eq!(
        run.code,
        Some(0),
        "control: with no repo-primary lock held, `worktree reconcile` in a \
         linked worktree must succeed — otherwise the probe's non-zero exit \
         cannot be attributed to the lock; stderr={:?}",
        run.stderr
    );

    std::fs::remove_dir_all(&tmp).ok();
}

/// The deliberate flip that the fix causes, asserted so it is recorded rather
/// than discovered later.
///
/// A lock file sitting under a LINKED WORKTREE's own project key is not the
/// repo-primary lock of anything: after the fix every checkout keys that lock
/// on the main worktree root, so such a file is a leftover from the buggy
/// scheme and must NOT gate. Before the fix this was the backlog's step-4
/// control and exited 1 — that observation is what proved the probe had
/// discriminating power, and this test is where the old behaviour is written
/// down as intentionally gone.
#[test]
fn worktree_keyed_lock_no_longer_gates_the_repo_primary_critical_section() {
    let (tmp, home, _repo, wt) = fixture("stalekey");
    let holder = Holder::spawn();
    let lock = place_lock(&project_dir(&home, &wt), holder.pid());

    let run = reconcile(&wt, &home);

    assert_eq!(
        run.code,
        Some(0),
        "a lock under the LINKED WORKTREE's own project key ({}) is not the \
         repo-primary lock and must not gate: the repo-primary lock is keyed by \
         the main worktree root. Exiting non-zero here means the per-checkout \
         keying is still live. stderr={:?}",
        lock.display(),
        run.stderr
    );

    drop(holder);
    std::fs::remove_dir_all(&tmp).ok();
}
