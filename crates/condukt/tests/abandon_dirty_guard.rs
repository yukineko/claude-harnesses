// このファイルは丸ごと integration test なので unwrap/expect を許可する
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! End-to-end coverage for the UNCOMMITTED-WORK half of the `state abandon
//! --all-stuck` re-dispatch predicate (backlog `5daf0b60` / `0dc6546f`).
//!
//! The progress half already landed (`356bd51d`): TTL-staleness alone no longer
//! authorises a reset, a confirmed `Known(Stalled)` is additionally required.
//! But both signals `stuck_task_ids` samples — the task's own worktree HEAD and
//! its `updated_at` — are **commit-shaped**: neither of them moves for a worker
//! that is editing files in its worktree without having committed yet. Such a
//! worker hardens to `Known(Stalled)` once the window elapses and IS abandoned:
//! reset to `Pending` with its `worktree`/`branch` cleared, after which the
//! skill re-dispatches a SECOND worker — the shared-index collision CLAUDE.md §8
//! names as the one conflict git cannot resolve, and this time the first
//! worker's uncommitted work is what gets orphaned. `state.rs` names this
//! residual in `stuck_task_ids`'s own doc comment; these tests are the thing
//! that was missing there ("no test pins it").
//!
//! # Why an e2e and not (only) a unit test
//!
//! Same reasoning as `abandon_progress_gate_e2e.rs`: the unit tests reach the
//! selector through a test-local adapter, and an adapter is a place a gate can
//! be installed without any production caller ever reaching it. These tests
//! spawn the real binary and drive the real subcommand, so they can only pass if
//! the guard is genuinely on the path `main.rs :: StateAction::Abandon` takes.
//!
//! # How `now` is driven
//!
//! The progress engine is multi-sample: one observation can never mean "frozen".
//! These tests set `HARNESS_PROGRESS_WINDOW_SECS=0` and invoke the subcommand
//! TWICE rather than sleeping — the second invocation is the one asserted. That
//! second call is also the only one in which the guard under test can fire at
//! all: on the first call the task is still `Undetermined` and is dropped by the
//! progress filter upstream of the guard.

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
        base.push(format!("condukt-abandon-dirty-{pid}-{tag}"));
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

    /// Rewrite task `task`'s `updated_at` to `ts`. There is no CLI flag for
    /// this (every write stamps "now") and the whole point of the fixture is a
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

