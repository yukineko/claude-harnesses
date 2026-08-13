//! End-to-end coverage for `condukt worktree reconcile` — the three-source
//! cross-check (on-disk `git worktree list` × condukt run state × the primary
//! tree) that decides whether a worktree may be deleted.
//!
//! # Why these runs are real
//!
//! Every repo, worktree, commit, dirty file and untracked file here is made by
//! the real `git` binary, and the verdict is read from the real built
//! `condukt` binary's `--json` output. Nothing about git is faked: a fake
//! `git worktree` would prove nothing about the stale-admin-dir and
//! untracked-file cases, which are exactly where the loss surface lives.
//!
//! # What IS injected
//!
//! A live Claude session cannot be spawned from a test, so "a task is running
//! in this worktree" is injected by writing a real run-state JSON into a real
//! condukt state namespace under a temp `$HOME`. The *progress* verdict is not
//! injected: it comes from the real multi-sample progress engine, driven by
//! making a real commit between two real probes.
//!
//! # The direction under test (this is the whole point)
//!
//! For a gate, "cannot determine" blocks the user. For a GC, the restrictive
//! side is **do not delete** — deletion is the only irreversible operation
//! here. So every assertion below that involves an unreadable/unattributable
//! input asserts `removable == false`.
//!
//! # Run state is not a death certificate
//!
//! condukt's run-state index contains only RUNNING tasks that recorded a
//! `worktree`, so "no task claims this path" is the ABSENCE of a claim, not an
//! observation that nothing is working there. The `session-*` trees CLAUDE.md
//! §8 mandates and hand-made `git worktree add` trees are never in that index,
//! so on the real repository every worktree came back `occupancy: dead` —
//! including one that was running a build at that moment and the live session's
//! own tree, which escaped `removable: true` only because they happened to be
//! dirty.
//!
//! Liveness for a path nothing claims must therefore come from an INDEPENDENT
//! witness: the worktree's own activity signal fed through the multi-sample
//! progress engine, or session transcript evidence. With no witness the answer
//! is `undetermined`, and undetermined keeps the directory.
//!
//! The anti-vacuity controls that stop "answer undetermined to everything" from
//! passing this file are `progressing_task_worktree_is_live_and_never_removable`
//! (a claimed, advancing worktree still reads `live`) and
//! `unclaimed_worktree_with_advancing_activity_is_live` (the witness path itself
//! must be able to reach `live` — without it, a constant `undetermined` for
//! every unclaimed path would satisfy every other assertion here). In the
//! permissive direction, `every_undetermined_input_yields_not_removable` pins
//! that the decision function still says yes somewhere, and
//! `dirty_worktree_is_preserved_before_it_becomes_removable` is the remaining
//! END-TO-END test in which a real directory reaches `removable == true`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_condukt")
}

fn run_git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
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
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn git_output(dir: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git runs")
}

