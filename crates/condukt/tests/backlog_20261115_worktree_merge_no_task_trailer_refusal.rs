// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Observed 2026-11-15: `condukt worktree merge --branch <b>` run WITHOUT
//! `--run/--task` refused with `merge-task-trailer-missing: N commit(s) on
//! '<b>' do not carry `Condukt-Task: ...` ... refusing to merge` (branch
//! `condukt/run-20261114-161340-68728/side-reds`). The command's help says
//! `--run ... Omit for a merge with no task association (behavior otherwise
//! unchanged)`.
//!
//! Two readings:
//!  (A) DEFECT: with no --run/--task there is no task association, so the
//!      task-trailer guard must not apply and a plain commit must merge.
//!  (B) INTENDED: the guard applies whenever the branch has a recorded task
//!      identity (`branch.<b>.conduktTask`, written by `worktree create --run`),
//!      even if the merge is invoked without --run/--task; the identity then
//!      comes from the record (`merge_gate.rs`: `(Some(rec), _) => rec`). In
//!      that case the help text ("no task association") is what is wrong.
//!
//! How to tell them apart: the existing test
//! `task_trailer_merge.rs::foreign_trailerless_commit_is_refused` freezes
//! reading (B) for a recorded branch (trailerless commit, no flags => refused).
//! If this test fails because the branch was created through `worktree create
//! --run` (as in this fixture), the observation is explained by (B) and the
//! resolution is a human ruling: fix the help text, or change the existing test
//! plus the guard. For a branch with NO recorded identity the code refuses with
//! a different code (`merge-task-undetermined`), also contradicting the help
//! text. Fixing means a decision on which of help / guard is canonical.
//!
//! DOES NOT PROVE: that the 2026-11-15 branch had a recorded identity (not
//! observed here; the fixture assumes it was created via `worktree create
//! --run`); behaviour of a hand-made branch with no config record; behaviour
//! with a trailered commit; any other merge gate (base, lint, conflicts).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn git(dir: &Path, args: &[&str]) -> Output {
    harness_core::git_env::scrub(&mut Command::new("git"))
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("spawn git")
}

fn run_git(dir: &Path, args: &[&str]) -> String {
    let out = git(dir, args);
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn condukt(repo: &Path, home: &Path, args: &[&str]) -> Output {
    harness_core::git_env::scrub(&mut Command::new(env!("CARGO_BIN_EXE_condukt")))
        .args(args)
        .current_dir(repo)
        .env("HOME", home)
        .env("CLAUDE_CODE_SESSION_ID", "sess-no-task-trailer")
        .env("CONDUKT_DEFAULT_BRANCH", "main")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("CONDUKT_DISABLE")
        .output()
        .expect("spawn condukt")
}

/// Run state for `run` holding `task` at kind=chore.
///
/// Re-anchored per user ruling 2026-11-20 round 4 Q-A (backlog 7768aeaf,
/// spec condukt-merge-proof-contract-7768aeaf R5/R6): a merge of a branch
/// with a `conduktTask` record now reads the task's kind from run state, and
/// an absent run is `merge-proof-contract(undetermined-task)`. kind=chore is
/// outside the proof contract; the property this file guards is unchanged.
fn init_chore_run(repo: &Path, home: &Path, run: &str, task: &str) {
    let dec = home.join(format!("{run}.chore-decomposition.json"));
    std::fs::write(
        &dec,
        format!(r#"{{"goal":"g","tasks":[{{"id":"{task}","title":"t","kind":"chore"}}]}}"#),
    )
    .unwrap();
    let out = condukt(
        repo,
        home,
        &[
            "state",
            "init",
            "--run",
            run,
            "--file",
            dec.to_str().unwrap(),
        ],
    );
    assert!(
        out.status.success(),
        "fixture: state init {run} (kind=chore): {out:?}"
    );
}

struct Fixture {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    home: PathBuf,
    branch: String,
    tip: String,
}

/// Repo on `main`; a worktree branch (created via the CLI) holding one plain
/// commit with no `Condukt-Task` trailer.
fn setup() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    run_git(&repo, &["init", "-q", "-b", "main"]);
    run_git(&repo, &["config", "user.email", "t@t.t"]);
    run_git(&repo, &["config", "user.name", "t"]);
    std::fs::write(repo.join("base.txt"), "base\n").unwrap();
    run_git(&repo, &["add", "."]);
    run_git(&repo, &["commit", "-q", "-m", "init"]);
    init_chore_run(&repo, &home, "runA", "side");
    let out = condukt(
        &repo,
        &home,
        &[
            "worktree", "create", "--run", "runA", "--topic", "side", "--branch", "side",
        ],
    );
    assert!(out.status.success(), "worktree create: {out:?}");
    let wt = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim().to_string());
    let branch = run_git(&wt, &["branch", "--show-current"]);
    std::fs::write(wt.join("plain.txt"), "plain\n").unwrap();
    run_git(&wt, &["add", "plain.txt"]);
    run_git(&wt, &["commit", "-q", "-m", "plain commit without trailer"]);
    let tip = run_git(&wt, &["rev-parse", "HEAD"]);
    Fixture {
        _tmp: tmp,
        repo,
        home,
        branch,
        tip,
    }
}

fn merge(f: &Fixture, extra: &[&str]) -> (Output, String) {
    let mut args = vec!["worktree", "merge", "--branch", f.branch.as_str()];
    args.extend_from_slice(extra);
    let out = condukt(&f.repo, &f.home, &args);
    eprintln!(
        "merge {extra:?} exit={:?}\nstdout={}\nstderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let main_after = run_git(&f.repo, &["rev-parse", "main"]);
    (out, main_after)
}

#[test]
fn merge_without_run_task_does_not_apply_task_trailer_guard() {
    let f = setup();
    let (out, _main_after) = merge(&f, &[]);
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        !stderr.contains("merge-task-trailer-missing"),
        "no --run/--task: the task-trailer guard must not refuse; stderr={stderr:?}"
    );
    assert!(
        out.status.success(),
        "merge without --run/--task must succeed; stderr={stderr:?}"
    );
    assert!(
        git(&f.repo, &["merge-base", "--is-ancestor", &f.tip, "main"])
            .status
            .success(),
        "default branch must contain the plain commit {}",
        f.tip
    );
}

/// Control: with --run/--task naming the task and a commit lacking the
/// trailer, the merge is still refused and main does not move.
#[test]
fn control_merge_with_run_task_and_missing_trailer_is_refused() {
    let f = setup();
    let before = run_git(&f.repo, &["rev-parse", "main"]);
    let (out, after) = merge(&f, &["--run", "runA", "--task", "side"]);
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        !out.status.success(),
        "control: merge with task and trailerless commit must refuse"
    );
    assert!(
        stderr.contains("merge-task-trailer-missing"),
        "control: refusal must be the trailer guard; stderr={stderr:?}"
    );
    assert_eq!(after, before, "refused merge must leave main untouched");
}
