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
// P5: the registry's RMW lock (`__claims__.lock`) is ONE lock for the whole
// repo, observed THROUGH condukt.
//
// This is the SAME defect as P1-P3 seen from the write side: it is not merely
// that the two trees kept independent `claims.json` snapshots, but that they
// would never contend on the same lock file while writing them, so even a
// load->mutate->save race between the two trees was unserialized.
//
// The previous version of this test computed
// `project_key(repo_root(main))` vs `project_key(repo_root(worktree))` itself
// and asserted they were equal. That called NO condukt code: it was an
// assertion about `repo_root`, which this fix deliberately does NOT change
// (re-keying it would orphan every existing run-state/precedent/autoflow file
// — that wider migration is backlog `43393ce2`). So it could never pass and
// proved nothing about the lock. Filed as backlog `dccd7394` and replaced here
// with a behavioural observation.
//
// Mechanism (same one `tests/run_lock_concurrency.rs` uses):
// `CONDUKT_TEST_CLAIM_DELAY_MS` widens `claim_tasks`' load->check->save section
// *while the `__claims__` lock is held*, so a holder can be parked inside the
// critical section for longer than `RunLock::DEADLINE` (10s) and a second,
// independently-spawned `condukt state claim-task` deterministically hits the
// contended path. Contention past the deadline is a fail-CLOSED hard skip
// stamped with the synthetic holder `__lock_contended__` (see
// `claim.rs`'s `LOCK_CONTENDED_HOLDER`).
// ---------------------------------------------------------------------------

/// Recursively look for a file named exactly `__claims__.lock` under `dir`
/// (the transient `__claims__.lock.tmp.*` publish files are NOT matched).
/// Observing the lock file condukt itself created is deliberate: it is the one
/// way to know the holder is inside the critical section without re-deriving
/// the path this test is supposed to be checking.
fn find_claims_lock(dir: &Path) -> Option<PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() {
            if let Some(found) = find_claims_lock(&p) {
                return Some(found);
            }
        } else if p
            .file_name()
            .map(|n| n == "__claims__.lock")
            .unwrap_or(false)
        {
            return Some(p);
        }
    }
    None
}

/// Block until the holder process has published `__claims__.lock`, so the
/// contender below is spawned while the critical section is genuinely held.
fn wait_for_claims_lock(state_dir: &Path) -> PathBuf {
    let start = std::time::Instant::now();
    while start.elapsed() < std::time::Duration::from_secs(20) {
        if let Some(p) = find_claims_lock(state_dir) {
            return p;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    panic!(
        "the holder never published a `__claims__.lock` under {} within 20s — the \
         test could not establish the contention it is supposed to observe",
        state_dir.display()
    );
}

#[test]
fn p5_claim_from_main_tree_contends_on_the_same_claims_lock_as_the_linked_worktree() {
    let fx = Fixture::new("p5-lock-sharing");

    // Holder: claims from the LINKED WORKTREE and parks inside the
    // `__claims__`-locked critical section for longer than RunLock::DEADLINE.
    let holder = Command::new(bin())
        .args([
            "state",
            "claim-task",
            "--run",
            "runHold",
            "--hashkey",
            "H-hold",
        ])
        .current_dir(&fx.worktree)
        .env("HOME", &fx.home)
        .env("CLAUDE_CODE_SESSION_ID", "sess-hold")
        .env("CONDUKT_TEST_CLAIM_DELAY_MS", "14000")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn holder condukt");

    let lock_path = wait_for_claims_lock(&fx.state_dir());

    // Contender: a DIFFERENT run claiming a DIFFERENT hashkey, from the MAIN
    // TREE. The hashkey is different on purpose — a skip here can only come
    // from the lock, never from a hashkey collision with the holder's claim.
    // No delay env: it must not park anywhere itself.
    let contender = fx.condukt_in(
        &fx.main_tree,
        &[
            "state",
            "claim-task",
            "--run",
            "runContend",
            "--hashkey",
            "H-contend",
        ],
    );

    let holder_out = holder.wait_with_output().expect("wait on holder");
    assert!(
        holder_out.status.success(),
        "P5 setup: the holder's own claim (from the linked worktree) should have \
         succeeded: exit={:?} stdout={} stderr={}",
        holder_out.status.code(),
        String::from_utf8_lossy(&holder_out.stdout),
        String::from_utf8_lossy(&holder_out.stderr)
    );

    let c_out = stdout_str(&contender);
    let c_err = String::from_utf8_lossy(&contender.stderr).into_owned();
    let c_json: serde_json::Value = serde_json::from_str(&c_out).unwrap_or_else(|e| {
        panic!(
            "PROPERTY P5: the contender's stdout was not valid claim-outcome JSON \
             ({e}) — it likely crashed instead of being cleanly hard-skipped: \
             stdout={c_out:?} stderr={c_err:?}"
        )
    });

    assert_eq!(
        c_json["claimed"],
        serde_json::json!([]),
        "PROPERTY P5 (one lock per repo) violated: a `state claim-task` run with \
         cwd=the MAIN TREE while another run holds the claims-registry lock from \
         cwd=a LINKED WORKTREE of the SAME repo must claim NOTHING. It claimed \
         something, which means the two trees took DIFFERENT `__claims__.lock` \
         files and read-modify-wrote the shared registry unserialized. Holder's \
         lock file was {}; contender stdout={c_json} stderr={c_err}",
        lock_path.display()
    );
    let skipped = c_json["skipped"]
        .as_array()
        .unwrap_or_else(|| panic!("contender JSON has no `skipped` array: {c_json}"));
    assert_eq!(
        skipped.len(),
        1,
        "PROPERTY P5: the one requested hashkey must be hard-skipped; got {c_json}"
    );
    assert_eq!(
        skipped[0]["holder_run"],
        serde_json::json!("__lock_contended__"),
        "PROPERTY P5 (one lock per repo) violated: the main-tree claim must be \
         hard-skipped by LOCK CONTENTION (synthetic holder `__lock_contended__`, \
         `claim.rs`'s LOCK_CONTENDED_HOLDER) with the linked worktree holding the \
         same `__claims__.lock` ({}). Got: {c_json} stderr={c_err}",
        lock_path.display()
    );
    assert_eq!(
        contender.status.code(),
        Some(1),
        "PROPERTY P5: a hard skip must exit 1 (main.rs's StateAction::ClaimTask); \
         got {:?} with stdout={c_json} stderr={c_err}",
        contender.status.code()
    );
}

/// Anti-vacuity control for P5: without contention, the very same main-tree
/// claim SUCCEEDS. Without this the P5 assertion above would also hold if
/// main-tree claims were simply always skipped, for any reason at all.
#[test]
fn p5_control_uncontended_main_tree_claim_succeeds() {
    let fx = Fixture::new("p5-lock-control");

    let out = fx.condukt_in(
        &fx.main_tree,
        &[
            "state",
            "claim-task",
            "--run",
            "runContend",
            "--hashkey",
            "H-contend",
        ],
    );
    let s = stdout_str(&out);
    assert!(
        out.status.success(),
        "control: an UNCONTENDED main-tree claim must succeed (exit 0); got {:?} \
         stdout={s} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_str(&s).expect("control: valid JSON");
    assert_eq!(
        v["claimed"],
        serde_json::json!(["H-contend"]),
        "control: the uncontended claim must actually claim the hashkey; got {v}"
    );
}
