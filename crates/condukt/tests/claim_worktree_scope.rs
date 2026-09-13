//! Regression coverage for backlog `d83b0e8f`: condukt's cross-session
//! task-claim registry (`crate::claim`) keys its on-disk store by
//! `project_key(repo_root(cwd))`. `harness_core::projkey::repo_root` treats
//! `.git` existing (file OR directory) as "this is the repo root", so inside a
//! LINKED git worktree — where `.git` is a plain FILE pointing at the real
//! repo's `.git/worktrees/<name>` — `repo_root` returns the worktree directory
//! itself instead of climbing to the main tree. Two different `repo_root`
//! values hash to two different `project_key`s, so the SAME repository gets
//! TWO separate `claims.json` registries depending on which worktree a
//! `condukt` invocation happens to run from.
//!
//! CLAUDE.md §8 mandates that all condukt work happens inside a linked
//! worktree (never the main tree directly), which is exactly the case this
//! split breaks: a claim taken while doing legitimate work in one worktree is
//! invisible to `condukt state claims`/`claim-task` run from any other
//! worktree of the same repo (including the main tree) — the last guard
//! against two sessions taking the same work silently returns "not claimed"
//! in the only place work is allowed to happen.
//!
//! These are black-box integration tests against the real `condukt` binary,
//! modelled on `tests/claim_registry_e2e.rs`'s Fixture, extended to also
//! materialize a real linked worktree (`git worktree add`) of the same repo.
//!
//! Per the task brief, this file does not implement a fix: it is expected
//! that P1, P2, P3, and P5 FAIL today and keep the file honest about that.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_condukt")
}

/// A temp git repo (with one commit, required for `git worktree add`) plus a
/// linked worktree of it, and an isolated HOME so `condukt`'s
/// `~/.condukt/state` never touches the real one.
struct Fixture {
    /// The main tree's working directory (where `git init` ran).
    main_tree: PathBuf,
    /// A linked worktree of `main_tree`, created via `git worktree add`.
    worktree: PathBuf,
    /// Isolated $HOME so `condukt`'s state_dir is `<home>/.condukt/state`.
    home: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let pid = std::process::id();
        let mut base = std::env::temp_dir();
        base.push(format!("condukt-wt-scope-{pid}-{tag}"));
        let _ = std::fs::remove_dir_all(&base);
        let main_tree = base.join("main-tree");
        let worktree = base.join("wt1");
        let home = base.join("home");
        std::fs::create_dir_all(&main_tree).unwrap();
        std::fs::create_dir_all(&home).unwrap();

        run_git(&main_tree, &["init", "-q"]);
        run_git(&main_tree, &["config", "user.email", "t@t.t"]);
        run_git(&main_tree, &["config", "user.name", "t"]);
        // `git worktree add` requires at least one commit to branch from.
        std::fs::write(main_tree.join("README.md"), "seed\n").unwrap();
        run_git(&main_tree, &["add", "README.md"]);
        run_git(&main_tree, &["commit", "-q", "-m", "seed"]);

        run_git(
            &main_tree,
            &["worktree", "add", "-b", "wt1", worktree.to_str().unwrap()],
        );
        // Sanity: the linked worktree's `.git` is a FILE (gitdir pointer), not
        // a directory — this is the exact condition `repo_root` mis-treats as
        // "this is a repo root" (see module docs and
        // `harness_core::projkey::repo_root`).
        let wt_git = worktree.join(".git");
        assert!(
            wt_git.exists() && wt_git.is_file(),
            "fixture invariant broken: linked worktree's .git must be a FILE, \
             got exists={} is_file={} at {}",
            wt_git.exists(),
            wt_git.is_file(),
            wt_git.display()
        );

        Self {
            main_tree,
            worktree,
            home,
        }
    }

    fn condukt_in(&self, cwd: &Path, args: &[&str]) -> Output {
        Command::new(bin())
            .args(args)
            .current_dir(cwd)
            .env("HOME", &self.home)
            .env("CLAUDE_CODE_SESSION_ID", "sess-test")
            .output()
            .expect("spawn condukt")
    }

    fn state_dir(&self) -> PathBuf {
        self.home.join(".condukt").join("state")
    }
}