/// `git` WITHOUT the success assertion — for the fixture preconditions that are
/// about a git invocation FAILING.
fn git_try(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("spawn git")
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

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
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

/// The two `--all-stuck` invocations the multi-sample engine needs, returning
/// the SECOND one's output (the one whose verdict is `Known(Stalled)` and thus
/// the only one in which the guard under test can fire).
fn abandon_all_stuck_twice(fx: &Fixture, rid: &str) -> std::process::Output {
    let a1 = fx.condukt(&["state", "abandon", "--run", rid, "--all-stuck"]);
    assert!(a1.status.success(), "abandon #1 failed: {a1:?}");
    let a2 = fx.condukt(&["state", "abandon", "--run", rid, "--all-stuck"]);
    assert!(a2.status.success(), "abandon #2 failed: {a2:?}");
    a2
}

fn stderr_of(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// (i) THE HEADLINE. A TTL-stale task whose worktree holds UNCOMMITTED work must
/// not be bulk-abandoned.
///
/// Both signals the progress gate reads are commit-shaped, so a worker that has
/// been editing without committing for longer than the window reads
/// `Known(Stalled)` while being demonstrably alive — there is work in its
/// worktree that no commit accounts for. Abandoning it clears the task's
/// worktree reference and re-dispatches a second worker; the direction of a
/// wrong call here is IRREVERSIBLE (detachment), so it must resolve to KEEP.
#[test]
fn all_stuck_does_not_abandon_a_task_whose_worktree_has_uncommitted_work() {
    let fx = Fixture::new("dirty");
    let (rid, wt) = running_task_past_ttl(&fx);

    // The live worker's unfinished edit: present on disk, accounted for by no
    // commit, so it moves neither of the gate's two signals.
    std::fs::write(wt.join("in-progress.rs"), "fn half_written() {\n").unwrap();
    let dirty_probe = git_try(&wt, &["status", "--porcelain"]);
    assert!(
        dirty_probe.status.success()
            && !String::from_utf8_lossy(&dirty_probe.stdout)
                .trim()
                .is_empty(),
        "fixture precondition: the worktree must be readable AND dirty; \
         `git status --porcelain` gave {dirty_probe:?}"
    );
    let head_before = head_of(&wt);

    let a2 = abandon_all_stuck_twice(&fx, &rid);

    assert_eq!(
        head_before,
        head_of(&wt),
        "fixture precondition: HEAD must stay frozen, so the progress verdict is \
         Known(Stalled) and the ONLY thing that can keep this task is the \
         uncommitted-work guard under test"
    );

    let t = fx.task(&rid, "t1");
    assert_eq!(
        t["status"], "running",
        "a worktree holding uncommitted work belongs to a worker that has not \
         finished; --all-stuck reset it to {} and cleared its worktree, which \
         re-dispatches a SECOND worker into it and orphans that work \
         (backlog 5daf0b60 / 0dc6546f)",
        t["status"]
    );
    assert!(
        t.get("worktree").is_some_and(|w| w.is_string()),
        "the kept task's worktree reference must survive; task is now {t}"
    );

    // CLAUDE.md §3: a silently smaller result set makes "checked" and "could
    // not check" indistinguishable downstream. The human must be told which
    // task was held back and why.
    let err = stderr_of(&a2);
    assert!(
        err.contains("t1") && err.contains("uncommitted"),
        "the skipped task and the reason must be reported on stderr; stderr was:\n{err}"
    );
}

/// (ii) FAIL-CLOSED ON CANNOT-DETERMINE (CLAUDE.md §3). A TTL-stale task whose
/// DIRTINESS cannot be read must not be bulk-abandoned either.
///
/// The fixture is a corrupt `.git/index` — precisely what a worker killed
/// mid-`git add` leaves behind. It is the case that separates this guard from
/// the progress gate that already exists: `git rev-parse HEAD` still SUCCEEDS
/// (so the progress verdict really is `Known(Stalled)` and the task really is
/// selected upstream), while `git status --porcelain` FAILS. The test asserts
/// both halves of that precondition, so it cannot pass for the already-covered
/// "worktree HEAD unreadable" reason.
#[test]
fn all_stuck_does_not_abandon_a_task_whose_dirtiness_cannot_be_determined() {
    let fx = Fixture::new("undeterminable");
    let (rid, wt) = running_task_past_ttl(&fx);

    std::fs::write(wt.join(".git").join("index"), b"NOT-AN-INDEX").unwrap();

    let head_probe = git_try(&wt, &["rev-parse", "HEAD"]);
    assert!(
        head_probe.status.success(),
        "fixture precondition: HEAD must stay READABLE (otherwise this test \
         would pass for the already-covered unreadable-HEAD reason); \
         `git rev-parse HEAD` gave {head_probe:?}"
    );
    let dirty_probe = git_try(&wt, &["status", "--porcelain"]);
    assert!(
        !dirty_probe.status.success(),
        "fixture precondition: `git status --porcelain` must FAIL, which is what \
         makes dirtiness cannot-determine; it gave {dirty_probe:?}"
    );

    let a2 = abandon_all_stuck_twice(&fx, &rid);

    let t = fx.task(&rid, "t1");
    assert_eq!(
        t["status"], "running",
        "'I cannot tell whether this worktree holds uncommitted work' is never \
         'it holds none'; --all-stuck resolved it to {} and re-dispatched on the \
         strength of a FAILED read (CLAUDE.md §3)",
        t["status"]
    );
    assert!(
        t.get("worktree").is_some_and(|w| w.is_string()),
        "the kept task's worktree reference must survive; task is now {t}"
    );

    let err = stderr_of(&a2);
    assert!(
        err.contains("t1") && err.contains("cannot determine"),
        "the skipped task and the cannot-determine reason must be reported on \
         stderr, not folded into a silently smaller result set; stderr was:\n{err}"
    );
}

/// (iii) **ANTI-VACUITY CONTROL — not a RED probe.** Green before and after; it
/// is what stops the guard being "implemented" as a blanket never-abandon.
///
/// A TTL-stale task whose worktree is CLEAN and whose HEAD stayed frozen across
/// the window is confirmed stalled with nothing to orphan, and `--all-stuck`
/// MUST still abandon it — that is the only path by which a genuinely abandoned
/// worktree is bulk-recovered.
#[test]
fn all_stuck_still_abandons_a_stale_task_whose_worktree_is_clean_control() {
    let fx = Fixture::new("clean-control");
    let (rid, wt) = running_task_past_ttl(&fx);
    let head_before = head_of(&wt);
    assert_eq!(
        git(&wt, &["status", "--porcelain"]),
        "",
        "fixture precondition: the control's worktree must be CLEAN"
    );

    abandon_all_stuck_twice(&fx, &rid);

    assert_eq!(
        head_before,
        head_of(&wt),
        "fixture precondition: nothing may commit in the worktree during this test"
    );

    let t = fx.task(&rid, "t1");
    assert_eq!(
        t["status"], "pending",
        "a stale task with a frozen HEAD and a clean worktree is confirmed dead \
         and MUST remain bulk-abandonable — a guard that answers 'nothing is \
         ever stuck' is not a fix; task is now {t}"
    );
    assert!(
        t.get("worktree").is_none_or(|w| w.is_null()),
        "an abandoned task's worktree must be cleared for re-dispatch; task is now {t}"
    );
}

/// (iv) The EXPLICIT override stays UNGATED. `state abandon --task <id>` is a
/// human naming one task, and it must still abandon a task the bulk guard
/// refuses — including one whose worktree is dirty.
///
/// Green before and after; it is here so that a fix which gates the SHARED code
/// path, and so silently disarms the human's escape hatch, fails.
#[test]
fn explicit_task_abandon_stays_ungated_for_a_dirty_worktree() {
    let fx = Fixture::new("explicit-dirty");
    let (rid, wt) = running_task_past_ttl(&fx);
    std::fs::write(wt.join("in-progress.rs"), "fn half_written() {\n").unwrap();
    assert_ne!(
        git(&wt, &["status", "--porcelain"]),
        "",
        "fixture precondition: the worktree must be dirty"
    );

    let explicit = fx.condukt(&["state", "abandon", "--run", &rid, "--task", "t1"]);
    assert!(
        explicit.status.success(),
        "explicit `state abandon --task t1` must succeed: {explicit:?}"
    );

    let t = fx.task(&rid, "t1");
    assert_eq!(
        t["status"], "pending",
        "`--task <id>` is the human override and is deliberately ungated; it must \
         abandon the task even when its worktree holds uncommitted work, because \
         that is how a genuinely dead worker is reclaimed. task is now {t}"
    );
}