struct Fixture {
    base: PathBuf,
    repo: PathBuf,
    wt_base: PathBuf,
    home: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let pid = std::process::id();
        let mut base = std::env::temp_dir();
        base.push(format!("condukt-wtrec-{pid}-{tag}"));
        let _ = std::fs::remove_dir_all(&base);
        let repo = base.join("repo");
        let wt_base = base.join("worktrees");
        let home = base.join("home");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&wt_base).unwrap();
        // The state ROOT must exist: an absent state root is "condukt state is
        // not readable here", which the implementation resolves to
        // undetermined (never to "no runs ⇒ everything is dead").
        std::fs::create_dir_all(home.join(".condukt").join("state")).unwrap();

        run_git(&repo, &["init", "-q", "-b", "main"]);
        run_git(&repo, &["config", "user.email", "t@t.t"]);
        run_git(&repo, &["config", "user.name", "t"]);
        std::fs::write(repo.join("seed.txt"), "seed\n").unwrap();
        run_git(&repo, &["add", "seed.txt"]);
        run_git(&repo, &["commit", "-q", "-m", "seed"]);

        Self {
            base,
            repo,
            wt_base,
            home,
        }
    }

    /// A registered linked worktree of the fixture repo.
    fn add_worktree(&self, name: &str, branch: &str) -> PathBuf {
        let path = self.wt_base.join(name);
        run_git(
            &self.repo,
            &[
                "worktree",
                "add",
                "-q",
                path.to_str().unwrap(),
                "-b",
                branch,
            ],
        );
        path
    }

    /// Write a run-state file into a condukt state namespace. `namespace` is
    /// the per-checkout directory name — passing one that is NOT this repo's
    /// own key is how the cross-checkout scan is exercised.
    fn write_run_state(&self, namespace: &str, run_id: &str, json: &str) {
        let dir = self.home.join(".condukt").join("state").join(namespace);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("{run_id}.json")), json).unwrap();
    }

    fn condukt(&self, args: &[&str]) -> Output {
        self.condukt_in(&self.repo, args)
    }

    /// Same, but run from an arbitrary directory. Under CLAUDE.md §8 every
    /// session works inside a LINKED worktree, so "reconcile invoked from a
    /// linked worktree" is the normal case, not an exotic one.
    fn condukt_in(&self, dir: &Path, args: &[&str]) -> Output {
        Command::new(bin())
            .args(args)
            .current_dir(dir)
            .env("HOME", &self.home)
            .env("CONDUKT_WORKTREE_BASE", &self.wt_base)
            .env_remove("CONDUKT_DISABLE")
            .output()
            .expect("condukt runs")
    }

    fn reconcile_json(&self, extra: &[&str]) -> serde_json::Value {
        self.reconcile_json_in(&self.repo, extra)
    }

    fn reconcile_json_in(&self, dir: &Path, extra: &[&str]) -> serde_json::Value {
        let mut args = vec!["worktree", "reconcile", "--json"];
        args.extend_from_slice(extra);
        let out = self.condukt_in(dir, &args);
        assert!(
            out.status.success(),
            "`condukt worktree reconcile --json` failed (exit {:?}):\nstdout:\n{}\nstderr:\n{}",
            out.status.code(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
            panic!(
                "reconcile --json emitted unparseable JSON: {e}\nstdout:\n{}",
                String::from_utf8_lossy(&out.stdout)
            )
        })
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

/// The report entry whose path ends with `name` (paths are canonicalized by
/// the tool, so compare on the suffix).
fn entry<'a>(report: &'a serde_json::Value, name: &str) -> &'a serde_json::Value {
    let entries = report["entries"]
        .as_array()
        .expect("report has an `entries` array");
    entries
        .iter()
        .find(|e| {
            e["path"]
                .as_str()
                .map(|p| Path::new(p).file_name().and_then(|n| n.to_str()) == Some(name))
                .unwrap_or(false)
        })
        .unwrap_or_else(|| {
            panic!(
                "no entry named {name} in report: {}",
                serde_json::to_string_pretty(report).unwrap()
            )
        })
}

fn removable(report: &serde_json::Value, name: &str) -> bool {
    entry(report, name)["removable"]
        .as_bool()
        .expect("entry has a boolean `removable`")
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

/// A minimal run-state document with one task in `status` pointing at `wt`.
fn run_state_json(run_id: &str, task_id: &str, status: &str, wt: &Path, updated_at: i64) -> String {
    format!(
        r#"{{
  "run_id": "{run_id}",
  "goal": "fixture",
  "tasks": [
    {{
      "id": "{task_id}",
      "status": "{status}",
      "worktree": "{}",
      "branch": "feat/{task_id}",
      "updated_at": {updated_at}
    }}
  ]
}}"#,
        wt.display()
    )
}

// ── 1. The core fix: unattributable ⇒ NOT deletable ────────────────────────