fn run_git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {args:?} failed: stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn stdout_str(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

/// List every `claims.json` this fixture's isolated state dir holds, as
/// `<project-key-dir>` strings, for self-explaining failure messages.
fn claims_json_dirs(state_dir: &Path) -> Vec<String> {
    let mut found = Vec::new();
    if let Ok(entries) = std::fs::read_dir(state_dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() && p.join("claims.json").exists() {
                found.push(p.file_name().unwrap().to_string_lossy().into_owned());
            }
        }
    }
    found.sort();
    found
}

// ---------------------------------------------------------------------------
// P1 (HEADLINE): cross-worktree visibility of `state claims`.
// ---------------------------------------------------------------------------
#[test]
fn p1_claim_taken_in_main_tree_is_visible_from_linked_worktree() {
    let fx = Fixture::new("p1-visibility");

    let claim = fx.condukt_in(
        &fx.main_tree,
        &["state", "claim-task", "--run", "runA", "--hashkey", "H-p1"],
    );
    assert!(
        claim.status.success(),
        "P1 setup: claiming H-p1 from the main tree should succeed: {claim:?}"
    );

    let claims_from_wt = fx.condukt_in(&fx.worktree, &["state", "claims"]);
    assert!(
        claims_from_wt.status.success(),
        "P1 setup: `state claims` from the linked worktree should succeed: {claims_from_wt:?}"
    );
    let out = stdout_str(&claims_from_wt);
    assert!(
        out.contains("H-p1") && out.contains("runA"),
        "PROPERTY P1 (cross-worktree visibility) violated: a task claimed with \
         cwd=main tree must be visible to `condukt state claims` run with \
         cwd=the linked worktree (same repo), holder run 'runA'. Got claims \
         output from the worktree: {out}"
    );
}

// ---------------------------------------------------------------------------
// P2 (HEADLINE): the actual guard — cross-worktree hard-skip.
// ---------------------------------------------------------------------------
#[test]
fn p2_second_run_claiming_from_linked_worktree_is_hard_skipped_not_claimed() {
    let fx = Fixture::new("p2-guard");

    let claim_a = fx.condukt_in(
        &fx.main_tree,
        &["state", "claim-task", "--run", "runA", "--hashkey", "H-p2"],
    );
    assert!(
        claim_a.status.success(),
        "P2 setup: run A claiming H-p2 from the main tree should succeed: {claim_a:?}"
    );

    let claim_b = fx.condukt_in(
        &fx.worktree,
        &["state", "claim-task", "--run", "runB", "--hashkey", "H-p2"],
    );
    let b_out = stdout_str(&claim_b);
    assert!(
        !claim_b.status.success() && b_out.contains("\"holder_run\": \"runA\""),
        "PROPERTY P2 (cross-worktree guard) violated: run B claiming the SAME \
         hashkey H-p2 from a DIFFERENT worktree of the same repo, while run A \
         already holds it (claimed from the main tree), must be HARD-SKIPPED \
         (non-zero exit, skip JSON naming holder_run=runA) — not claimed. \
         Got exit={:?} stdout={b_out}",
        claim_b.status.code()
    );
}

// ---------------------------------------------------------------------------
// P3: store identity — one physical claims.json for the whole repo.
// ---------------------------------------------------------------------------
#[test]
fn p3_main_tree_and_linked_worktree_share_one_claims_json_file() {
    let fx = Fixture::new("p3-store-identity");

    let claim_main = fx.condukt_in(
        &fx.main_tree,
        &[
            "state",
            "claim-task",
            "--run",
            "runMain",
            "--hashkey",
            "H-main",
        ],
    );
    assert!(
        claim_main.status.success(),
        "P3 setup: claim from main tree should succeed: {claim_main:?}"
    );

    let claim_wt = fx.condukt_in(
        &fx.worktree,
        &["state", "claim-task", "--run", "runWt", "--hashkey", "H-wt"],
    );
    assert!(
        claim_wt.status.success(),
        "P3 setup: claim from linked worktree should succeed: {claim_wt:?}"
    );

    let dirs = claims_json_dirs(&fx.state_dir());
    assert_eq!(
        dirs.len(),
        1,
        "PROPERTY P3 (store identity) violated: the main tree and its own \
         linked worktree must write/read the SAME claims.json (one \
         project-keyed directory under the state dir), since they are the \
         SAME repository. Directory listing of {} containing claims.json: \
         {dirs:?} (expected exactly 1 entry, found {})",
        fx.state_dir().display(),
        dirs.len()
    );
}

