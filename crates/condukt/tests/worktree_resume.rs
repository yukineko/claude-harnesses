//! End-to-end coverage for `condukt worktree resume-candidates` — the *resume*
//! consumer of the `worktree reconcile` report (backlog `0cf3775c`).
//!
//! # The gap this pins
//!
//! `worktree reconcile --json` already computes, for every worktree on disk,
//! a three-valued live/dead/undetermined verdict cross-checked against
//! condukt's run state. It has exactly ONE consumer today — the GC
//! (`WtAction::Cleanup`), which asks "may I delete this?". Nothing consumes it
//! for the opposite purpose: "may the next session PICK THIS UP?". So
//! "an interrupted session's work can be resumed by the next session" was a
//! surface with no path attached.
//!
//! The other half already exists but is run-scoped and requires you to already
//! know the run id: `condukt state resume-context --run <RID>`. The missing
//! piece is DISCOVERY, and the new path must **feed** that command rather than
//! reimplement it.
//!
//! # The direction is INVERTED again — twice over
//!
//! For a gate, "cannot determine" blocks the user. For the GC, the restrictive
//! side is "do not delete". For RESUME, the restrictive side is a third thing:
//! **do not resume AND do not discard**. Handing a still-live session's
//! worktree to a second session is the shared-index collision CLAUDE.md §8
//! names as the one conflict git cannot resolve; and quietly writing a
//! worktree off as "nothing to resume here" loses the work just as surely as
//! deleting it. So:
//!
//! * `held` — positively occupied. Never offered.
//! * `resumable` — positively unoccupied AND condukt run state holds a
//!   non-terminal task recorded at this path.
//! * `undetermined` — everything else. Neither resumed nor discarded.
//!
//! # Why these runs are real
//!
//! Every repo, worktree and commit here is made by the real `git` binary, and
//! every verdict is read from the real built `condukt` binary's `--json`
//! output. Where a run's existence is the fixture, it is created by the real
//! `condukt state init` / `state set` commands so the run lands in THIS
//! checkout's own state namespace — which is what makes the emitted
//! `resume_command` genuinely runnable rather than merely well-formed. The
//! foreign-namespace cases are injected as raw JSON, exactly as
//! `worktree_reconcile.rs` does, because a live Claude session cannot be
//! spawned from a test.
//!
//! The `held` verdict is NOT injected: it comes from the real multi-sample
//! progress engine, driven by making a real commit between two real probes.
//!
//! # Anti-vacuity
//!
//! An implementation answering `undetermined` to everything would satisfy every
//! restrictive assertion in this file. Two controls exist so that it cannot:
//! [`interrupted_failed_task_worktree_is_resumable`] (the positive direction)
//! and [`live_task_worktree_is_held_never_resumable`] (the restrictive
//! direction, which must be `held` specifically — not `undetermined`).

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