/// The core fix, stated against the SHIPPING command only (no new subcommand):
/// `condukt worktree cleanup --remove` must not delete a directory it cannot
/// attribute to any repo.
///
/// This test is deliberately independent of `worktree reconcile` so that it is
/// observable RED against today's binary for the RIGHT reason — the deletion
/// actually happening — rather than merely because a new subcommand does not
/// parse yet. `worktree::orphans()` pushes an unattributable dir straight onto
/// the list `WtAction::Cleanup` feeds to `remove_dir_all`, and calls that
/// "conservative"; deletion is the irreversible direction, so it is the exact
/// opposite.
#[test]
fn cleanup_remove_does_not_delete_an_unattributable_dir() {
    let f = Fixture::new("cleanup-unattributable");
    let ghost = f.wt_base.join("ghost-dir");
    std::fs::create_dir_all(&ghost).unwrap();
    std::fs::write(ghost.join("work.txt"), "irreplaceable\n").unwrap();

    let out = f.condukt(&["worktree", "cleanup", "--remove"]);
    assert!(
        out.status.success(),
        "cleanup --remove failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        ghost.join("work.txt").exists(),
        "cleanup --remove DELETED a directory it could not attribute to any \
         repo — `git worktree list` never knew it, and its content exists \
         nowhere else. stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A directory under `worktree_base` that cannot be attributed to any repo —
/// no `.git` at all — must NOT be reported deletable, and `worktree cleanup
/// --remove` must leave it on disk.
///
/// This is the fail-open this work exists to close: `worktree::orphans()`
/// pushed exactly this directory onto the deletion list and called it
/// "conservative". `owning_repo_git_dir() == None` means "I cannot attribute
/// this directory", and deletion is irreversible.
#[test]
fn unattributable_dir_is_not_deletable() {
    let f = Fixture::new("unattributable");
    let ghost = f.wt_base.join("ghost-dir");
    std::fs::create_dir_all(&ghost).unwrap();
    std::fs::write(ghost.join("work.txt"), "irreplaceable\n").unwrap();

    let report = f.reconcile_json(&[]);
    let e = entry(&report, "ghost-dir");
    assert_eq!(
        e["attribution"]["value"], "undetermined",
        "a dir with no .git cannot be attributed to a repo; entry: {e}"
    );
    assert!(
        !removable(&report, "ghost-dir"),
        "an unattributable dir must never be removable; entry: {e}"
    );

    let out = f.condukt(&["worktree", "cleanup", "--remove"]);
    assert!(
        out.status.success(),
        "cleanup --remove failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        ghost.join("work.txt").exists(),
        "cleanup --remove deleted an unattributable directory: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Same, for the malformed-pointer shape: a `.git` FILE whose content is not a
/// resolvable `gitdir:` line.
#[test]
fn malformed_git_pointer_dir_is_not_deletable() {
    let f = Fixture::new("malformed");
    let broken = f.wt_base.join("broken-ptr");
    std::fs::create_dir_all(&broken).unwrap();
    std::fs::write(broken.join(".git"), "this is not a gitdir line\n").unwrap();
    std::fs::write(broken.join("work.txt"), "irreplaceable\n").unwrap();

    let report = f.reconcile_json(&[]);
    let e = entry(&report, "broken-ptr");
    assert_eq!(
        e["attribution"]["value"], "undetermined",
        "a malformed .git pointer is unattributable; entry: {e}"
    );
    assert!(
        !removable(&report, "broken-ptr"),
        "an unattributable dir must never be removable; entry: {e}"
    );

    let out = f.condukt(&["worktree", "cleanup", "--remove"]);
    assert!(out.status.success());
    assert!(
        broken.join("work.txt").exists(),
        "cleanup --remove deleted a dir with a malformed .git pointer"
    );
}

// ── 2. Dirty preservation ──────────────────────────────────────────────────

/// A dead worktree carrying BOTH a tracked modification and an untracked file
/// is not removable; after preservation it is; the preserved commit's tree
/// really contains the untracked file's bytes; and preservation does not touch
/// the working tree.
#[test]
fn dirty_worktree_is_preserved_before_it_becomes_removable() {
    let f = Fixture::new("dirty-preserve");
    let wt = f.add_worktree("wt-dirty", "feat/dirty");
    // Tracked modification.
    std::fs::write(wt.join("seed.txt"), "seed\nmodified-by-a-dead-session\n").unwrap();
    // Untracked file — the 2404-line loss surface, in miniature.
    std::fs::write(wt.join("rescue-me.rs"), "fn irreplaceable() {}\n").unwrap();

    let status_before = run_git(&wt, &["status", "--porcelain"]);

    let report = f.reconcile_json(&[]);
    let e = entry(&report, "wt-dirty");
    assert_eq!(e["dirty"]["value"], true, "entry: {e}");
    assert!(
        !removable(&report, "wt-dirty"),
        "a dirty worktree must not be removable before its content is preserved; entry: {e}"
    );

    let report = f.reconcile_json(&["--preserve"]);
    let e = entry(&report, "wt-dirty");
    assert_eq!(
        e["preserved"]["preserved"], true,
        "--preserve must capture the content; entry: {e}"
    );
    assert!(
        removable(&report, "wt-dirty"),
        "once preserved, a dead dirty worktree is removable; entry: {e}"
    );

    let git_ref = e["preserved"]["ref"]
        .as_str()
        .expect("preserved entry names a ref")
        .to_string();

    // The ref must resolve, and its TREE must contain the untracked file's
    // bytes — a ref that merely exists proves nothing.
    let untracked = run_git(&f.repo, &["show", &format!("{git_ref}:rescue-me.rs")]);
    assert_eq!(
        untracked, "fn irreplaceable() {}",
        "the preserved tree must contain the UNTRACKED file's content"
    );
    let tracked = run_git(&f.repo, &["show", &format!("{git_ref}:seed.txt")]);
    assert_eq!(
        tracked, "seed\nmodified-by-a-dead-session",
        "the preserved tree must contain the modified tracked content"
    );

    // The working tree must be untouched by preservation.
    assert_eq!(
        std::fs::read_to_string(wt.join("seed.txt")).unwrap(),
        "seed\nmodified-by-a-dead-session\n"
    );
    assert_eq!(
        std::fs::read_to_string(wt.join("rescue-me.rs")).unwrap(),
        "fn irreplaceable() {}\n"
    );
    assert_eq!(
        run_git(&wt, &["status", "--porcelain"]),
        status_before,
        "preservation must not modify the working tree or the index"
    );
}

/// A preserved ref that no longer matches the worktree's CURRENT content must
/// not authorize deletion: new work written after the preservation re-arms the
/// gate.
#[test]
fn stale_preserved_ref_does_not_authorize_deletion() {
    let f = Fixture::new("stale-preserved");
    let wt = f.add_worktree("wt-stale-pres", "feat/stale-pres");
    std::fs::write(wt.join("first.txt"), "first\n").unwrap();

    let report = f.reconcile_json(&["--preserve"]);
    assert!(removable(&report, "wt-stale-pres"));

    // More work lands after the preservation.
    std::fs::write(wt.join("second.txt"), "second\n").unwrap();

    let report = f.reconcile_json(&[]);
    let e = entry(&report, "wt-stale-pres");
    assert_eq!(
        e["preserved"]["preserved"], false,
        "a preserved ref older than the current content is not a preservation; entry: {e}"
    );
    assert!(
        !removable(&report, "wt-stale-pres"),
        "work added after preservation must re-arm the gate; entry: {e}"
    );
}

// ── 3. Anti-vacuity controls (both directions) ─────────────────────────────

/// (a) REWRITTEN. This fixture — a clean, attributable worktree under a state
/// root that is perfectly readable and simply EMPTY — used to assert
/// `occupancy == "dead"` and `removable == true`, i.e. it wrote "condukt has no
/// run for you, therefore you are dead" down as the specification.
///
/// An empty-but-readable state root is not evidence about this directory at
/// all. It is the state every checkout is in before its first condukt run, and
/// it is what any worktree created outside condukt (every §8 `session-*` tree,
/// every hand-made `git worktree add`) looks like forever. So the same fixture
/// now pins the opposite: no claim plus no witness is `undetermined`, and
/// undetermined keeps the directory.
///
/// What this test STOPS proving, stated rather than glossed over: it was the
/// end-to-end control that a real directory can reach `removable == true` at
/// all, so it was what would have caught an implementation that made the whole
/// GC vacuous. That property is no longer covered by THIS test; see
/// `.scratch/t2-testwriter-notes.md` and the module header for what does and
/// does not still cover it.
#[test]
fn unclaimed_clean_worktree_with_no_witness_is_not_removable() {
    let f = Fixture::new("dead-clean");
    let _wt = f.add_worktree("wt-clean", "feat/clean");

    let report = f.reconcile_json(&[]);
    assert_eq!(
        report["state_scan"]["readable"], true,
        "fixture precondition: the state root is readable, so an undetermined \
         answer here cannot be the already-covered unreadable-state path: {report}"
    );
    assert_eq!(
        report["state_scan"]["runs"], 0,
        "fixture precondition: this state root is EMPTY — the point of the test \
         is that emptiness is not evidence: {report}"
    );

    let e = entry(&report, "wt-clean");
    assert_eq!(e["attribution"]["value"], "this-repo", "entry: {e}");
    assert_eq!(e["dirty"]["value"], false, "entry: {e}");
    assert_eq!(
        e["occupancy"]["value"], "undetermined",
        "a readable-but-empty state root says nothing about who is working in \
         this directory; reporting the KNOWN value `dead` claims an observation \
         that was never made; entry: {e}"
    );
    assert!(
        !e["occupancy"]["reason"]
            .as_str()
            .unwrap_or_default()
            .trim()
            .is_empty(),
        "an undetermined occupancy must say why; entry: {e}"
    );
    assert!(
        !removable(&report, "wt-clean"),
        "undetermined occupancy must never authorize an irreversible delete; entry: {e}"
    );
    assert_eq!(
        report["removable_count"], 0,
        "the report's own summary must agree with the per-entry decision: {report}"
    );
}

/// The core fix, in the shape the defect was measured in: a worktree that NO
/// running condukt task claims must be `undetermined`, never `dead` — even when
/// condukt's run state is complete, readable, and full of other runs.
///
/// Everything else about this entry is positively known and permissive: git
/// registers it, so it is attributable to this repo; `git status` runs and is
/// empty, so it is clean. The ONLY input that used to make it `dead` is the
/// absence of a claim — and the index this absence is read from contains only
/// RUNNING tasks that recorded a `worktree`, so it can never contain a worktree
/// condukt did not create. The run written here claims a DIFFERENT path, so the
/// scan is complete AND non-empty: this cannot pass through the
/// `corrupt_run_state_makes_occupancy_undetermined` path by accident.
#[test]
fn unclaimed_worktree_occupancy_is_undetermined_not_dead() {
    let f = Fixture::new("unclaimed-undetermined");
    let _wt = f.add_worktree("wt-unclaimed", "feat/unclaimed");
    let elsewhere = f.wt_base.join("some-other-tree");
    f.write_run_state(
        "some-other-checkout-beef",
        "run-elsewhere",
        &run_state_json("run-elsewhere", "t1", "running", &elsewhere, now_secs()),
    );

    let report = f.reconcile_json(&[]);
    assert_eq!(
        report["state_scan"]["readable"], true,
        "fixture precondition: the scan must be COMPLETE, otherwise this test \
         would be re-proving the unreadable-state path: {report}"
    );
    assert!(
        report["state_scan"]["running_tasks"]
            .as_u64()
            .unwrap_or_default()
            >= 1,
        "fixture precondition: the scan must have found a real running task, so \
         'nothing claims wt-unclaimed' is a genuine absence of a claim: {report}"
    );

    let e = entry(&report, "wt-unclaimed");
    assert_eq!(e["attribution"]["value"], "this-repo", "entry: {e}");
    assert_eq!(e["dirty"]["value"], false, "entry: {e}");
    assert_eq!(
        e["occupancy"]["value"], "undetermined",
        "condukt's index holds only RUNNING tasks that recorded a worktree, so \
         the absence of a claim is NO INFORMATION about this directory — \
         reporting it as the KNOWN value `dead` is a claim the evidence does \
         not support, and it is how a live session tree was reported dead while \
         it was running a build; entry: {e}"
    );
    let reason = e["occupancy"]["reason"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        !reason.trim().is_empty(),
        "an undetermined occupancy must say WHY — a silent undetermined leaves \
         the operator unable to tell 'checked' from 'could not check'; entry: {e}"
    );
    assert!(
        !reason.contains("could not be read in full"),
        "the run state WAS read in full here (state_scan.readable == true), so \
         blaming an unreadable scan would be a false reason; reason: {reason}"
    );
    assert!(
        !removable(&report, "wt-unclaimed"),
        "a directory whose occupancy is undetermined must not be deletable; entry: {e}"
    );
}

/// (b) The witness path must be able to reach `live` for a worktree NOTHING
/// claims. This is the anti-vacuity control for the fix itself: the two tests
/// above are also satisfied by an implementation that answers a constant
/// `undetermined` to every unclaimed path and observes nothing at all, and such
/// an implementation would report the live `session-*` tree exactly as
/// confidently — and exactly as baselessly — as the old `dead` did.
///
/// The witness is not injected. The first probe has no prior snapshot, so it is
/// undetermined by construction; then real work lands in the worktree (a file
/// appears, which is what a live session looks like to an outside observer: no
/// condukt run state, no commit yet, just content moving) and the second probe
/// observes the worktree's own activity signal advance.
#[test]
fn unclaimed_worktree_with_advancing_activity_is_live() {
    let f = Fixture::new("unclaimed-witness");
    let wt = f.add_worktree("wt-witnessed", "feat/witnessed");

    // Probe 1: anchors the witness. One observation is never a verdict.
    let report = f.reconcile_json(&[]);
    let e = entry(&report, "wt-witnessed");
    assert_eq!(
        e["occupancy"]["value"], "undetermined",
        "a first, unanchored probe cannot witness anything; entry: {e}"
    );
    assert!(!removable(&report, "wt-witnessed"), "entry: {e}");

    // Real work in the worktree, with no condukt run state anywhere.
    std::fs::write(wt.join("worker-notes.md"), "in progress\n").unwrap();

    let report = f.reconcile_json(&[]);
    let e = entry(&report, "wt-witnessed");
    assert_eq!(
        e["occupancy"]["value"], "live",
        "the worktree's own activity advanced between two probes — that is an \
         independent witness of liveness, and it must be consulted for a path \
         no condukt task claims; if this reads `undetermined` the fix is a \
         constant and nothing is actually being observed; entry: {e}"
    );
    assert!(
        !removable(&report, "wt-witnessed"),
        "a live worktree is never removable; entry: {e}"
    );
}

/// (c) A worktree whose owning condukt task is PROGRESSING is live and is
/// never removable — even though it is clean. This is the control that an
/// implementation answering "undetermined" to everything must fail.
///
/// The progress verdict is not injected: the first probe has no prior snapshot
/// (undetermined by construction), then a real commit moves the worktree HEAD,
/// and the second probe observes the advance. The run state is written into a
/// FOREIGN namespace (not this repo's own project key) so this also pins the
/// cross-checkout scan.
#[test]
fn progressing_task_worktree_is_live_and_never_removable() {
    let f = Fixture::new("live-task");
    let wt = f.add_worktree("wt-live", "feat/live");
    let now = now_secs();
    f.write_run_state(
        "some-other-checkout-deadbeef",
        "run-live",
        &run_state_json("run-live", "t1", "running", &wt, now),
    );

    // First probe: anchors the progress snapshot.
    let report = f.reconcile_json(&[]);
    assert!(
        !removable(&report, "wt-live"),
        "a worktree claimed by a running task is never removable on a first, \
         unanchored probe: {}",
        entry(&report, "wt-live")
    );

    // Real durable progress: a commit in the worktree moves its own HEAD.
    std::fs::write(wt.join("progress.txt"), "work\n").unwrap();
    run_git(&wt, &["add", "progress.txt"]);
    run_git(&wt, &["commit", "-q", "-m", "worker commit"]);

    let report = f.reconcile_json(&[]);
    let e = entry(&report, "wt-live");
    assert_eq!(
        e["occupancy"]["value"], "live",
        "a task whose own worktree HEAD advanced is live; entry: {e}"
    );
    assert_eq!(e["dirty"]["value"], false, "entry: {e}");
    assert!(
        !removable(&report, "wt-live"),
        "a live worktree must never be removable, clean or not; entry: {e}"
    );
    // `live` must be BACKED by an observed progress verdict on the claim, not
    // asserted by fiat: the report has to show which task was seen advancing.
    let claims = e["claims"]
        .as_array()
        .expect("the entry carries its claims");
    assert_eq!(claims.len(), 1, "entry: {e}");
    assert_eq!(
        claims[0]["progress"], "progressing",
        "the `live` verdict must be traceable to a claim that was actually \
         observed progressing; entry: {e}"
    );
}

/// An unreadable run-state file must make occupancy undetermined rather than
/// "no run claims it ⇒ dead". A clean worktree that would otherwise be
/// removable stops being removable.
#[test]
fn corrupt_run_state_makes_occupancy_undetermined() {
    let f = Fixture::new("corrupt-state");
    let _wt = f.add_worktree("wt-corrupt", "feat/corrupt");
    f.write_run_state("some-other-checkout-cafe", "run-broken", "{not json at all");

    let report = f.reconcile_json(&[]);
    let e = entry(&report, "wt-corrupt");
    assert_eq!(
        e["occupancy"]["value"], "undetermined",
        "an unreadable run state cannot prove nothing claims this worktree; entry: {e}"
    );
    assert!(
        !removable(&report, "wt-corrupt"),
        "undetermined occupancy must not be removable; entry: {e}"
    );
}

// ── 4. The primary (main) working tree is never removable ──────────────────

#[test]
fn primary_tree_is_never_removable() {
    let f = Fixture::new("primary");

    // clean, unreferenced
    let report = f.reconcile_json(&[]);
    let e = entry(&report, "repo");
    assert_eq!(e["role"], "primary", "entry: {e}");
    assert!(!removable(&report, "repo"), "entry: {e}");

    // dirty
    std::fs::write(f.repo.join("seed.txt"), "seed\ndirty\n").unwrap();
    std::fs::write(f.repo.join("untracked.txt"), "u\n").unwrap();
    let report = f.reconcile_json(&[]);
    assert!(!removable(&report, "repo"), "{}", entry(&report, "repo"));

    // dirty + a running task referencing it
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    f.write_run_state(
        "some-other-checkout-1234",
        "run-main",
        &run_state_json("run-main", "t1", "running", &f.repo, now),
    );
    let report = f.reconcile_json(&[]);
    assert!(!removable(&report, "repo"), "{}", entry(&report, "repo"));

    // clean again, still referenced, and after a --preserve pass
    run_git(&f.repo, &["checkout", "--", "seed.txt"]);
    std::fs::remove_file(f.repo.join("untracked.txt")).unwrap();
    let report = f.reconcile_json(&["--preserve"]);
    assert!(
        !removable(&report, "repo"),
        "the primary tree is never removable, in any state; entry: {}",
        entry(&report, "repo")
    );
}

/// The primary tree must be identified as the primary **no matter which
/// worktree the command is invoked from**.
///
/// Measured 2026-08-12 against the first cut of this feature, run from inside
/// a linked worktree of this very repository: `/Users/yuki/src/harness` — the
/// real main working tree — came back `role: registered`, and the LINKED
/// worktree the command happened to run in came back `role: primary`. The
/// cause is that primary-detection compared against `git rev-parse
/// --show-toplevel`, which answers "the worktree I am standing in", not "this
/// repository's main working tree".
///
/// This is not a cosmetic mislabelling. `is_removable` gives `Role::Primary`
/// an unconditional `false`; demoting main to `registered` hands main's
/// absolute protection back to the ordinary occupancy/dirty path, so a clean
/// main with no condukt task claiming it reads `removable: true`. And under
/// CLAUDE.md §8 every session works inside a linked worktree, so the broken
/// case is the NORMAL invocation, not an edge one — which is exactly what
/// "もちろん main でもする" was asking to cover.
#[test]
fn primary_is_identified_when_invoked_from_a_linked_worktree() {
    let f = Fixture::new("primary-from-linked");
    let linked = f.add_worktree("wt-vantage", "feat/vantage");

    let report = f.reconcile_json_in(&linked, &[]);
    let main_entry = entry(&report, "repo");
    assert_eq!(
        main_entry["role"], "primary",
        "the repository's main working tree must be identified as the primary \
         even when reconcile runs from a linked worktree; entry: {main_entry}"
    );
    assert!(
        !removable(&report, "repo"),
        "main must keep its unconditional protection from every vantage point; \
         entry: {main_entry}"
    );

    // Exactly one entry may claim the primary role, otherwise "which one is
    // main?" is answered twice and the protection stops meaning anything.
    let primaries: Vec<&str> = report["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["role"] == "primary")
        .map(|e| e["path"].as_str().unwrap())
        .collect();
    assert_eq!(
        primaries.len(),
        1,
        "exactly one entry is the primary tree; got {primaries:?}"
    );
    assert!(
        primaries[0].ends_with("repo"),
        "the primary is the main working tree, not the vantage point; got {primaries:?}"
    );
}

// ── 5. Stale (unregistered) worktree of this repo ──────────────────────────

/// A worktree of THIS repo whose git admin dir was destroyed (an incomplete
/// `git worktree remove`): git can no longer run inside it, so its dirtiness
/// cannot be read from `git status`. That is "cannot determine", and it must
/// not be deleted. `worktree cleanup --remove` must leave it alone.
#[test]
fn stale_worktree_with_destroyed_admin_dir_is_not_deleted() {
    let f = Fixture::new("stale-admin");
    let wt = f.add_worktree("wt-stale", "feat/stale");
    std::fs::write(wt.join("unfinished.rs"), "fn unfinished() {}\n").unwrap();
    std::fs::remove_dir_all(f.repo.join(".git").join("worktrees").join("wt-stale")).unwrap();

    // git itself no longer registers it.
    let listed = run_git(&f.repo, &["worktree", "list", "--porcelain"]);
    assert!(
        !listed.contains("wt-stale"),
        "fixture precondition: the worktree must be unregistered; got {listed}"
    );

    let report = f.reconcile_json(&[]);
    let e = entry(&report, "wt-stale");
    assert_eq!(
        e["attribution"]["value"], "this-repo",
        "a stale worktree still points back at this repo; entry: {e}"
    );
    assert!(
        !removable(&report, "wt-stale"),
        "a stale worktree whose dirtiness cannot be read must not be removable; entry: {e}"
    );

    let out = f.condukt(&["worktree", "cleanup", "--remove"]);
    assert!(out.status.success());
    assert!(
        wt.join("unfinished.rs").exists(),
        "cleanup --remove destroyed unfinished work in a stale worktree"
    );
}

/// A live worktree belonging to a DIFFERENT repo that happens to share the
/// worktree base is attributed to that other repo and is never removable here.
#[test]
fn other_repos_worktree_is_never_removable() {
    let f = Fixture::new("other-repo");
    let other = f.base.join("other-repo");
    std::fs::create_dir_all(&other).unwrap();
    run_git(&other, &["init", "-q", "-b", "main"]);
    run_git(&other, &["config", "user.email", "t@t.t"]);
    run_git(&other, &["config", "user.name", "t"]);
    std::fs::write(other.join("o.txt"), "o\n").unwrap();
    run_git(&other, &["add", "o.txt"]);
    run_git(&other, &["commit", "-q", "-m", "seed"]);
    let other_wt = f.wt_base.join("other-wt");
    run_git(
        &other,
        &[
            "worktree",
            "add",
            "-q",
            other_wt.to_str().unwrap(),
            "-b",
            "feat/other",
        ],
    );

    let report = f.reconcile_json(&[]);
    let e = entry(&report, "other-wt");
    assert_eq!(e["attribution"]["value"], "other-repo", "entry: {e}");
    assert!(!removable(&report, "other-wt"), "entry: {e}");

    let out = f.condukt(&["worktree", "cleanup", "--remove"]);
    assert!(out.status.success());
    assert!(
        other_wt.join("o.txt").exists(),
        "cleanup --remove deleted another repo's live worktree"
    );
}

// ── 6. The removable decision, driven over its whole input space ───────────
//
// The pure decision function lives in the binary crate (condukt has no lib
// target), so it is driven here through the CLI's own debug surface:
// `condukt worktree reconcile --decision-table` prints one row per
// (role × attribution × occupancy × dirty × preserved) combination with the
// decision the SAME function produced. The assertions below are behavioural —
// they are about the boolean decision, never about the shape of a
// `Determination` — because an `Undetermined` arm written `=> {}` compiles and
// a shape assertion on it can never be observed RED.

#[test]
fn every_undetermined_input_yields_not_removable() {
    let f = Fixture::new("truth-table");
    let out = f.condukt(&["worktree", "reconcile", "--decision-table"]);
    assert!(
        out.status.success(),
        "--decision-table failed (exit {:?}): {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    let table: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "--decision-table emitted unparseable JSON: {e}\n{}",
            String::from_utf8_lossy(&out.stdout)
        )
    });
    let rows = table["rows"].as_array().expect("table has rows");

    // 3 roles × 3 attributions × 3 occupancies × 3 dirtiness × 2 preserved.
    assert_eq!(
        rows.len(),
        3 * 3 * 3 * 3 * 2,
        "the table must cover the whole input space"
    );

    let mut undetermined_rows = 0usize;
    let mut removable_rows = 0usize;
    for row in rows {
        let has_undetermined = ["attribution", "occupancy", "dirty"]
            .iter()
            .any(|k| row[*k] == "undetermined");
        let decision = row["removable"].as_bool().expect("row has removable");
        if has_undetermined {
            undetermined_rows += 1;
            assert!(
                !decision,
                "an undetermined input must never be removable; row: {row}"
            );
        }
        if row["role"] == "primary" {
            assert!(
                !decision,
                "the primary tree must never be removable; row: {row}"
            );
        }
        if row["occupancy"] == "live" {
            assert!(!decision, "a live worktree is never removable; row: {row}");
        }
        if row["dirty"] == "true" && row["preserved"] == false {
            assert!(
                !decision,
                "unpreserved dirty work is never removable; row: {row}"
            );
        }
        if decision {
            removable_rows += 1;
        }
    }
    assert!(
        undetermined_rows > 0,
        "anti-vacuity: the table must actually contain undetermined rows"
    );
    assert!(
        removable_rows > 0,
        "anti-vacuity: a table where NOTHING is removable would satisfy every \
         assertion above without deciding anything"
    );
}

// ── 7. Wiring: deletion is reachable only through the gate ─────────────────

/// `cleanup --remove` acts ONLY through the gate: with nothing removable it
/// deletes nothing, leaves the registered worktree alone (a registered
/// worktree is git's to remove, not GC debris), and reports what it refused to
/// delete rather than staying silent about it.
#[test]
fn cleanup_remove_deletes_nothing_when_nothing_is_removable() {
    let f = Fixture::new("cleanup-wiring");
    let ghost = f.wt_base.join("ghost-dir");
    std::fs::create_dir_all(&ghost).unwrap();
    let wt = f.add_worktree("wt-registered", "feat/reg");

    let out = f.condukt(&["worktree", "cleanup", "--remove"]);
    assert!(out.status.success());
    assert!(ghost.exists(), "unattributable dir was deleted");
    assert!(wt.exists(), "registered worktree was deleted by cleanup");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("ghost-dir"),
        "cleanup must REPORT what it refused to delete and why; stderr:\n{stderr}"
    );
}

/// `git worktree list` disagreeing with the on-disk truth is itself surfaced:
/// the report contains the primary tree, the registered worktrees, and the
/// unregistered directories, all in one document.
#[test]
fn report_enumerates_all_three_sources() {
    let f = Fixture::new("three-sources");
    f.add_worktree("wt-a", "feat/a");
    std::fs::create_dir_all(f.wt_base.join("ghost-dir")).unwrap();

    let report = f.reconcile_json(&[]);
    assert_eq!(entry(&report, "repo")["role"], "primary");
    assert_eq!(entry(&report, "wt-a")["role"], "registered");
    assert_eq!(entry(&report, "ghost-dir")["role"], "unregistered");
    assert!(
        report["state_scan"]["readable"].as_bool().is_some(),
        "the report must state whether condukt's run state was readable: {report}"
    );
}

/// Sanity: git itself is present and the output shape of `git worktree list
/// --porcelain` is what the implementation parses (a fixture guard, so a git
/// upgrade that changes the format fails loudly here rather than silently
/// producing an empty enumeration).
#[test]
fn git_worktree_list_porcelain_shape_is_as_expected() {
    let f = Fixture::new("porcelain-shape");
    f.add_worktree("wt-shape", "feat/shape");
    let out = git_output(&f.repo, &["worktree", "list", "--porcelain"]);
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.starts_with("worktree "), "unexpected shape: {text}");
    assert_eq!(
        text.matches("worktree ").count(),
        2,
        "primary + one linked worktree expected: {text}"
    );
}
