//! Black-box coverage for the tri-valued stuck scan (backlog `b637936f`).
//!
//! `state.rs::stuck_task_ids` filters Running tasks with
//! `.filter(|t| t.updated_at.map(|ts| ts < threshold).unwrap_or(false))`.
//! A Running task whose `updated_at` is `None` therefore falls out of the
//! filter (the `.unwrap_or(false)` arm) and is silently treated as "not
//! stuck" — CLAUDE.md §3 forbids resolving "cannot determine" to "clean",
//! and §1 names exactly this kind of silence (a bare `nothing to abandon`
//! that reads as "all fine") a fail-open.
//!
//! `condukt` ships no `lib.rs` (bin-only), so every test here drives the
//! real binary end-to-end rather than calling `state::stuck_task_ids` (or
//! the `scan_stuck`/`StuckScan` API the fix is expected to introduce)
//! in-process. The fixture shape is lifted from
//! `abandon_progress_gate_e2e.rs`.
//!
//! These tests are written against the CONTRACT the fix is expected to
//! satisfy (see backlog `b637936f`), not against today's code. Properties
//! R1/R2/R5 are expected to be RED today; R3/R4 are anti-vacuity controls
//! that must stay GREEN; R6 is a pure empirical probe with no fixed
//! expectation baked in ahead of running it.

use std::path::{Path, PathBuf};
use std::process::Command;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_condukt")
}

/// A throwaway git repo + an isolated `$HOME`, so run state and the progress
/// snapshot store land under `<home>/.condukt` and never touch the developer's
/// real store.
struct Fixture {
    repo: PathBuf,
    home: PathBuf,
    state_dir: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let pid = std::process::id();
        let mut base = std::env::temp_dir();
        base.push(format!("condukt-stuck-tristate-{pid}-{tag}"));
        let _ = std::fs::remove_dir_all(&base);
        let repo = base.join("repo");
        let home = base.join("home");
        let state_dir = home.join(".condukt").join("state");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&state_dir).unwrap();
        init_git_repo(&repo);
        Self {
            repo,
            home,
            state_dir,
        }
    }

    fn condukt(&self, args: &[&str]) -> std::process::Output {
        Command::new(bin())
            .args(args)
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            // Multi-sample without sleeping: a frozen fingerprint may harden on
            // the very next observation. It still takes TWO observations — the
            // engine never calls a single sample "frozen".
            .env("HARNESS_PROGRESS_WINDOW_SECS", "0")
            .output()
            .expect("spawn condukt")
    }

    fn run_state_path(&self, rid: &str) -> PathBuf {
        let mut found = Vec::new();
        for entry in std::fs::read_dir(&self.state_dir).expect("state dir readable") {
            let p = entry.expect("dir entry").path().join(format!("{rid}.json"));
            if p.exists() {
                found.push(p);
            }
        }
        assert_eq!(
            found.len(),
            1,
            "expected exactly one run-state file for {rid}, found {found:?}"
        );
        found.remove(0)
    }

    fn load_run_state(&self, rid: &str) -> serde_json::Value {
        let txt = std::fs::read_to_string(self.run_state_path(rid)).expect("read run state");
        serde_json::from_str(&txt).expect("run state is valid json")
    }

    fn write_run_state(&self, rid: &str, v: &serde_json::Value) {
        let path = self.run_state_path(rid);
        std::fs::write(&path, serde_json::to_string_pretty(v).unwrap()).expect("write run state");
    }

    /// Rewrite task `task`'s `updated_at` to `ts`. There is no CLI flag for
    /// this (every write stamps "now"), and the whole point of the fixture is a
    /// task whose last durable transition is far in the past.
    fn set_updated_at(&self, rid: &str, task: &str, ts: i64) {
        let mut v = self.load_run_state(rid);
        let tasks = v["tasks"].as_array_mut().expect("tasks array");
        let t = tasks
            .iter_mut()
            .find(|t| t["id"] == task)
            .unwrap_or_else(|| panic!("no task {task} in run state"));
        t["updated_at"] = serde_json::json!(ts);
        self.write_run_state(rid, &v);
    }

    /// Blow away `task`'s `updated_at` entirely (JSON `null`) — the exact
    /// "cannot determine" shape the defect maps to "healthy".
    fn clear_updated_at(&self, rid: &str, task: &str) {
        let mut v = self.load_run_state(rid);
        let tasks = v["tasks"].as_array_mut().expect("tasks array");
        let t = tasks
            .iter_mut()
            .find(|t| t["id"] == task)
            .unwrap_or_else(|| panic!("no task {task} in run state"));
        t["updated_at"] = serde_json::Value::Null;
        self.write_run_state(rid, &v);
    }

    fn task(&self, rid: &str, task: &str) -> serde_json::Value {
        let v = self.load_run_state(rid);
        v["tasks"]
            .as_array()
            .expect("tasks array")
            .iter()
            .find(|t| t["id"] == task)
            .unwrap_or_else(|| panic!("no task {task} in run state"))
            .clone()
    }
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {args:?} in {} failed: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn init_git_repo(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "t@t.t"]);
    git(dir, &["config", "user.name", "t"]);
    std::fs::write(dir.join("base.txt"), "base\n").unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "-q", "-m", "init"]);
}