struct Fixture {
    base: PathBuf,
    repo: PathBuf,
    wt_base: PathBuf,
    home: PathBuf,
    state_dir: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let pid = std::process::id();
        let mut base = std::env::temp_dir();
        base.push(format!("condukt-wtresume-{pid}-{tag}"));
        let _ = std::fs::remove_dir_all(&base);
        let repo = base.join("repo");
        let wt_base = base.join("worktrees");
        let home = base.join("home");
        let state_dir = home.join(".condukt").join("state");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&wt_base).unwrap();
        // The state ROOT must exist: an absent state root is "condukt state is
        // not readable here", which reconcile resolves to undetermined (never
        // to "no runs ⇒ everything is dead" — and therefore never, here, to
        // "everything is free to resume").
        std::fs::create_dir_all(&state_dir).unwrap();
        // Claude Code's per-project transcript root, for the same reason: an
        // ABSENT store is "whether a session works here was never looked up",
        // which the death rule resolves to undetermined — and therefore never
        // to "free to resume". Present-but-empty is the observation that no
        // session has ever run in these worktrees.
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
            wt_base,
            home,
            state_dir,
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

    /// Write a run-state file into a condukt state namespace directly.
    /// `namespace` is the per-checkout directory name — passing one that is NOT
    /// this repo's own key is how the cross-checkout scan is exercised.
    fn write_run_state(&self, namespace: &str, run_id: &str, json: &str) -> PathBuf {
        let dir = self.state_dir.join(namespace);
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(format!("{run_id}.json"));
        std::fs::write(&p, json).unwrap();
        p
    }

    fn condukt(&self, args: &[&str]) -> Output {
        Command::new(bin())
            .args(args)
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env("CONDUKT_WORKTREE_BASE", &self.wt_base)
            // Collapse the multi-sample window to zero so these tests need no
            // wall-clock wait. It does NOT turn one observation into a freeze:
            // the engine still requires two samples with an unchanged
            // fingerprint, which is what `settle` below supplies.
            .env("HARNESS_PROGRESS_WINDOW_SECS", "0")
            .env_remove("CONDUKT_DISABLE")
            .output()
            .expect("condukt runs")
    }

    /// Advance the progress state machine by one probe, discarding the result.
    ///
    /// Death is established, not inferred from the absence of a claim, and a
    /// single observation can never establish a freeze — so any test whose
    /// fixture precondition is "nobody is working here" must anchor a first
    /// sample before reading the verdict.
    fn settle(&self) {
        let _ = self.reconcile_json();
    }

    fn json_of(&self, args: &[&str]) -> serde_json::Value {
        let out = self.condukt(args);
        assert!(
            out.status.success(),
            "`condukt {}` failed (exit {:?}):\nstdout:\n{}\nstderr:\n{}",
            args.join(" "),
            out.status.code(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
            panic!(
                "`condukt {}` emitted unparseable JSON: {e}\nstdout:\n{}",
                args.join(" "),
                String::from_utf8_lossy(&out.stdout)
            )
        })
    }

    fn resume_json(&self) -> serde_json::Value {
        self.json_of(&["worktree", "resume-candidates", "--json"])
    }

    fn reconcile_json(&self) -> serde_json::Value {
        self.json_of(&["worktree", "reconcile", "--json"])
    }

    /// Seed a real run in THIS checkout's own state namespace via the real
    /// `state init`, so `state resume-context --run <rid>` can actually load it.
    /// Returns the run id.
    fn init_run(&self, tasks_json: &str) -> String {
        let decomp = self.base.join("decomp.json");
        std::fs::write(
            &decomp,
            format!(r#"{{"goal":"fixture","tasks":{tasks_json}}}"#),
        )
        .unwrap();
        let out = self.condukt(&["state", "init", "--file", decomp.to_str().unwrap()]);
        assert!(
            out.status.success(),
            "state init failed (exit {:?}):\nstdout:\n{}\nstderr:\n{}",
            out.status.code(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        stdout
            .lines()
            .chain(stderr.lines())
            .rev()
            .map(str::trim)
            .find(|l| l.starts_with("run-"))
            .unwrap_or_else(|| panic!("no run id in state init output:\n{stdout}\n{stderr}"))
            .to_string()
    }

    fn set_task(&self, run: &str, task: &str, status: &str, worktree: &Path, branch: &str) {
        let out = self.condukt(&[
            "state",
            "set",
            "--run",
            run,
            "--task",
            task,
            "--status",
            status,
            "--worktree",
            worktree.to_str().unwrap(),
            "--branch",
            branch,
        ]);
        assert!(
            out.status.success(),
            "state set failed (exit {:?}): {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// The on-disk run-state file for `rid`, wherever the binary namespaced it.
    fn run_state_path(&self, rid: &str) -> PathBuf {
        let mut found = Vec::new();
        for e in std::fs::read_dir(&self.state_dir).expect("state root readable") {
            let p = e.expect("dir entry").path().join(format!("{rid}.json"));
            if p.exists() {
                found.push(p);
            }
        }
        assert_eq!(found.len(), 1, "expected one run-state file for {rid}");
        found.remove(0)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

/// The candidate whose path ends with `name` (paths are canonicalized by the
/// tool, so compare on the basename).
fn candidate<'a>(report: &'a serde_json::Value, name: &str) -> &'a serde_json::Value {
    let list = report["candidates"]
        .as_array()
        .unwrap_or_else(|| panic!("report has a `candidates` array; got: {report}"));
    list.iter()
        .find(|c| {
            c["path"]
                .as_str()
                .map(|p| Path::new(p).file_name().and_then(|n| n.to_str()) == Some(name))
                .unwrap_or(false)
        })
        .unwrap_or_else(|| {
            panic!(
                "no candidate named {name}; the classification list must contain \
                 every judged worktree, including the ones it refuses to offer \
                 (verdict `held` / `undetermined`), otherwise 'not offered' and \
                 'not looked at' are indistinguishable. report:\n{}",
                serde_json::to_string_pretty(report).unwrap()
            )
        })
}

fn verdict(report: &serde_json::Value, name: &str) -> String {
    candidate(report, name)["verdict"]
        .as_str()
        .unwrap_or_else(|| {
            panic!(
                "candidate {name} has no string `verdict`: {}",
                candidate(report, name)
            )
        })
        .to_string()
}

fn resumable_count(report: &serde_json::Value) -> u64 {
    report["resumable_count"]
        .as_u64()
        .unwrap_or_else(|| panic!("report has a numeric `resumable_count`; got: {report}"))
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

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

// ── 1. The consumer exists, and it consumes the reconcile REPORT ───────────

/// `worktree resume-candidates --json` emits the fixed shape, and its
/// `state_scan` is byte-for-byte the one `worktree reconcile --json` reports
/// for the same fixture.
///
/// That equality is the observable proof that the new path consumes the
/// existing report rather than re-deriving its own view of condukt's run state.
/// A second, independent derivation is exactly how the GC and the resume path
/// would drift apart — one concluding "dead, delete it" while the other
/// concluded "alive, hands off".
///
/// The fixture is deliberately NOT empty: it has two namespaces, two runs and
/// one running task, so `state_scan` carries real counts. An empty state root
/// would make this equality trivially satisfiable by any implementation that
/// hardcodes `{readable: true, 0, 0, 0}`.
#[test]
fn resume_candidates_consumes_the_same_report_as_reconcile() {
    let f = Fixture::new("consumes-report");
    let wt = f.add_worktree("wt-idle", "feat/idle");
    let other = f.add_worktree("wt-busy", "feat/busy");

    // Namespace A: this checkout's own, created by the real binary.
    let rid = f.init_run(r#"[{"id":"t1","title":"x","touched_files":["a.rs"],"deps":[],"class":"serial","done_criteria":"d"}]"#);
    f.set_task(&rid, "t1", "failed", &wt, "feat/idle");
    // Namespace B: a foreign checkout with a RUNNING task.
    f.write_run_state(
        "some-other-checkout-deadbeef",
        "run-foreign",
        &run_state_json("run-foreign", "t9", "running", &other, now_secs()),
    );

    let resume = f.resume_json();
    let recon = f.reconcile_json();

    assert_eq!(
        resume["repo"], recon["repo"],
        "both views must be about the same repository"
    );
    assert_eq!(
        resume["state_scan"],
        recon["state_scan"],
        "resume-candidates must forward the reconcile report's `state_scan` \
         VERBATIM — a separately derived scan is a second source of truth that \
         can disagree with the one the GC acts on.\nresume: {}\nreconcile: {}",
        serde_json::to_string_pretty(&resume["state_scan"]).unwrap(),
        serde_json::to_string_pretty(&recon["state_scan"]).unwrap()
    );
    // Anti-triviality guard on the fixture itself.
    assert_eq!(
        recon["state_scan"]["readable"], true,
        "fixture precondition: the scan must be readable here"
    );
    assert_eq!(
        recon["state_scan"]["runs"].as_u64(),
        Some(2),
        "fixture precondition: two runs across two namespaces; state_scan: {}",
        recon["state_scan"]
    );

    let list = resume["candidates"]
        .as_array()
        .unwrap_or_else(|| panic!("report has a `candidates` array; got: {resume}"));
    assert!(
        !list.is_empty(),
        "the classification list must not be empty for a fixture with worktrees \
         on disk: {resume}"
    );
    for c in list {
        let v = c["verdict"]
            .as_str()
            .unwrap_or_else(|| panic!("every candidate carries a string `verdict`; got: {c}"));
        assert!(
            matches!(v, "resumable" | "held" | "undetermined"),
            "the verdict is three-valued and nothing else; got {v:?} in {c}"
        );
        assert!(
            c["path"].as_str().is_some(),
            "every candidate names the worktree it is about: {c}"
        );
        assert!(
            c["reason"].as_str().is_some(),
            "every verdict states WHY — silence about a worktree that is not \
             offered leaves 'checked and held' indistinguishable from 'could \
             not check': {c}"
        );
    }

    // The branch is carried through from the report so the operator can see
    // what would be resumed.
    assert_eq!(
        candidate(&resume, "wt-idle")["branch"],
        "feat/idle",
        "candidate: {}",
        candidate(&resume, "wt-idle")
    );

    assert_eq!(
        resumable_count(&resume),
        resume["candidates"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|c| c["verdict"] == "resumable")
            .count() as u64,
        "`resumable_count` must equal the number of `resumable` candidates — a \
         count that disagrees with the list is a summary the operator can act \
         on without ever seeing what it summarizes: {resume}"
    );
}

// ── 2. Anti-vacuity, positive direction: `resumable` actually fires ────────

/// An interrupted session's worktree — nothing running in it, a run-state task
/// recorded at that path whose status is non-terminal (`failed`) — IS offered
/// for resume, and the offer names the run id `state resume-context` needs.
///
/// Without this control an implementation that answered `undetermined` to
/// everything would satisfy every other assertion in this file while attaching
/// no path at all to the surface this work exists to close.
#[test]
fn interrupted_failed_task_worktree_is_resumable() {
    let f = Fixture::new("resumable");
    let wt = f.add_worktree("wt-interrupted", "feat/interrupted");
    let rid = f.init_run(r#"[{"id":"t1","title":"x","touched_files":["a.rs"],"deps":[],"class":"serial","done_criteria":"d"}]"#);
    f.set_task(&rid, "t1", "failed", &wt, "feat/interrupted");

    // Fixture precondition, read from the report the new path consumes: this
    // worktree is POSITIVELY unoccupied — no RUNNING task claims it (from a
    // fully readable scan) AND its own signals were observed frozen across the
    // window with no session transcript growing against its path. `dead` is
    // what makes `resumable` reachable at all, and it now takes two probes to
    // establish, because the absence of a claim alone never established it.
    f.settle();
    let recon = f.reconcile_json();
    let e = recon["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["path"].as_str().unwrap().ends_with("wt-interrupted"))
        .expect("reconcile reports the worktree");
    assert_eq!(
        e["occupancy"]["value"], "dead",
        "fixture precondition: nobody is working here; entry: {e}"
    );

    let resume = f.resume_json();
    let c = candidate(&resume, "wt-interrupted");
    assert_eq!(
        c["verdict"], "resumable",
        "a positively unoccupied worktree whose task is non-terminal is exactly \
         the interrupted work the next session should pick up; candidate: {c}"
    );
    assert_eq!(
        c["run_id"], rid,
        "the offer must name the run `state resume-context` is scoped to; \
         candidate: {c}"
    );
    assert_eq!(c["task_id"], "t1", "candidate: {c}");
    assert_eq!(
        c["resume_command"],
        serde_json::Value::String(format!("condukt state resume-context --run {rid}")),
        "candidate: {c}"
    );
    assert_eq!(
        resumable_count(&resume),
        1,
        "exactly one worktree is resumable in this fixture: {resume}"
    );
}

/// A task in a TERMINAL status is finished work, not interrupted work: its
/// worktree must not be offered for resume.
///
/// This is the other half of the `resumable` rule. Without it, "resumable"
/// degenerates to "any dead worktree condukt has ever heard of", and the next
/// session is pointed at runs that have nothing left to do.
#[test]
fn verified_task_worktree_is_not_resumable() {
    let f = Fixture::new("terminal");
    let wt = f.add_worktree("wt-finished", "feat/finished");
    let rid = f.init_run(r#"[{"id":"t1","title":"x","touched_files":["a.rs"],"deps":[],"class":"serial","done_criteria":"d"}]"#);
    f.set_task(&rid, "t1", "verified", &wt, "feat/finished");

    let resume = f.resume_json();
    assert_ne!(
        verdict(&resume, "wt-finished"),
        "resumable",
        "a verified task has no unfinished work to resume; candidate: {}",
        candidate(&resume, "wt-finished")
    );
    assert_eq!(
        resumable_count(&resume),
        0,
        "nothing in this fixture is resumable: {resume}"
    );
}

// ── 3. Anti-vacuity, restrictive direction: `held` actually fires ──────────

/// A worktree whose owning condukt task is PROGRESSING is `held` — another
/// session is working there and it must never be offered.
///
/// `held` specifically, not `undetermined`: the two are different facts and
/// collapsing "I know someone is there" into "I cannot tell" would let a future
/// change relax the restrictive side without any test noticing.
///
/// The occupancy verdict is not injected. The first probe has no prior snapshot
/// (undetermined by construction), then a real commit moves the worktree HEAD,
/// and the second probe observes the advance. The run state is written into a
/// FOREIGN namespace so this also pins the cross-checkout scan: a worktree
/// claimed by a run created in another checkout is still held.
#[test]
fn live_task_worktree_is_held_never_resumable() {
    let f = Fixture::new("held");
    let wt = f.add_worktree("wt-live", "feat/live");
    f.write_run_state(
        "some-other-checkout-deadbeef",
        "run-live",
        &run_state_json("run-live", "t1", "running", &wt, now_secs()),
    );

    // First probe: anchors the progress snapshot. An unanchored probe cannot
    // know anybody is there — and must therefore NOT offer the worktree.
    let first = f.resume_json();
    assert_ne!(
        verdict(&first, "wt-live"),
        "resumable",
        "a worktree claimed by a running task must never be offered on a first, \
         unanchored probe; candidate: {}",
        candidate(&first, "wt-live")
    );

    // Real durable progress: a commit in the worktree moves its own HEAD.
    std::fs::write(wt.join("progress.txt"), "work\n").unwrap();
    run_git(&wt, &["add", "progress.txt"]);
    run_git(&wt, &["commit", "-q", "-m", "worker commit"]);

    let second = f.resume_json();
    let c = candidate(&second, "wt-live");
    assert_eq!(
        c["verdict"], "held",
        "a task whose own worktree HEAD advanced is positively alive: handing \
         its worktree to a second session is the shared-index collision \
         CLAUDE.md §8 names as the one conflict git cannot resolve; \
         candidate: {c}"
    );
    assert_eq!(
        resumable_count(&second),
        0,
        "nothing is resumable while the only worktree is held: {second}"
    );
}

// ── 4. Undetermined resolves restrictively ─────────────────────────────────

/// (a) An unreadable run-state file makes the scan incomplete: "no run claims
/// this worktree" can no longer be concluded, so the worktree is neither
/// offered nor written off.
#[test]
fn corrupt_run_state_makes_the_candidate_undetermined() {
    let f = Fixture::new("corrupt");
    let _wt = f.add_worktree("wt-corrupt", "feat/corrupt");
    f.write_run_state("some-other-checkout-cafe", "run-broken", "{not json at all");

    let resume = f.resume_json();
    assert_eq!(
        resume["state_scan"]["readable"], false,
        "fixture precondition: one unparseable run file makes the scan \
         incomplete: {resume}"
    );
    let c = candidate(&resume, "wt-corrupt");
    assert_eq!(
        c["verdict"], "undetermined",
        "an unreadable run state cannot prove that nothing is working here, so \
         the worktree can be neither resumed nor written off; candidate: {c}"
    );
    assert_eq!(
        resumable_count(&resume),
        0,
        "nothing may be offered while condukt's run state is not fully \
         readable: {resume}"
    );
}

/// (b) A DANGLING claim — run state records a worktree that is not on disk at
/// all — is `undetermined`.
///
/// The two sources disagree: condukt believes work is happening in a directory
/// that does not exist. That is not "nothing to resume" (the recorded path may
/// have been moved, or the state may be the stale half), and it is emphatically
/// not something to discard. `reconcile` already surfaces exactly this as
/// `dangling_claims`; the resume path must carry it through as a verdict the
/// operator can see rather than dropping it.
#[test]
fn dangling_claim_is_undetermined_not_resumable() {
    let f = Fixture::new("dangling");
    // Deliberately never created on disk.
    let ghost = f.wt_base.join("wt-vanished");
    f.write_run_state(
        "some-other-checkout-1234",
        "run-dangling",
        &run_state_json("run-dangling", "t1", "running", &ghost, now_secs()),
    );
    assert!(
        !ghost.exists(),
        "fixture precondition: the recorded worktree must not exist"
    );

    // Fixture precondition, read from the report the new path consumes.
    let recon = f.reconcile_json();
    let dangling = recon["dangling_claims"]
        .as_array()
        .expect("report has dangling_claims");
    assert_eq!(
        dangling.len(),
        1,
        "fixture precondition: reconcile must see the disagreement: {recon}"
    );

    let resume = f.resume_json();
    let c = candidate(&resume, "wt-vanished");
    assert_eq!(
        c["verdict"], "undetermined",
        "condukt's state and the disk disagree about this path — it may be \
         neither resumed nor written off; candidate: {c}"
    );
    assert_eq!(
        resumable_count(&resume),
        0,
        "a dangling claim is not an offer: {resume}"
    );
    // And it must not be reported as discardable either: this command has no
    // authority to conclude anything is disposable.
    assert!(
        !c.as_object()
            .map(|o| o.contains_key("removable") || o.contains_key("discardable"))
            .unwrap_or(false),
        "the resume path must not attach a disposal verdict to a dangling \
         claim; candidate: {c}"
    );
}

// ── 5. Print-only: it mutates nothing ──────────────────────────────────────

/// `resume-candidates` is a read of two sources and a classification. It must
/// not resume anything, must not delete anything, and must not write state.
///
/// Concretely, after two invocations: the worktree still exists with its
/// content (tracked modification AND untracked file) byte-identical, the
/// run-state file's bytes are unchanged, no ref was created or deleted (in
/// particular no `refs/preserved/...` — preservation is `reconcile --preserve`'s
/// job and writes into the repo), and git's worktree registry is unchanged.
#[test]
fn resume_candidates_mutates_nothing() {
    let f = Fixture::new("read-only");
    let wt = f.add_worktree("wt-untouched", "feat/untouched");
    std::fs::write(wt.join("seed.txt"), "seed\nmodified-by-a-dead-session\n").unwrap();
    std::fs::write(wt.join("rescue-me.rs"), "fn irreplaceable() {}\n").unwrap();

    let state_path = f.write_run_state(
        "some-other-checkout-beef",
        "run-ro",
        &run_state_json("run-ro", "t1", "failed", &wt, now_secs()),
    );

    let state_before = std::fs::read(&state_path).unwrap();
    let refs_before = run_git(
        &f.repo,
        &["for-each-ref", "--format=%(refname) %(objectname)"],
    );
    let registry_before = run_git(&f.repo, &["worktree", "list", "--porcelain"]);
    let head_before = run_git(&wt, &["rev-parse", "HEAD"]);
    let status_before = run_git(&wt, &["status", "--porcelain"]);

    let _ = f.resume_json();
    let _ = f.resume_json();

    assert!(
        wt.join("rescue-me.rs").exists(),
        "resume-candidates deleted a worktree's untracked work"
    );
    assert_eq!(
        std::fs::read_to_string(wt.join("rescue-me.rs")).unwrap(),
        "fn irreplaceable() {}\n"
    );
    assert_eq!(
        std::fs::read_to_string(wt.join("seed.txt")).unwrap(),
        "seed\nmodified-by-a-dead-session\n"
    );
    assert_eq!(
        std::fs::read(&state_path).unwrap(),
        state_before,
        "resume-candidates rewrote condukt's run state — a print-only \
         discovery command must not decide anything on the operator's behalf"
    );
    assert_eq!(
        run_git(
            &f.repo,
            &["for-each-ref", "--format=%(refname) %(objectname)"]
        ),
        refs_before,
        "resume-candidates created or deleted a ref"
    );
    assert!(
        !refs_before.contains("refs/preserved/")
            && !run_git(&f.repo, &["for-each-ref", "--format=%(refname)"])
                .contains("refs/preserved/"),
        "resume-candidates must not preserve: that is `reconcile --preserve`, \
         an explicit, opt-in write"
    );
    assert_eq!(
        run_git(&f.repo, &["worktree", "list", "--porcelain"]),
        registry_before,
        "resume-candidates changed git's worktree registry"
    );
    assert_eq!(run_git(&wt, &["rev-parse", "HEAD"]), head_before);
    assert_eq!(
        run_git(&wt, &["status", "--porcelain"]),
        status_before,
        "resume-candidates touched the working tree or the index"
    );
}

// ── 6. It hands off to `state resume-context`, it does not reimplement it ──

/// The offer is exactly the `state resume-context` invocation for that run —
/// and that invocation really runs.
///
/// The point is the division of labour: discovery answers "WHICH run", the
/// existing run-scoped command answers "what is left IN that run". A resume
/// path that re-derived the per-task breakdown itself would be a second
/// implementation of the same thing, free to drift. Nothing about
/// `resume-context`'s own output is asserted here.
#[test]
fn resume_command_hands_off_to_state_resume_context() {
    let f = Fixture::new("handoff");
    let wt = f.add_worktree("wt-handoff", "feat/handoff");
    let rid = f.init_run(r#"[{"id":"t1","title":"x","touched_files":["a.rs"],"deps":[],"class":"serial","done_criteria":"d"}]"#);
    f.set_task(&rid, "t1", "failed", &wt, "feat/handoff");

    // Establish death before reading the offer: an unclaimed worktree is
    // undetermined on a first probe, and undetermined is not an offer.
    f.settle();
    let resume = f.resume_json();
    let c = candidate(&resume, "wt-handoff");
    let cmd = c["resume_command"]
        .as_str()
        .unwrap_or_else(|| panic!("a resumable candidate carries a resume_command: {c}"));
    assert_eq!(
        cmd,
        format!("condukt state resume-context --run {rid}"),
        "the offer must BE the existing run-scoped command, verbatim; \
         candidate: {c}"
    );

    // The handoff must be usable, not merely well-formed: the run has to be
    // loadable by the command the candidate names.
    let words: Vec<&str> = cmd.split_whitespace().skip(1).collect();
    let out = f.condukt(&words);
    assert!(
        out.status.success(),
        "the emitted resume_command must actually run (exit {:?}):\nstdout:\n{}\nstderr:\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "the emitted resume_command must produce the resume context: {e}\n{}",
            String::from_utf8_lossy(&out.stdout)
        )
    });

    // Sanity that the run really is the one on disk (guards against a
    // resume_command naming a run id that never existed).
    assert!(f.run_state_path(&rid).exists());
}
