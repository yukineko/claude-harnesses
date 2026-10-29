//! backlog 4e560a64 — HEAD-read half.
//!
//! condukt reads a recorded task worktree's HEAD with
//! `harness_core::progress::git_head_signal(path)` (`git -C path rev-parse
//! HEAD`). When `path` has lost its own `.git` but sits inside ANOTHER git
//! repository, git walks up and answers with the ENCLOSING repo's HEAD — a
//! confident `Known` signal about the wrong tree.
//!
//! Observable surface: `condukt state probe --run RID --json`, which prints the
//! per-task `task-worktree-head` signal (`readable` + `detail`, i.e. the sha
//! that was read) assembled by `state::task_progress` — the SAME function the
//! reap-authoritative `state abandon --all-stuck` selector uses. It is the
//! most direct print of the head signal itself.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
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
        base.push(format!("condukt-head-4e560a64-{pid}-{tag}"));
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
        harness_core::git_env::scrub(&mut Command::new(bin()))
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

    fn task_status(&self, rid: &str, task: &str) -> String {
        let txt = std::fs::read_to_string(self.run_state_path(rid)).expect("read run state");
        let v: serde_json::Value = serde_json::from_str(&txt).expect("run state is valid json");
        v["tasks"]
            .as_array()
            .expect("tasks array")
            .iter()
            .find(|t| t["id"] == task)
            .unwrap_or_else(|| panic!("no task {task} in run state"))["status"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    }

    /// `state probe --run RID --json` → the `task-worktree-head` signal of
    /// task `t1` as `(readable, detail)`, plus the task verdict.
    fn probe_t1(&self, rid: &str) -> (bool, String, String) {
        let out = self.condukt(&["state", "probe", "--run", rid, "--json"]);
        assert!(out.status.success(), "state probe failed: {out:?}");
        let v: serde_json::Value =
            serde_json::from_slice(&out.stdout).expect("probe --json prints json");
        let task = v["tasks"]
            .as_array()
            .expect("tasks array")
            .iter()
            .find(|t| t["task_id"] == "t1")
            .unwrap_or_else(|| panic!("probe did not report t1: {v}"))
            .clone();
        let sig = task["signals"]
            .as_array()
            .expect("signals array")
            .iter()
            .find(|s| s["name"] == "task-worktree-head")
            .unwrap_or_else(|| panic!("no task-worktree-head signal: {task}"))
            .clone();
        (
            sig["readable"].as_bool().expect("readable bool"),
            sig["detail"].as_str().unwrap_or_default().to_string(),
            task["verdict"].as_str().unwrap_or_default().to_string(),
        )
    }
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = harness_core::git_env::scrub(&mut Command::new("git"))
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

/// Init a run and put `t1` into `running` pointing at a real git worktree under
/// `$HOME`. Returns `(run id, worktree path)`.
fn running_task(fx: &Fixture) -> (String, PathBuf) {
    let decomp = fx.repo.join("decomp.json");
    std::fs::write(
        &decomp,
        r#"{"goal":"g","tasks":[{"id":"t1","title":"x","touched_files":["a.rs"],"deps":[],"class":"serial","done_criteria":"d"}]}"#,
    )
    .unwrap();
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
    assert_eq!(
        fx.task_status(&rid, "t1"),
        "running",
        "fixture precondition: t1 must be running (probe only reports RUNNING tasks)"
    );
    (rid, wt)
}

/// Control (must pass on current code): when the recorded worktree DOES have
/// its own `.git`, the probe reaches the head-signal path and reports exactly
/// that worktree's HEAD as readable. This proves the fixture drives the code
/// path the RED test below observes.
#[test]
fn control_worktree_with_own_git_reports_its_own_head() {
    let fx = Fixture::new("control");
    let (rid, wt) = running_task(&fx);
    let own_head = git(&wt, &["rev-parse", "HEAD"]);

    let (readable, detail, _verdict) = fx.probe_t1(&rid);
    assert!(
        readable,
        "control: a worktree with its own .git must yield a readable HEAD; detail={detail:?}"
    );
    assert_eq!(
        detail, own_head,
        "control: the head signal must be the worktree's own HEAD"
    );
}

/// backlog 4e560a64 (HEAD-read half). The recorded worktree loses its own
/// `.git` and `$HOME` (which contains it) becomes a git repo. The worktree has
/// no HEAD of its own, so the `task-worktree-head` signal must be UNREADABLE
/// (cannot determine), never the enclosing repo's HEAD reported as this task's.
/// Consequence check: with that signal unreadable the fail-closed combiner
/// forces the verdict to `undetermined`, so repeated probes must never converge
/// to `stalled` on the strength of the parent's frozen HEAD.
#[test]
fn head_of_a_gitless_worktree_is_not_read_from_the_enclosing_repo() {
    let fx = Fixture::new("red");
    let (rid, wt) = running_task(&fx);

    std::fs::remove_dir_all(wt.join(".git")).unwrap();
    git(&fx.home, &["init", "-q", "-b", "main"]);
    git(&fx.home, &["config", "user.email", "t@t.t"]);
    git(&fx.home, &["config", "user.name", "t"]);
    git(&fx.home, &["add", "worker-worktree"]);
    git(&fx.home, &["commit", "-q", "-m", "parent"]);
    let parent_head = git(&fx.home, &["rev-parse", "HEAD"]);

    // Precondition: git really does walk up from the worktree to the parent —
    // otherwise the defect cannot be exercised and the test would pass vacuously.
    assert_eq!(
        git(&wt, &["rev-parse", "HEAD"]),
        parent_head,
        "precondition: `git -C <wt> rev-parse HEAD` resolves the enclosing repo"
    );
    assert!(
        !wt.join(".git").exists(),
        "precondition: worktree has no .git"
    );

    let (readable, detail, _) = fx.probe_t1(&rid);
    assert_ne!(
        detail,
        parent_head,
        "task-worktree-head reported the ENCLOSING repo's HEAD ({parent_head}) as the HEAD \
         of worktree {} which has no .git of its own",
        wt.display()
    );
    assert!(
        !readable,
        "a worktree with no .git of its own has no HEAD: task-worktree-head must be \
         unreadable (cannot determine); detail={detail:?}"
    );

    // Second probe (window 0): must stay undetermined, never `stalled`.
    let (_, _, verdict) = fx.probe_t1(&rid);
    assert_eq!(
        verdict, "undetermined",
        "the task's progress must not be judged on the parent repo's HEAD"
    );
}
