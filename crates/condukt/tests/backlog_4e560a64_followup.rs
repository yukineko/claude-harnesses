//! backlog 4e560a64 follow-up: the remaining condukt paths that run git with a
//! RECORDED task worktree as cwd. When that worktree has lost its own `.git`
//! but sits inside another git repository, git walks up and answers about the
//! ENCLOSING repository. Each test below builds exactly that layout (the
//! worktree is a plain directory inside the run's repo) and asserts condukt
//! does not act on / report the parent's answer as the worktree's.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use std::path::{Path, PathBuf};
use std::process::Command;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_condukt")
}

struct Fixture {
    repo: PathBuf,
    home: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let pid = std::process::id();
        let mut base = std::env::temp_dir();
        base.push(format!("condukt-4e560a64-followup-{pid}-{tag}"));
        let _ = std::fs::remove_dir_all(&base);
        let repo = base.join("repo");
        let home = base.join("home");
        std::fs::create_dir_all(home.join(".condukt").join("state")).unwrap();
        init_git_repo(&repo);
        Self { repo, home }
    }

    fn condukt(&self, args: &[&str]) -> std::process::Output {
        Command::new(bin())
            .args(args)
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env("HARNESS_PROGRESS_WINDOW_SECS", "0")
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .output()
            .expect("spawn condukt")
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
    git(dir, &["add", "base.txt"]);
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

/// Init a run whose task `t1` is `running` on a worktree INSIDE the run repo
/// (`<repo>/wt`) that was its own git repo when recorded and has since lost its
/// `.git`. Returns `(run id, worktree path)`.
fn running_task_on_gitless_worktree(fx: &Fixture) -> (String, PathBuf) {
    let decomp = fx.home.join("decomp.json");
    std::fs::write(
        &decomp,
        r#"{"goal":"g","tasks":[{"id":"t1","title":"x","touched_files":["a.rs"],"deps":[],"class":"serial","done_criteria":"d"}]}"#,
    )
    .unwrap();
    let init = fx.condukt(&["state", "init", "--file", decomp.to_str().unwrap()]);
    assert!(init.status.success(), "state init failed: {init:?}");
    let rid = run_id_from(&init);

    git(&fx.repo, &["branch", "feat/t1"]);
    let wt = fx.repo.join("wt");
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
    assert!(set.status.success(), "state set running failed: {set:?}");
    std::fs::remove_dir_all(wt.join(".git")).unwrap();
    assert_eq!(
        Path::new(&git(&wt, &["rev-parse", "--show-toplevel"]))
            .canonicalize()
            .unwrap(),
        fx.repo.canonicalize().unwrap(),
        "precondition: git run in the gitless worktree resolves the ENCLOSING repo"
    );
    (rid, wt)
}

/// Candidate A — `state rollback` runs `git reset --hard SHA` in each recorded
/// worktree. In a gitless worktree that reset lands on the ENCLOSING repo,
/// moving its branch and discarding its commits.
#[test]
fn rollback_does_not_reset_the_enclosing_repo_of_a_gitless_worktree() {
    let fx = Fixture::new("rollback");
    let (rid, _wt) = running_task_on_gitless_worktree(&fx);

    let cp = fx.condukt(&["state", "checkpoint", "--run", &rid]);
    assert!(cp.status.success(), "checkpoint failed: {cp:?}");

    // The enclosing repo moves on after the checkpoint.
    std::fs::write(fx.repo.join("later.txt"), "later\n").unwrap();
    git(&fx.repo, &["add", "later.txt"]);
    git(&fx.repo, &["commit", "-q", "-m", "later"]);
    let parent_head = git(&fx.repo, &["rev-parse", "HEAD"]);

    let _rb = fx.condukt(&["state", "rollback", "--run", &rid]);
    assert_eq!(
        git(&fx.repo, &["rev-parse", "HEAD"]),
        parent_head,
        "rollback reset the ENCLOSING repo's HEAD through a worktree that has no .git of its own"
    );
    assert!(
        fx.repo.join("later.txt").exists(),
        "rollback discarded the enclosing repo's committed file"
    );
}

/// Make the enclosing repo sit on a branch that differs from `main`, so a
/// `git diff main...HEAD` walked up from the worktree is NON-empty and gets
/// classified / recorded as if it were the task's change.
fn diverge_enclosing_repo(fx: &Fixture) {
    git(&fx.repo, &["checkout", "-q", "-b", "other"]);
    std::fs::write(fx.repo.join("parent_only.rs"), "pub fn parent() {}\n").unwrap();
    git(&fx.repo, &["add", "parent_only.rs"]);
    git(&fx.repo, &["commit", "-q", "-m", "parent change"]);
}

/// Candidate B — the done-edge runtime-overlap check lists the task's changed
/// files with `git diff --name-only BASE...HEAD` in the worktree. In a gitless
/// worktree it lists the ENCLOSING repo's changes and reports a checked
/// "no overlap"; the check must instead be named as not run.
#[test]
fn overlap_check_of_a_gitless_worktree_is_not_read_from_the_enclosing_repo() {
    let fx = Fixture::new("overlap");
    let (rid, _wt) = running_task_on_gitless_worktree(&fx);
    diverge_enclosing_repo(&fx);

    let done = fx.condukt(&[
        "state", "set", "--run", &rid, "--task", "t1", "--status", "done",
    ]);
    let err = String::from_utf8_lossy(&done.stderr);
    assert!(
        err.contains("runtime-overlap check could NOT run for task 't1'"),
        "the overlap check silently used the ENCLOSING repo's diff as task t1's changeset. \
         stderr:\n{err}"
    );
}

/// Candidate C — the done-edge diff-risk recording diffs the worktree with
/// `git diff BASE...HEAD`. In a gitless worktree it classifies the ENCLOSING
/// repo's diff and reports nothing (classified-not-high); it must instead be
/// named as not inspected.
#[test]
fn diff_risk_of_a_gitless_worktree_is_not_read_from_the_enclosing_repo() {
    let fx = Fixture::new("diffrisk");
    let (rid, _wt) = running_task_on_gitless_worktree(&fx);
    diverge_enclosing_repo(&fx);

    let done = fx.condukt(&[
        "state", "set", "--run", &rid, "--task", "t1", "--status", "done",
    ]);
    let err = String::from_utf8_lossy(&done.stderr);
    assert!(
        err.contains("diff-risk: NOT INSPECTED") || err.contains("diff-risk: NOT RUN"),
        "diff-risk classified the ENCLOSING repo's diff as task t1's and reported it clean. \
         stderr:\n{err}"
    );
}