fn write_decomp(fx: &Fixture, task_id: &str) -> PathBuf {
    let p = fx.repo.join("decomp.json");
    std::fs::write(
        &p,
        format!(
            r#"{{"goal":"g","tasks":[{{"id":"{task_id}","title":"x","touched_files":["a.rs"],"deps":[],"class":"serial","done_criteria":"d"}}]}}"#
        ),
    )
    .unwrap();
    p
}

fn write_decomp_pending_kind(fx: &Fixture, task_id: &str) -> PathBuf {
    // `state init` always creates Pending tasks (see `StateAction::Init` in
    // main.rs); the decomposition JSON does not carry a "kind" that changes
    // that, so this is identical to `write_decomp` — kept as a separate name
    // only to make R4's intent ("a non-Running candidate") explicit at the
    // call site.
    write_decomp(fx, task_id)
}

fn run_id_from(out: &std::process::Output) -> String {
    let s = String::from_utf8_lossy(&out.stdout);
    s.lines()
        .chain(String::from_utf8_lossy(&out.stderr).lines())
        .rev()
        .map(str::trim)
        .find(|l| l.starts_with("run-"))
        .expect("a run- id in init output")
        .to_string()
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn stderr_of(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// Init a run and dispatch `t1` into a real git worktree as Running. Returns
/// `(run id, worktree path)`. Right after this, `updated_at` is `Some(now)`
/// (the `state set` handler stamps it unconditionally).
fn init_and_dispatch_running(fx: &Fixture, tag: &str) -> (String, PathBuf) {
    let decomp = write_decomp(fx, "t1");
    let init = fx.condukt(&["state", "init", "--file", decomp.to_str().unwrap()]);
    assert!(init.status.success(), "state init failed: {init:?}");
    let rid = run_id_from(&init);

    let wt = fx.home.join(format!("worker-worktree-{tag}"));
    init_git_repo(&wt);

    let set = fx.condukt(&[
        "state",
        "set",
        "--run",
        &rid,
        "--task",
        "t1",
        "--status",
        "running",
        "--worktree",
        wt.to_str().unwrap(),
        "--branch",
        "feat/t1",
    ]);
    assert!(set.status.success(), "state set failed: {set:?}");
    assert_eq!(
        fx.task(&rid, "t1")["status"],
        "running",
        "fixture precondition: t1 must be running before the abandon calls"
    );
    (rid, wt)
}

// ── R1 ──────────────────────────────────────────────────────────────────
//
// A Running task whose `updated_at` is absent (JSON `null`) is "cannot
// determine" — never "fine". `state abandon --all-stuck` must surface it
// (stderr names the task) and signal non-cleanliness via a distinct exit
// code (3), never a bare `nothing to abandon` / exit 0.
#[test]
fn r1_missing_updated_at_is_reported_and_exits_nonzero() {
    let fx = Fixture::new("r1");
    let (rid, _wt) = init_and_dispatch_running(&fx, "r1");
    fx.clear_updated_at(&rid, "t1");
    assert!(
        fx.task(&rid, "t1")["updated_at"].is_null(),
        "fixture precondition: t1.updated_at must be null before the abandon call"
    );

    let out = fx.condukt(&["state", "abandon", "--run", &rid, "--all-stuck"]);
    let err = stderr_of(&out);

    assert!(
        err.contains("t1"),
        "R1: a Running task with no `updated_at` is undetermined, not healthy; \
         `state abandon --all-stuck` must name it ('t1') on stderr instead of \
         staying silent about it. stderr was: {err:?}"
    );
    assert_eq!(
        out.status.code(),
        Some(3),
        "R1: at least one task is undetermined, so the scan is not clean; exit \
         code must be the distinct undetermined code (3), not {:?} (stdout={:?} \
         stderr={err:?})",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout)
    );
}

// ── R2 ──────────────────────────────────────────────────────────────────
//
// The wording itself must not read as "clean". A bare `nothing to abandon`
// is exactly the fail-open CLAUDE.md §1 calls out: silence a human/script
// reads as "no problem".
#[test]
fn r2_wording_conveys_undetermined_not_clean() {
    let fx = Fixture::new("r2");
    let (rid, _wt) = init_and_dispatch_running(&fx, "r2");
    fx.clear_updated_at(&rid, "t1");

    let out = fx.condukt(&["state", "abandon", "--run", &rid, "--all-stuck"]);
    let err = stderr_of(&out);
    let err_lower = err.to_lowercase();

    assert!(
        err_lower.contains("undetermined") || err_lower.contains("cannot determine"),
        "R2: stderr must convey that the task's status could not be determined \
         (\"undetermined\" / \"cannot determine\"), not imply it is fine. stderr \
         was: {err:?}"
    );
    assert_ne!(
        err.trim(),
        "nothing to abandon",
        "R2: stderr must not be the bare 'nothing to abandon' — that reads as \
         clean when at least one task's liveness is undetermined. stderr was: \
         {err:?}"
    );
}

// ── R3 (control) ────────────────────────────────────────────────────────
//
// A genuinely healthy task (Running, fresh `updated_at`) must stay clean:
// exit 0, nothing reported as undetermined. This is the control that stops
// the fix from reporting EVERY task as undetermined — it must pass both
// before and after the fix.
#[test]
fn r3_control_fresh_updated_at_is_clean() {
    let fx = Fixture::new("r3");
    let (rid, _wt) = init_and_dispatch_running(&fx, "r3");
    // `updated_at` was just stamped to `now` by `state set`; well within any
    // plausible stuck TTL. No further mutation.

    let out = fx.condukt(&["state", "abandon", "--run", &rid, "--all-stuck"]);
    let err = stderr_of(&out);

    assert_eq!(
        out.status.code(),
        Some(0),
        "R3 control: a Running task with a fresh `updated_at` is healthy and \
         must exit 0 (stdout={:?} stderr={err:?})",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        !err.to_lowercase().contains("undetermined"),
        "R3 control: a healthy task must not be listed as undetermined. stderr \
         was: {err:?}"
    );
}

// ── R4 (control) ────────────────────────────────────────────────────────
//
// A non-Running task (freshly initialized, Pending, `updated_at` absent by
// construction) is never a stuck/undetermined candidate. Missing timestamps
// on tasks that were never dispatched must not become noise.
#[test]
fn r4_control_non_running_missing_updated_at_is_clean() {
    let fx = Fixture::new("r4");
    let decomp = write_decomp_pending_kind(&fx, "t1");
    let init = fx.condukt(&["state", "init", "--file", decomp.to_str().unwrap()]);
    assert!(init.status.success(), "state init failed: {init:?}");
    let rid = run_id_from(&init);

    let t1 = fx.task(&rid, "t1");
    assert_eq!(
        t1["status"], "pending",
        "fixture precondition: a freshly initialized task is Pending"
    );
    assert!(
        t1["updated_at"].is_null(),
        "fixture precondition: a freshly initialized task has no updated_at"
    );

    let out = fx.condukt(&["state", "abandon", "--run", &rid, "--all-stuck"]);
    let err = stderr_of(&out);

    assert_eq!(
        out.status.code(),
        Some(0),
        "R4 control: a non-Running task must never be a candidate, regardless \
         of its updated_at; exit must be 0 (stdout={:?} stderr={err:?})",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        !err.to_lowercase().contains("undetermined"),
        "R4 control: a non-Running task's missing timestamp must not surface as \
         an undetermined finding. stderr was: {err:?}"
    );
}

// ── R5 ──────────────────────────────────────────────────────────────────
//
// A TTL-stale Running task whose recorded worktree no longer exists yields
// `Determination::Undetermined` from the progress engine (see
// `task_progress` / `git_head_signal` in state.rs — an unreadable HEAD is
// "cannot determine", never "frozen"). It must be reported as undetermined
// (exit 3), not silently dropped from every list.
#[test]
fn r5_ttl_stale_unreadable_worktree_is_reported_undetermined() {
    let fx = Fixture::new("r5");
    let (rid, wt) = init_and_dispatch_running(&fx, "r5");

    // Far past any plausible stuck TTL (default 1800s).
    let long_ago = now_secs() - 1_000_000;
    fx.set_updated_at(&rid, "t1", long_ago);
    // The worktree is recorded in run state but no longer readable as a repo.
    std::fs::remove_dir_all(&wt).expect("remove the recorded worktree");
    assert!(
        !wt.exists(),
        "fixture precondition: the worktree must be gone"
    );

    let out = fx.condukt(&["state", "abandon", "--run", &rid, "--all-stuck"]);
    let err = stderr_of(&out);
    let err_lower = err.to_lowercase();

    assert!(
        err.contains("t1"),
        "R5: a TTL-stale task whose worktree HEAD is unreadable is undetermined \
         (not stuck, not healthy) and must be named on stderr. stderr was: \
         {err:?}"
    );
    assert!(
        err_lower.contains("undetermined") || err_lower.contains("cannot determine"),
        "R5: stderr must convey the verdict is undetermined, not silently drop \
         the task. stderr was: {err:?}"
    );
    assert_eq!(
        out.status.code(),
        Some(3),
        "R5: at least one task is undetermined, so exit must be the distinct \
         undetermined code (3), not {:?} (stdout={:?} stderr={err:?})",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout)
    );

    // Sanity per the existing (already-correct) half of this contract: an
    // undetermined verdict must never be treated as abandonable.
    let t = fx.task(&rid, "t1");
    assert_eq!(
        t["status"], "running",
        "R5: an undetermined verdict must never authorise abandon; task is now \
         {t}"
    );
}

