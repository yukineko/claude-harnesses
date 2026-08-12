//! End-to-end coverage for the re-dispatch predicate behind
//! `condukt state abandon` (backlog `356bd51d`).
//!
//! `state abandon --all-stuck` resets every task the predicate returns to
//! `Pending` and clears its `worktree`/`branch`, after which the skill
//! re-dispatches it. When that predicate is a PURE TTL check, a worker that is
//! merely QUIET — thinking, or editing files without committing, for longer
//! than `stuck_ttl_secs` — is classified dead while fully alive, and a SECOND
//! worker is put into the first one's worktree. That is the shared-index
//! collision CLAUDE.md §8 names as the one conflict git cannot resolve.
//!
//! # Why this is an e2e and not (only) a unit test
//!
//! The unit tests in `state.rs` call the selector directly, so they must be
//! routed through a test-local adapter that absorbs the selector's upcoming
//! signature change — and an adapter is a place a gate could be installed
//! without any production caller ever reaching it. These tests spawn the real
//! binary and drive the real subcommand, so they are signature-independent and
//! can only pass if the gate is genuinely in the path
//! `main.rs :: StateAction::Abandon` takes.
//!
//! # How `now` is driven
//!
//! The progress engine is multi-sample: one observation can never mean "frozen"
//! (the first is `Undetermined` by construction), and a frozen fingerprint only
//! hardens to `Stalled` once the window has elapsed. These tests set
//! `HARNESS_PROGRESS_WINDOW_SECS=0` and invoke the subcommand TWICE rather than
//! sleeping — the second invocation is the one whose verdict is asserted.

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
        base.push(format!("condukt-abandon-gate-{pid}-{tag}"));
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

    /// The on-disk run state for `rid`, found under the (project-keyed)
    /// namespace directory the binary chose.
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

    /// Rewrite task `task`'s `updated_at` to `ts`. There is no CLI flag for
    /// this (every write stamps "now"), and the whole point of the fixture is a
    /// task whose last durable transition is far in the past.
    fn set_updated_at(&self, rid: &str, task: &str, ts: i64) {
        let path = self.run_state_path(rid);
        let mut v = self.load_run_state(rid);
        let tasks = v["tasks"].as_array_mut().expect("tasks array");
        let t = tasks
            .iter_mut()
            .find(|t| t["id"] == task)
            .unwrap_or_else(|| panic!("no task {task} in run state"));
        t["updated_at"] = serde_json::json!(ts);
        std::fs::write(&path, serde_json::to_string_pretty(&v).unwrap()).expect("write run state");
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

fn head_of(repo: &Path) -> String {
    git(repo, &["rev-parse", "HEAD"])
}

/// One empty commit, asserting HEAD actually moved — a git that silently did
/// nothing must fail loudly here, not read downstream as "nothing advanced".
fn advance_head(repo: &Path) {
    let before = head_of(repo);
    git(repo, &["commit", "-q", "--allow-empty", "-m", "advance"]);
    assert_ne!(
        before,
        head_of(repo),
        "fixture precondition: an empty commit must move HEAD in {}",
        repo.display()
    );
}

fn write_decomp(fx: &Fixture) -> PathBuf {
    let p = fx.repo.join("decomp.json");
    std::fs::write(
        &p,
        r#"{"goal":"g","tasks":[{"id":"t1","title":"x","touched_files":["a.rs"],"deps":[],"class":"serial","done_criteria":"d"}]}"#,
    )
    .unwrap();
    p
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

/// Init a run, put `t1` into `running` pointing at a real git worktree, and
/// backdate its `updated_at` far past the stuck TTL. Returns `(run id,
/// worktree path)`.
fn running_task_past_ttl(fx: &Fixture) -> (String, PathBuf) {
    let decomp = write_decomp(fx);
    let init = fx.condukt(&["state", "init", "--file", decomp.to_str().unwrap()]);
    assert!(init.status.success(), "state init failed: {init:?}");
    let rid = run_id_from(&init);

    let wt = fx.home.join("worker-worktree");
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

    // Far past any plausible stuck TTL (default 1800s): by the TTL alone this
    // worker is as "dead" as the predicate can say.
    let long_ago = now_secs() - 1_000_000;
    fx.set_updated_at(&rid, "t1", long_ago);
    assert_eq!(
        fx.task(&rid, "t1")["status"],
        "running",
        "fixture precondition: t1 must be running before the abandon calls"
    );
    (rid, wt)
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// (356bd51d — the headline, at the real consumer) `state abandon --all-stuck`
/// must NOT abandon a task past the TTL whose own worktree HEAD is ADVANCING.
///
/// Abandoning it resets it to `Pending` and clears its worktree/branch, which is
/// precisely the instruction to re-dispatch a SECOND worker into the worktree a
/// live one is committing in.
#[test]
fn all_stuck_does_not_abandon_a_task_whose_worktree_is_advancing() {
    let fx = Fixture::new("progressing");
    let (rid, wt) = running_task_past_ttl(&fx);

    // Call 1 anchors the progress fingerprint (one observation is never "frozen").
    let a1 = fx.condukt(&["state", "abandon", "--run", &rid, "--all-stuck"]);
    assert!(a1.status.success(), "abandon #1 failed: {a1:?}");
    // The worker does durable, observable work in its OWN worktree.
    advance_head(&wt);
    let a2 = fx.condukt(&["state", "abandon", "--run", &rid, "--all-stuck"]);
    assert!(a2.status.success(), "abandon #2 failed: {a2:?}");

    let t = fx.task(&rid, "t1");
    assert_eq!(
        t["status"], "running",
        "a task whose own worktree HEAD advanced is a LIVE worker; --all-stuck \
         reset it to {} and cleared its worktree, which re-dispatches a SECOND \
         worker into that worktree (backlog 356bd51d)",
        t["status"]
    );
    assert!(
        t.get("worktree").is_some_and(|w| w.is_string()),
        "the live worker's worktree must not be cleared; task is now {t}"
    );
}

/// Fail-closed (CLAUDE.md §3), at the real consumer: `state abandon
/// --all-stuck` must NOT abandon a task past the TTL whose recorded worktree
/// cannot be READ. "Cannot determine" is not "dead"; a failed read must never
/// acquire re-dispatch authority. The human override (`--task <id>`) stays
/// available for a genuinely dead worker whose worktree was deleted — see
/// `explicit_task_abandon_stays_ungated_for_a_progressing_task`.
#[test]
fn all_stuck_does_not_abandon_a_task_whose_worktree_is_unreadable() {
    let fx = Fixture::new("unreadable");
    let (rid, wt) = running_task_past_ttl(&fx);
    // The worktree is recorded in run state but no longer readable as a repo.
    std::fs::remove_dir_all(&wt).expect("remove the recorded worktree");
    assert!(
        !wt.exists(),
        "fixture precondition: the worktree must be gone"
    );

    let a1 = fx.condukt(&["state", "abandon", "--run", &rid, "--all-stuck"]);
    assert!(a1.status.success(), "abandon #1 failed: {a1:?}");
    let a2 = fx.condukt(&["state", "abandon", "--run", &rid, "--all-stuck"]);
    assert!(a2.status.success(), "abandon #2 failed: {a2:?}");

    let t = fx.task(&rid, "t1");
    assert_eq!(
        t["status"], "running",
        "an unreadable worktree HEAD is 'cannot determine', never 'frozen'; \
         --all-stuck resolved it to {} and re-dispatched on the strength of a \
         failed read",
        t["status"]
    );
}

/// **ANTI-VACUITY CONTROL — not a RED probe.** It passes both before and after
/// the gate, and it is what stops the gate being "fixed" by never abandoning
/// anything.
///
/// A task past the TTL whose own worktree HEAD is FROZEN across the window is
/// confirmed stalled, and `--all-stuck` MUST still abandon it: that is the only
/// path by which a genuinely abandoned worktree is bulk-recovered.
#[test]
fn all_stuck_still_abandons_a_task_whose_worktree_is_frozen_control() {
    let fx = Fixture::new("frozen-control");
    let (rid, wt) = running_task_past_ttl(&fx);
    let head_before = head_of(&wt);

    let a1 = fx.condukt(&["state", "abandon", "--run", &rid, "--all-stuck"]);
    assert!(a1.status.success(), "abandon #1 failed: {a1:?}");
    let a2 = fx.condukt(&["state", "abandon", "--run", &rid, "--all-stuck"]);
    assert!(a2.status.success(), "abandon #2 failed: {a2:?}");
    assert_eq!(
        head_before,
        head_of(&wt),
        "fixture precondition: nothing may commit in the worktree during this test"
    );

    let t = fx.task(&rid, "t1");
    assert_eq!(
        t["status"], "pending",
        "a task past the TTL whose worktree stayed frozen across the window is \
         confirmed stalled and MUST remain bulk-abandonable — a predicate that \
         answers 'nothing is ever stuck' is not a fix; task is now {t}"
    );
    assert!(
        t.get("worktree").is_none_or(|w| w.is_null()),
        "an abandoned task's worktree must be cleared for re-dispatch; task is now {t}"
    );
}

/// The EXPLICIT override stays UNGATED: `state abandon --task <id>` is a human
/// naming one task, and it must still abandon a task the bulk gate refuses.
///
/// This is why fail-closing `--all-stuck` does not strand anybody: the operator
/// keeps a way to reclaim a task whose progress cannot be determined (or which
/// is, by every machine signal, alive) — but it takes a human saying so, not a
/// predicate guessing. Green before and after the fix; it is here so a fix that
/// gates the SHARED code path, and so silently disarms the override too, fails.
#[test]
fn explicit_task_abandon_stays_ungated_for_a_progressing_task() {
    let fx = Fixture::new("explicit-override");
    let (rid, wt) = running_task_past_ttl(&fx);

    // The worker is demonstrably alive: it just committed in its own worktree,
    // so its progress reads Progressing (or, with no prior sample,
    // Undetermined) — either way a verdict the bulk gate must refuse to reap.
    // No `--all-stuck` warm-up here: this test is about the override alone, and
    // a bulk call would (today) abandon the task before the override runs and
    // make this pass/fail for a reason that is not the arm under test.
    advance_head(&wt);

    let explicit = fx.condukt(&["state", "abandon", "--run", &rid, "--task", "t1"]);
    assert!(
        explicit.status.success(),
        "explicit `state abandon --task t1` must succeed: {explicit:?}"
    );

    let t = fx.task(&rid, "t1");
    assert_eq!(
        t["status"], "pending",
        "`--task <id>` is the human override and is deliberately ungated; it \
         must abandon the task even when its progress says the worker is alive. \
         task is now {t}"
    );
}
