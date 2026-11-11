// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro (scout 2026-11-12 F4): open runs never expire.
//!
//! `state::open_runs` (crates/condukt/src/state.rs:621-629) keeps every run with
//! a non-terminal task, whatever its age, and `condukt state list`
//! (crates/condukt/src/main.rs:5126-5137) prints each exactly like a live run.
//! A run abandoned 60 days ago with no live owner is therefore indistinguishable
//! from one being worked right now.
//!
//! Property pinned: a run whose newest task `updated_at` is 60 days old, with
//! no live owner (no claim, no worktree), is either absent from `state list` or
//! carries a (case-insensitive) `stale` marker on its line. The control — a
//! fresh run in the same project — must be listed with no such marker, checked
//! first in the same invocation's output.
//!
//! RED observed at rev 656b1c38 (written without a fix, by a disinterested
//! author — CLAUDE.md §2(a)).
//!
//! The real binary runs against a per-test `$HOME`, state dir and worktree base
//! inside a throwaway git repo with `GIT_CONFIG_GLOBAL=/dev/null`; nothing
//! under the real `~/.condukt`, `~/.budgetguard` or `~/.overwatch` is touched.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const SIXTY_DAYS: u64 = 60 * 24 * 60 * 60;

struct Fixture {
    _tmp: tempfile::TempDir,
    base: PathBuf,
    repo: PathBuf,
    home: PathBuf,
}

fn run_git(dir: &Path, args: &[&str]) {
    let out = harness_core::git_env::scrub(&mut Command::new("git"))
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn find_file(dir: &Path, name: &str) -> Option<PathBuf> {
    for entry in std::fs::read_dir(dir).ok()? {
        let p = entry.ok()?.path();
        if p.is_dir() {
            if let Some(found) = find_file(&p, name) {
                return Some(found);
            }
        } else if p.file_name().and_then(|n| n.to_str()) == Some(name) {
            return Some(p);
        }
    }
    None
}

impl Fixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let repo = base.join("repo");
        let home = base.join("home");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        run_git(&repo, &["init", "-q", "-b", "main"]);
        run_git(&repo, &["config", "user.email", "t@t.t"]);
        run_git(&repo, &["config", "user.name", "t"]);
        run_git(&repo, &["config", "commit.gpgsign", "false"]);
        std::fs::write(repo.join("base.txt"), "base\n").unwrap();
        run_git(&repo, &["add", "base.txt"]);
        run_git(&repo, &["commit", "-q", "-m", "init"]);
        Self {
            _tmp: tmp,
            base,
            repo,
            home,
        }
    }

    fn condukt(&self, args: &[&str]) -> Output {
        harness_core::git_env::scrub(&mut Command::new(env!("CARGO_BIN_EXE_condukt")))
            .args(args)
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env("CONDUKT_WORKTREE_BASE", self.base.join("wts"))
            .env("CONDUKT_DEFAULT_BRANCH", "main")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .env_remove("CONDUKT_DISABLE")
            .env_remove("CONDUKT_STUCK_TTL_SECS")
            .output()
            .expect("spawn condukt")
    }

    /// `condukt state init --run <run>` a one-task run with `goal`.
    fn init_run(&self, run: &str, goal: &str) {
        let dec = serde_json::json!({
            "goal": goal,
            "tasks": [{"id": "t1", "title": "TaskOne", "touched_files": []}]
        });
        let dp = self.base.join(format!("dec-{run}.json"));
        std::fs::write(&dp, dec.to_string()).unwrap();
        let out = self.condukt(&[
            "state",
            "init",
            "--run",
            run,
            "--file",
            dp.to_str().unwrap(),
        ]);
        assert!(
            out.status.success(),
            "fixture precondition: state init {run}: {:?} {:?}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// Age every task of `run` to `secs` ago (task `updated_at` and the state
    /// file's mtime), leaving its statuses non-terminal.
    fn age_run(&self, run: &str, secs: u64) {
        let path = find_file(
            &self.home.join(".condukt").join("state"),
            &format!("{run}.json"),
        )
        .unwrap_or_else(|| panic!("run-state file for {run} not found under $HOME"));
        let mut v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let then = SystemTime::now() - Duration::from_secs(secs);
        let then_secs = then.duration_since(UNIX_EPOCH).unwrap().as_secs() as i64;
        let tasks = v["tasks"].as_array_mut().expect("tasks array");
        assert!(!tasks.is_empty(), "fixture precondition: run has tasks");
        for t in tasks.iter_mut() {
            assert_ne!(
                t["status"], "verified",
                "fixture precondition: task must be non-terminal"
            );
            t["updated_at"] = serde_json::json!(then_secs);
        }
        std::fs::write(&path, serde_json::to_string_pretty(&v).unwrap()).unwrap();
        let f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        f.set_modified(then).unwrap();
    }
}

#[test]
#[ignore = "scout 2026-11-12 F4: RED, open defect. Remove the ignore when fixed"]
fn sixty_day_old_open_run_without_owner_is_not_listed_as_live() {
    let fx = Fixture::new();
    let old = "run-scout-f4-old";
    let fresh = "run-scout-f4-fresh";
    fx.init_run(old, "old-run-goal");
    fx.init_run(fresh, "fresh-run-goal");
    fx.age_run(old, SIXTY_DAYS);

    // No live owner: nothing claimed for either run.
    let claims = fx.condukt(&["state", "claims"]);
    let claims_txt = String::from_utf8_lossy(&claims.stdout).to_string();
    assert!(
        !claims_txt.contains(old),
        "fixture precondition: the old run must have no live claim: {claims_txt}"
    );

    let out = fx.condukt(&["state", "list"]);
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    eprintln!(
        "observation: exit={:?}\nstdout=\n{stdout}\nstderr={stderr}",
        out.status.code()
    );
    assert!(out.status.success(), "state list must succeed: {stderr}");

    // CONTROL: the fresh run is listed, with no stale marker.
    let fresh_line = stdout
        .lines()
        .find(|l| l.starts_with(&format!("{fresh}\t")))
        .unwrap_or_else(|| panic!("CONTROL: fresh run {fresh} must be listed; got {stdout}"));
    assert!(
        !fresh_line.to_lowercase().contains("stale"),
        "CONTROL: fresh run must carry no stale marker: {fresh_line:?}"
    );

    // RED: the 60-day-old ownerless run is absent or marked stale.
    let old_line = stdout.lines().find(|l| l.starts_with(&format!("{old}\t")));
    assert!(
        old_line.is_none_or(|l| l.to_lowercase().contains("stale")),
        "a run whose newest task update is 60 days old, with no live owner, is listed by \
         `condukt state list` exactly like a live run: {old_line:?}\nfull stdout:\n{stdout}"
    );
}