// ---------------------------------------------------------------------------
// P4 (control): genuinely different repos still get different registries.
// This must PASS today and keep passing after any fix.
// ---------------------------------------------------------------------------
#[test]
fn p4_two_unrelated_repos_still_get_separate_claims_json_files() {
    let pid = std::process::id();
    let mut base = std::env::temp_dir();
    base.push(format!("condukt-wt-scope-{pid}-p4-control"));
    let _ = std::fs::remove_dir_all(&base);
    let repo_a = base.join("repo-a");
    let repo_b = base.join("repo-b");
    let home = base.join("home");
    std::fs::create_dir_all(&repo_a).unwrap();
    std::fs::create_dir_all(&repo_b).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    for r in [&repo_a, &repo_b] {
        run_git(r, &["init", "-q"]);
        run_git(r, &["config", "user.email", "t@t.t"]);
        run_git(r, &["config", "user.name", "t"]);
    }

    let run = |cwd: &Path, run_id: &str, hk: &str| -> Output {
        Command::new(bin())
            .args(["state", "claim-task", "--run", run_id, "--hashkey", hk])
            .current_dir(cwd)
            .env("HOME", &home)
            .env("CLAUDE_CODE_SESSION_ID", "sess-test")
            .output()
            .expect("spawn condukt")
    };

    let a = run(&repo_a, "runA", "H-a");
    assert!(a.status.success(), "control setup: repo A claim: {a:?}");
    let b = run(&repo_b, "runB", "H-b");
    assert!(b.status.success(), "control setup: repo B claim: {b:?}");

    let state_dir = home.join(".condukt").join("state");
    let dirs = claims_json_dirs(&state_dir);
    assert_eq!(
        dirs.len(),
        2,
        "PROPERTY P4 (control: distinct repos stay separate) violated: two \
         UNRELATED temp git repos must get DIFFERENT claims.json files — \
         collapsing them would be a different, worse bug (all projects \
         sharing one registry). Directory listing of {} containing \
         claims.json: {dirs:?} (expected exactly 2 entries, found {})",
        state_dir.display(),
        dirs.len()
    );
}

// ---------------------------------------------------------------------------
// P5: the registry's RMW lock (`__claims__`) is ALSO keyed by
// `project_key(repo_root(cwd))` (confirmed by reading
// crates/condukt/src/lock.rs:147-152's `lock_path`, which joins
// `cfg.state_dir.join(project_key(&repo_root(cwd)))` with
// `<safe_session(run_id)>.lock` — the identical composition `claim.rs`'s
// `registry_path` uses for `claims.json`). `CLAIMS_LOCK_KEY` is `"__claims__"`,
// which `harness_core::store::safe_session` passes through unchanged (every
// char is alnum/underscore), so the two lock paths are directly comparable
// using the same public `project_key`/`repo_root` functions `lock_path` calls
// internally. This is the SAME defect as P1-P3, not a separate one: it is not
// merely that the two worktrees keep independent claims.json snapshots, but
// that they'd also never contend on the same lock file while writing them,
// so even a load->mutate->save race between the two trees is unserialized.
// ---------------------------------------------------------------------------
#[test]
fn p5_claims_lock_file_path_is_shared_across_main_tree_and_linked_worktree() {
    let fx = Fixture::new("p5-lock-sharing");
    let state_dir = fx.state_dir();

    let main_lock = state_dir
        .join(harness_core::projkey::project_key(
            &harness_core::projkey::repo_root(&fx.main_tree),
        ))
        .join("__claims__.lock");
    let wt_lock = state_dir
        .join(harness_core::projkey::project_key(
            &harness_core::projkey::repo_root(&fx.worktree),
        ))
        .join("__claims__.lock");

    assert_eq!(
        main_lock,
        wt_lock,
        "PROPERTY P5 (lock sharing) violated: the claims-registry RMW lock \
         file (`__claims__.lock`, see crate::lock::lock_path in \
         crates/condukt/src/lock.rs) must resolve to the SAME path for the \
         main tree and its own linked worktree, since they are the SAME \
         repository and must serialize on ONE lock. Computed main-tree lock \
         path: {} ; computed linked-worktree lock path: {} (these must be \
         equal)",
        main_lock.display(),
        wt_lock.display()
    );
}
