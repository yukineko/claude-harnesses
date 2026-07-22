//! Cross-session namespacing for condukt worktrees/branches (backlog 0213f7a9).
//!
//! `worktree_base` is machine-global (`~/.condukt/worktrees`, config.rs) and the
//! driver cuts worktrees as `--topic <t.id> --branch condukt/<t.id>`
//! (SKILL.md Phase 5). Task ids are per-run and NOT comparable across runs
//! (claim.rs:18-19), so two CONCURRENT sessions that both emit `t1` previously
//! aimed at the exact same path and the exact same branch ref. Three concrete
//! collisions followed:
//!
//!   1. `worktree.rs` "worktree path already exists"          (hard bail)
//!   2. `worktree.rs` "branch ... already checked out"        (hard bail)
//!   3. MOST DANGEROUS — `create()` force-deletes a lingering, non-live branch
//!      ref (`git branch -D`) before re-cutting. A peer session that removed its
//!      worktree dir but has NOT merged yet leaves exactly such a ref, so the
//!      peer's commits were silently destroyed.
//!
//! These tests pin the namespace: `--run <RID>` makes the worktree dir and the
//! branch ref run-scoped, so two runs emitting the same task id (a) both create
//! their worktrees and (b) never touch each other's branch ref.
//!
//! Spawns the real binary against an isolated HOME so `worktree_base` lands in a
//! throwaway dir.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_condukt")
}

