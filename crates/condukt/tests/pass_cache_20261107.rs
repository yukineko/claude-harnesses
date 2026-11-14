// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Pass-cache contract for condukt's mechanical done_criteria command (user
//! request 2026-11-07). The command is executed by
//! `condukt state check-criteria` (main.rs `run_mechanical`, via
//! `verify::mechanical_skip_verdict`), not by `state set --status verified`.
//!
//! A mechanical command that PASSED against a task worktree must not be
//! re-executed while the worktree content (tracked + untracked-not-ignored)
//! and the exact argv are unchanged; the verdict (exit 0, skip_verifier true)
//! must be the same as for a real pass.
//!
//! Observation: the command appends a line to a counter file outside the
//! tree. Only exit codes, JSON booleans and counts are asserted, never prose.
//!
//! - 1 (RED): unchanged worktree + same argv after a pass is not re-executed.
//! - 2a/2b (CONTROL): editing a tracked file / adding an untracked file re-executes.
//! - 3 (CONTROL): a different argv on the same worktree re-executes.
//! - 4 (CONTROL): a FAILED run is never cached; re-run executes and exits 1.
//! - 5a (CONTROL): worktree is not a git repo (key uncomputable) executes.
//! - 5b (CONTROL): a corrupted cache store executes. Vacuous at HEAD where no
//!   store exists; meaningful after the cache lands.
//!
//! Written by a disinterested author (CLAUDE.md section 2(a)). Do NOT weaken a
//! test to make it pass; remove the `#[ignore]` on a RED test when fixed.

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_condukt")
}

struct Fx {
    base: PathBuf,
    home: PathBuf,
    repo: PathBuf,
    wt: PathBuf,
    counter: PathBuf,
    n: u32,
}

impl Drop for Fx {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

fn git(dir: &Path, args: &[&str]) {
    let out = harness_core::git_env::scrub(&mut Command::new("git"))
        .args(args)
        .current_dir(dir)
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else {
                out.push(p);
            }
        }
    }
}

impl Fx {
    /// `git_wt`: the task worktree is a real git worktree; otherwise a plain dir.
    fn new(tag: &str, git_wt: bool) -> Fx {
        let mut base = std::env::temp_dir();
        base.push(format!("condukt-pcache-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let repo = base.join("repo");
        let home = base.join("home");
        let wt = base.join("wt");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["config", "user.email", "t@t.t"]);
        git(&repo, &["config", "user.name", "t"]);
        git(&repo, &["config", "commit.gpgsign", "false"]);
        std::fs::write(repo.join("base.txt"), "base\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-q", "-m", "init"]);
        if git_wt {
            git(
                &repo,
                &[
                    "worktree",
                    "add",
                    "-q",
                    "-b",
                    "condukt/pc",
                    wt.to_str().unwrap(),
                ],
            );
        } else {
            std::fs::create_dir_all(&wt).unwrap();
            std::fs::write(wt.join("base.txt"), "base\n").unwrap();
        }
        let counter = base.join("counter.txt");
        Fx {
            base,
            home,
            repo,
            wt,
            counter,
            n: 0,
        }
    }

    fn condukt(&self, args: &[&str]) -> Output {
        common::prime_if_running(args, &self.home, "sess-pcache");
        harness_core::git_env::scrub(&mut Command::new(bin()))
            .args(args)
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env(
                "CONDUKT_TESTSCOPE_LEDGER",
                common::testscope_ledger(&self.home),
            )
            .env("CLAUDE_CODE_SESSION_ID", "sess-pcache")
            .env("CONDUKT_AUTONOMOUS", "0")
            .env_remove("CONDUKT_DISABLE")
            .stdin(std::process::Stdio::null())
            .output()
            .expect("spawn condukt")
    }

    fn ok(&self, args: &[&str]) {
        let out = self.condukt(args);
        assert!(out.status.success(), "condukt {args:?} failed: {out:?}");
    }

    /// New run whose t1 has the mechanical check `sh -c 'echo x >> COUNTER; <tail>'`
    /// and whose worktree is the fixture worktree. Returns the run id.
    fn run_with(&mut self, tail: &str) -> String {
        self.n += 1;
        let run = format!("r{}", self.n);
        let cmd = format!("sh -c 'echo x >> {}; {tail}'", self.counter.display());
        let dec = self.base.join(format!("{run}-dec.json"));
        std::fs::write(
            &dec,
            serde_json::json!({
                "goal": "g",
                "tasks": [{
                    "id": "t1", "title": "t", "touched_files": [format!("work{}.txt", self.n)],
                    "deps": [], "class": "parallel",
                    "done_criteria": "the check exits 0",
                    "is_behavioral": false,
                    "mechanical_check": { "cmd": cmd }
                }]
            })
            .to_string(),
        )
        .unwrap();
        self.ok(&[
            "state",
            "init",
            "--run",
            &run,
            "--file",
            dec.to_str().unwrap(),
        ]);
        self.ok(&[
            "state",
            "set",
            "--run",
            &run,
            "--task",
            "t1",
            "--status",
            "running",
            "--worktree",
            self.wt.to_str().unwrap(),
        ]);
        run
    }

