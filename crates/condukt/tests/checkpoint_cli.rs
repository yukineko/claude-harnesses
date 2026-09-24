// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! End-to-end coverage for `condukt state checkpoint` / `state rollback` — the
//! durable reversibility net (charter #7). Spawns the built binary against an
//! isolated temp state dir so it exercises the real CLI, the on-disk
//! checkpoint/journal stores, and run-state restoration. Fails before the
//! subcommands exist (unrecognized subcommand) and passes once wired = a
//! genuine Fail->Pass reproduction oracle for the wiring task.

use std::path::{Path, PathBuf};
use std::process::Command;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_condukt")
}

/// A throwaway git repo + an isolated HOME so the run's state, checkpoints and
/// journal land under `<home>/.condukt/state` and never touch the developer's
/// real store (condukt derives its base dir from `$HOME/.condukt`).
struct Fixture {
    repo: PathBuf,
    home: PathBuf,
    state_dir: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let pid = std::process::id();
        let mut base = std::env::temp_dir();
        base.push(format!("condukt-ckpt-cli-{pid}-{tag}"));
        let _ = std::fs::remove_dir_all(&base);
        let repo = base.join("repo");
        let home = base.join("home");
        let state_dir = home.join(".condukt").join("state");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&state_dir).unwrap();
        // Minimal git repo so `worktree::toplevel` resolves without error.
        run_git(&repo, &["init", "-q"]);
        run_git(&repo, &["config", "user.email", "t@t.t"]);
        run_git(&repo, &["config", "user.name", "t"]);
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
            .output()
            .expect("spawn condukt")
    }
}

fn run_git(dir: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("spawn git")
        .status
        .success();
    assert!(ok, "git {args:?} failed");
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

/// Extract the run id from `state init`'s output (the last non-empty line).
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

#[test]
fn checkpoint_then_rollback_restores_run_state_and_journals() {
    let fx = Fixture::new("roundtrip");
    let decomp = write_decomp(&fx);

    let init = fx.condukt(&["state", "init", "--file", decomp.to_str().unwrap()]);
    assert!(init.status.success(), "init failed: {init:?}");
    let rid = run_id_from(&init);

    // Checkpoint the pristine (pending) run-state → seq 1.
    let cp = fx.condukt(&["state", "checkpoint", "--run", &rid, "--label", "phase-a"]);
    assert!(cp.status.success(), "checkpoint failed: {cp:?}");
    assert_eq!(String::from_utf8_lossy(&cp.stdout).trim(), "1");

    // The journal file must now exist.
    let journal = fx.state_dir_journal(&rid);
    assert!(
        journal.exists(),
        "journal not written at {}",
        journal.display()
    );

    // Mutate the task status away from the snapshot.
    let set = fx.condukt(&[
        "state", "set", "--run", &rid, "--task", "t1", "--status", "running",
    ]);
    assert!(set.status.success(), "set failed: {set:?}");
    let show_before = fx.condukt(&["state", "show", "--run", &rid]);
    assert!(String::from_utf8_lossy(&show_before.stdout).contains("running"));

    // Roll back → run-state restored to the snapshot (pending), journal grows.
    let rb = fx.condukt(&["state", "rollback", "--run", &rid]);
    assert!(rb.status.success(), "rollback failed: {rb:?}");
    assert_eq!(String::from_utf8_lossy(&rb.stdout).trim(), "1");

    let show_after = fx.condukt(&["state", "show", "--run", &rid]);
    let after = String::from_utf8_lossy(&show_after.stdout);
    assert!(
        after.contains("\"status\": \"pending\"") || after.contains("Pending"),
        "run-state not restored to snapshot: {after}"
    );

    // Journal has a checkpoint entry followed by a rollback entry.
    let jtext = std::fs::read_to_string(&journal).unwrap();
    let kinds: Vec<&str> = jtext.lines().filter(|l| !l.trim().is_empty()).collect();
    assert!(
        kinds.len() >= 2,
        "expected >=2 journal lines, got {kinds:?}"
    );
    assert!(
        kinds[0].contains("checkpoint"),
        "first entry not checkpoint: {}",
        kinds[0]
    );
    assert!(
        kinds.iter().any(|l| l.contains("rollback")),
        "no rollback entry in journal: {kinds:?}"
    );
}

impl Fixture {
    fn state_dir_journal(&self, rid: &str) -> PathBuf {
        // Mirror the binary's project-key layout by globbing for the journal.
        find_by_suffix(&self.state_dir, &format!("{rid}.journal.jsonl"))
            .unwrap_or_else(|| self.state_dir.join(format!("{rid}.journal.jsonl")))
    }
}

#[test]
fn verified_task_that_fails_auto_rolls_back_and_journals() {
    let fx = Fixture::new("autorollback");
    let decomp = write_decomp(&fx);
    let init = fx.condukt(&["state", "init", "--file", decomp.to_str().unwrap()]);
    assert!(init.status.success(), "init failed: {init:?}");
    let rid = run_id_from(&init);

    // Snapshot the pristine (pending) state, then drive the task to verified.
    let cp = fx.condukt(&["state", "checkpoint", "--run", &rid]);
    assert!(cp.status.success(), "checkpoint failed: {cp:?}");
    let verify = fx.condukt(&[
        "state", "set", "--run", &rid, "--task", "t1", "--status", "verified",
    ]);
    assert!(verify.status.success(), "verify failed: {verify:?}");

    // A verified task that now FAILS must auto-roll-back to the checkpoint.
    let fail = fx.condukt(&[
        "state", "set", "--run", &rid, "--task", "t1", "--status", "failed",
    ]);
    assert!(fail.status.success(), "fail-set failed: {fail:?}");

    // Run-state restored to the snapshot: task is pending again, not failed.
    let show = fx.condukt(&["state", "show", "--run", &rid]);
    let after = String::from_utf8_lossy(&show.stdout);
    assert!(
        after.contains("\"status\": \"pending\"") || after.contains("Pending"),
        "auto-rollback did not restore snapshot: {after}"
    );
    assert!(
        !after.contains("failed"),
        "task still failed after auto-rollback: {after}"
    );

    // The auto-rollback is journaled.
    let journal = fx.state_dir_journal(&rid);
    let jtext = std::fs::read_to_string(&journal).unwrap();
    assert!(
        jtext.contains("auto_rollback"),
        "no auto_rollback journal entry: {jtext}"
    );
}

/// Recursively find a file whose path ends with `suffix` (the project-key dir is
/// a hash we don't want to recompute in the test).
fn find_by_suffix(root: &Path, suffix: &str) -> Option<PathBuf> {
    let entries = std::fs::read_dir(root).ok()?;
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            if let Some(found) = find_by_suffix(&p, suffix) {
                return Some(found);
            }
        } else if p.to_string_lossy().ends_with(suffix) {
            return Some(p);
        }
    }
    None
}
