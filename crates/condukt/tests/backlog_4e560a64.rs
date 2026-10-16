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

fn init_git_repo(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "t@t.t"]);
    git(dir, &["config", "user.name", "t"]);
    std::fs::write(dir.join("base.txt"), "base\n").unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "-q", "-m", "init"]);
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
///
/// The second call's exit status is deliberately NOT asserted here: it is the
/// property under test and each caller pins it explicitly (backlog `d65110df`:
/// an observed-dirty keep exits 0, a cannot-determine keep exits 3). The first
/// call is still required to succeed — every fixture is HEAD-readable, so its
/// verdict is an in-progress `AwaitingSample`, which is not an exit-3 failure.
fn abandon_all_stuck_twice(fx: &Fixture, rid: &str) -> std::process::Output {
    let a1 = fx.condukt(&["state", "abandon", "--run", rid, "--all-stuck"]);
    assert!(a1.status.success(), "abandon #1 failed: {a1:?}");
    fx.condukt(&["state", "abandon", "--run", rid, "--all-stuck"])
}

fn stderr_of(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// backlog 4e560a64. The recorded task worktree loses its own `.git` but sits
/// inside ANOTHER git repository (here: `$HOME` made a repo). `is_dirty` runs
/// `git status --porcelain` with that path as cwd, git walks up to the parent,
/// and the parent's dirtiness (an untracked `.condukt/` OUTSIDE the worktree)
/// is reported as "worktree <wt> has uncommitted changes" — a confident answer
/// about the wrong tree. The worktree path itself holds no uncommitted change.
#[test]
#[ignore = "backlog 4e560a64: RED repro"]
fn dirtiness_of_a_gitless_worktree_is_not_read_from_the_enclosing_repo() {
    let fx = Fixture::new("b4e560a64");
    let (rid, wt) = running_task_past_ttl(&fx);

    // The worktree loses its own .git; $HOME becomes the enclosing repo, with
    // the worktree's files committed there and `.condukt/` left untracked.
    std::fs::remove_dir_all(wt.join(".git")).unwrap();
    git(&fx.home, &["init", "-q", "-b", "main"]);
    git(&fx.home, &["config", "user.email", "t@t.t"]);
    git(&fx.home, &["config", "user.name", "t"]);
    git(&fx.home, &["add", "worker-worktree"]);
    git(&fx.home, &["commit", "-q", "-m", "parent"]);

    // Preconditions: nothing under the worktree path is uncommitted, while the
    // enclosing repo IS dirty elsewhere.
    let scoped = git(&wt, &["status", "--porcelain", "--", "."]);
    assert!(
        scoped.is_empty(),
        "precondition: worktree path clean: {scoped:?}"
    );
    let whole = git(&wt, &["status", "--porcelain"]);
    assert!(
        whole.contains(".condukt"),
        "precondition: parent dirty outside the worktree: {whole:?}"
    );

    let a2 = abandon_all_stuck_twice(&fx, &rid);
    let err = stderr_of(&a2);
    let wrong = format!("worktree {} has uncommitted changes", wt.display());
    assert!(
        !err.contains(&wrong),
        "the worktree has no .git of its own and nothing uncommitted under it; the \
         'uncommitted changes' reading came from the ENCLOSING repo. stderr:\n{err}"
    );
}