struct Fixture {
    base: PathBuf,
    repo: PathBuf,
    home: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let pid = std::process::id();
        let mut base = std::env::temp_dir();
        base.push(format!("condukt-wt-ns-{pid}-{tag}"));
        let _ = std::fs::remove_dir_all(&base);
        let repo = base.join("repo");
        let home = base.join("home");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        git_ok(&repo, &["init", "-q", "-b", "main"]);
        git_ok(&repo, &["config", "user.email", "t@t.t"]);
        git_ok(&repo, &["config", "user.name", "t"]);
        std::fs::write(repo.join("seed.txt"), "seed\n").unwrap();
        git_ok(&repo, &["add", "seed.txt"]);
        git_ok(&repo, &["commit", "-q", "-m", "seed"]);
        Self { base, repo, home }
    }

    fn condukt(&self, args: &[&str]) -> Output {
        Command::new(bin())
            .args(args)
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .output()
            .expect("spawn condukt")
    }

    /// `condukt worktree create` for a run-namespaced task, returning the Output.
    fn create_ns(&self, run: &str, topic: &str, branch: &str) -> Output {
        self.condukt(&[
            "worktree", "create", "--run", run, "--topic", topic, "--branch", branch,
        ])
    }

    fn stdout_path(out: &Output) -> PathBuf {
        PathBuf::from(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    fn branches(&self) -> String {
        let out = Command::new("git")
            .args(["branch", "--list", "--format=%(refname:short)"])
            .current_dir(&self.repo)
            .output()
            .expect("spawn git branch");
        String::from_utf8_lossy(&out.stdout).to_string()
    }

    fn rev(&self, refname: &str) -> Option<String> {
        let out = Command::new("git")
            .args(["rev-parse", "--verify", "--quiet", refname])
            .current_dir(&self.repo)
            .output()
            .expect("spawn git rev-parse");
        if !out.status.success() {
            return None;
        }
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if s.is_empty() {
            None
        } else {
            Some(s)
        }
    }
}

fn git_ok(dir: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("spawn git")
        .status
        .success();
    assert!(ok, "git {args:?} failed in {}", dir.display());
}

/// DoD (1): two concurrent runs emitting the SAME task id must BOTH create their
/// worktree — distinct dirs, distinct branch refs, no bail.
#[test]
fn two_runs_with_the_same_task_id_both_create_worktrees() {
    let fx = Fixture::new("both-create");

    let a = fx.create_ns("runA", "t1", "condukt/t1");
    assert!(
        a.status.success(),
        "runA create must succeed: stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&a.stdout),
        String::from_utf8_lossy(&a.stderr)
    );
    let b = fx.create_ns("runB", "t1", "condukt/t1");
    assert!(
        b.status.success(),
        "runB create (SAME task id t1) must not collide with runA: stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&b.stdout),
        String::from_utf8_lossy(&b.stderr)
    );

    let pa = Fixture::stdout_path(&a);
    let pb = Fixture::stdout_path(&b);
    assert_ne!(pa, pb, "the two runs must get DISTINCT worktree paths");
    assert!(
        pa.is_dir(),
        "runA worktree dir must exist at {}",
        pa.display()
    );
    assert!(
        pb.is_dir(),
        "runB worktree dir must exist at {}",
        pb.display()
    );

    // Both worktree dirs must remain DIRECT children of worktree_base: `orphans()`
    // only reads the top level of worktree_base, so a nested `<base>/<run>/<t1>`
    // layout would make the intermediate dir look like unattributable debris.
    assert_eq!(
        pa.parent(),
        pb.parent(),
        "namespaced worktrees must stay direct children of the same worktree_base \
         (orphans() only scans one level); got {} vs {}",
        pa.display(),
        pb.display()
    );

    let branches = fx.branches();
    assert!(
        branches.contains("condukt/runA/t1"),
        "runA's branch must be run-namespaced; got: {branches}"
    );
    assert!(
        branches.contains("condukt/runB/t1"),
        "runB's branch must be run-namespaced; got: {branches}"
    );
}

/// DoD (2): re-cutting a worktree must NEVER force-delete a peer run's branch
/// ref. The peer is in the exact state `create()`'s `git branch -D` targets:
/// worktree dir gone (so it is not "checked out"), commits NOT merged.
#[test]
fn re_cut_never_force_deletes_a_peer_runs_branch_ref() {
    let fx = Fixture::new("no-peer-nuke");

    // Peer runA cuts t1 and commits work that is NOT merged anywhere.
    let a = fx.create_ns("runA", "t1", "condukt/t1");
    assert!(a.status.success(), "runA create failed: {a:?}");
    let wt_a = Fixture::stdout_path(&a);
    std::fs::write(wt_a.join("peer_work.txt"), "precious\n").unwrap();
    git_ok(&wt_a, &["add", "peer_work.txt"]);
    git_ok(&wt_a, &["commit", "-q", "-m", "peer work"]);
    let peer_sha = fx
        .rev("refs/heads/condukt/runA/t1")
        .expect("peer branch ref must exist after runA's commit");

    // The peer's worktree DIR disappears out-of-band (crash / manual cleanup /
    // `worktree remove` without a merge). Its branch ref survives and is no
    // longer "checked out in a live worktree" — precisely the state the
    // `git branch -D` guard in create() considers a "stale leftover".
    std::fs::remove_dir_all(&wt_a).unwrap();

    // runB now cuts the SAME task id, then re-cuts it (the retry path that runs
    // prune + branch -D). Neither may touch runA's ref.
    let b = fx.create_ns("runB", "t1", "condukt/t1");
    assert!(b.status.success(), "runB create failed: {b:?}");
    let wt_b = Fixture::stdout_path(&b);
    std::fs::remove_dir_all(&wt_b).unwrap();
    let b2 = fx.create_ns("runB", "t1", "condukt/t1");
    assert!(
        b2.status.success(),
        "runB re-cut must succeed (prune + force-delete of its OWN stale ref): {b2:?}"
    );

    assert_eq!(
        fx.rev("refs/heads/condukt/runA/t1").as_deref(),
        Some(peer_sha.as_str()),
        "runB's re-cut must NOT delete or move the peer run's branch ref \
         (was {peer_sha}); branches now: {}",
        fx.branches()
    );
    assert!(
        fx.base.join("repo").exists(),
        "sanity: fixture repo still present"
    );
}

/// The namespace is only as unique as the run id that feeds it. `state init`
/// auto-generates one from a second-granular timestamp, so two sessions starting
/// inside the same second used to get the SAME id — which would hand both runs
/// the same worktree dir and the same branch ref again, re-opening the exact
/// collision `--run` exists to close. Two back-to-back inits must differ.
#[test]
fn concurrent_state_init_calls_get_distinct_run_ids() {
    let fx = Fixture::new("distinct-run-ids");
    let dec = fx.repo.join("dec.json");
    std::fs::write(
        &dec,
        r#"{"goal":"g","tasks":[{"id":"t1","title":"a","touched_files":[],"deps":[],"class":"parallel","done_criteria":"d"}]}"#,
    )
    .unwrap();

    // Back-to-back spawn: both processes init within the same wall-clock second,
    // which is precisely the window a second-granular id cannot separate.
    let spawn = || {
        Command::new(bin())
            .args(["state", "init", "--file", dec.to_str().unwrap()])
            .current_dir(&fx.repo)
            .env("HOME", &fx.home)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn condukt state init")
    };
    let a = spawn();
    let b = spawn();
    let out_a = a.wait_with_output().expect("wait A");
    let out_b = b.wait_with_output().expect("wait B");
    assert!(out_a.status.success(), "init A failed: {out_a:?}");
    assert!(out_b.status.success(), "init B failed: {out_b:?}");

    let id_of = |out: &Output| -> String {
        let s = String::from_utf8_lossy(&out.stdout).to_string();
        s.lines()
            .map(str::trim)
            .rev()
            .find(|l| l.starts_with("run-"))
            .unwrap_or_else(|| panic!("no run- id in init output: {s:?}"))
            .to_string()
    };
    let (ia, ib) = (id_of(&out_a), id_of(&out_b));
    assert_ne!(
        ia, ib,
        "two concurrent `state init` calls must get DISTINCT run ids — an identical \
         id would give both runs the same namespaced worktree dir and branch ref"
    );
}

/// Backward compatibility: `--run` is OPTIONAL. Omitting it keeps the exact
/// legacy layout (`<worktree_base>/<topic>` on `<branch>` verbatim), so
/// worktrees/branches created by an older condukt are still addressable and are
/// never orphaned by the new naming.
#[test]
fn omitting_run_keeps_the_legacy_unnamespaced_layout() {
    let fx = Fixture::new("legacy");

    let out = fx.condukt(&[
        "worktree",
        "create",
        "--topic",
        "t1",
        "--branch",
        "condukt/t1",
    ]);
    assert!(
        out.status.success(),
        "legacy (no --run) create must still work: {out:?}"
    );
    let p = Fixture::stdout_path(&out);
    assert_eq!(
        p.file_name().and_then(|s| s.to_str()),
        Some("t1"),
        "legacy layout must remain <worktree_base>/<topic>; got {}",
        p.display()
    );
    assert!(
        fx.rev("refs/heads/condukt/t1").is_some(),
        "legacy branch name must be used verbatim; branches: {}",
        fx.branches()
    );
}