    /// Returns (exit code, skip_verifier from stdout JSON).
    fn check(&self, run: &str) -> (Option<i32>, Option<bool>) {
        let out = self.condukt(&["state", "check-criteria", "--run", run, "--task", "t1"]);
        let stdout = String::from_utf8_lossy(&out.stdout);
        let skip = stdout
            .lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .find_map(|v| v.get("skip_verifier").and_then(|b| b.as_bool()));
        (out.status.code(), skip)
    }

    fn runs(&self) -> usize {
        std::fs::read_to_string(&self.counter)
            .map(|s| s.lines().count())
            .unwrap_or(0)
    }
}

const PASS: (Option<i32>, Option<bool>) = (Some(0), Some(true));

#[test]
fn p1_unchanged_worktree_and_argv_after_a_pass_is_not_reexecuted() {
    let mut fx = Fx::new("p1", true);
    let r = fx.run_with("exit 0");
    assert_eq!(fx.check(&r), PASS);
    assert_eq!(fx.runs(), 1);
    assert_eq!(fx.check(&r), PASS, "verdict stays pass on a hit");
    assert_eq!(
        fx.runs(),
        1,
        "mechanical cmd re-executed on an unchanged worktree"
    );
}

#[test]
fn p2a_editing_a_tracked_file_reexecutes() {
    let mut fx = Fx::new("p2a", true);
    let r = fx.run_with("exit 0");
    assert_eq!(fx.check(&r), PASS);
    std::fs::write(fx.wt.join("base.txt"), "edited\n").unwrap();
    assert_eq!(fx.check(&r), PASS);
    assert_eq!(fx.runs(), 2);
}

#[test]
fn p2b_adding_an_untracked_file_reexecutes() {
    let mut fx = Fx::new("p2b", true);
    let r = fx.run_with("exit 0");
    assert_eq!(fx.check(&r), PASS);
    std::fs::write(fx.wt.join("new.txt"), "n\n").unwrap();
    assert_eq!(fx.check(&r), PASS);
    assert_eq!(fx.runs(), 2);
}

#[test]
fn p3_different_argv_on_same_worktree_reexecutes() {
    let mut fx = Fx::new("p3", true);
    let r1 = fx.run_with("exit 0");
    assert_eq!(fx.check(&r1), PASS);
    let r2 = fx.run_with("true; exit 0");
    assert_eq!(fx.check(&r2), PASS);
    assert_eq!(fx.runs(), 2);
}

#[test]
fn p4_a_failed_run_is_never_cached() {
    let mut fx = Fx::new("p4", true);
    let r = fx.run_with("exit 1");
    assert_eq!(fx.check(&r).0, Some(1));
    assert_eq!(fx.check(&r).0, Some(1), "still fails on the same worktree");
    assert_eq!(fx.runs(), 2, "a failed run must be executed again");
}

#[test]
fn p5a_uncomputable_key_not_a_git_repo_executes_every_time() {
    let mut fx = Fx::new("p5a", false);
    let r = fx.run_with("exit 0");
    assert_eq!(fx.check(&r), PASS);
    assert_eq!(fx.check(&r), PASS);
    assert_eq!(fx.runs(), 2, "no key => never a cache hit");
}

#[test]
fn p5b_corrupt_cache_store_executes_instead_of_hitting() {
    let mut fx = Fx::new("p5b", true);
    let r = fx.run_with("exit 0");
    let mut before = Vec::new();
    walk(&fx.home, &mut before);
    assert_eq!(fx.check(&r), PASS);
    let mut after = Vec::new();
    walk(&fx.home, &mut after);
    // Corrupt every file the first passing check created under HOME (the run
    // state existed before, so it is untouched); the cache store is among
    // them once a cache exists.
    for p in after.iter().filter(|p| !before.contains(p)) {
        std::fs::write(p, b"\x00\xffnot json, not anything\x00").unwrap();
    }
    assert_eq!(
        fx.check(&r),
        PASS,
        "corrupt store must not break a green check"
    );
    assert_eq!(fx.runs(), 2, "corrupt store must never be a hit");
}