// ── R6 ──────────────────────────────────────────────────────────────────
//
// Empirical probe (no fixed expectation): does the abandon handler's
// `t.updated_at = None` reset survive into the RE-DISPATCHED task, i.e. does
// a task abandoned-then-re-dispatched via the normal `state set --status
// running` CLI end up Running with `updated_at` absent (the "abandon
// manufactures the invisible state" half of the ticket)? This test pins
// whatever is actually observed — see the test body / report for the
// reading.
#[test]
fn r6_abandon_then_redispatch_updated_at_is_observed_not_assumed() {
    let fx = Fixture::new("r6");
    let (rid, wt) = init_and_dispatch_running(&fx, "r6");

    // Drive the task past the TTL with a FROZEN worktree so the progress
    // engine converges to `Known(Stalled)` and the bulk gate actually
    // abandons it (mirrors `all_stuck_still_abandons_a_task_whose_worktree_is_frozen_control`
    // in abandon_progress_gate_e2e.rs). Two calls: the first only anchors the
    // fingerprint (one observation is never "frozen").
    let long_ago = now_secs() - 1_000_000;
    fx.set_updated_at(&rid, "t1", long_ago);
    let a1 = fx.condukt(&["state", "abandon", "--run", &rid, "--all-stuck"]);
    assert!(a1.status.success(), "abandon #1 failed: {a1:?}");
    let a2 = fx.condukt(&["state", "abandon", "--run", &rid, "--all-stuck"]);
    assert!(a2.status.success(), "abandon #2 failed: {a2:?}");

    let after_abandon = fx.task(&rid, "t1");
    assert_eq!(
        after_abandon["status"], "pending",
        "fixture precondition: t1 must have been abandoned to Pending by the \
         second call; task is now {after_abandon}"
    );
    assert!(
        after_abandon["updated_at"].is_null(),
        "fixture precondition (matches the ticket's stated abandon behaviour): \
         abandon must clear updated_at to null; task is now {after_abandon}"
    );

    // Re-dispatch through the REAL CLI path a worker/skill would use.
    let redispatch = fx.condukt(&[
        "state",
        "set",
        "--run",
        &rid,
        "--task",
        "t1",
        "--status",
        "running",
        "--worktree",
        wt.to_str().unwrap(),
        "--branch",
        "feat/t1-retry",
    ]);
    assert!(
        redispatch.status.success(),
        "re-dispatch `state set --status running` failed: {redispatch:?}"
    );

    let after_redispatch = fx.task(&rid, "t1");
    assert_eq!(
        after_redispatch["status"], "running",
        "fixture precondition: re-dispatch must land the task back in Running; \
         task is now {after_redispatch}"
    );

    eprintln!(
        "R6 RAW OBSERVATION: after_redispatch.updated_at = {}",
        after_redispatch["updated_at"]
    );

    // THE OBSERVATION: what is `updated_at` after a normal re-dispatch that
    // followed an abandon? Pinned to what was actually measured when this
    // test was written (see the accompanying report for the raw command
    // output) — NOT to the ticket's a-priori claim.
    assert!(
        after_redispatch["updated_at"].is_i64() || after_redispatch["updated_at"].is_u64(),
        "R6 OBSERVED: re-dispatching via `state set --status running` after an \
         abandon sets `updated_at` to a fresh timestamp (the Set handler stamps \
         it unconditionally at main.rs `t.updated_at = Some(state::now_secs())`), \
         so the Running+missing-updated_at invisible state does NOT survive a \
         normal re-dispatch in this build. If this assertion fails, the machine \
         disagrees with that reading and the value actually observed was: {}",
        after_redispatch["updated_at"]
    );
}
