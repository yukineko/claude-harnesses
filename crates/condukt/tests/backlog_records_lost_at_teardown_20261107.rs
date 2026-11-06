// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repros (2026-11-07, session c2e3bc17): condukt loses records at teardown.
//!
//! (a) `condukt worktree remove` deletes a task worktree's tdd proofs
//!     (`<wt>/.tdd/<task>.red.json` / `.green.json`). Afterwards `tdd oracle`
//!     and `condukt state check-oracle` cannot see a F->P oracle any more; only
//!     run state's own `fp_oracle_valid` flag survives. Desired: the proofs are
//!     persisted durably (keyed by run + task) BEFORE removal, check-oracle
//!     still reports `valid_fp_oracle:true` afterwards, and if persisting fails
//!     the teardown is refused (non-zero, worktree left in place).
//! (b) The verifier verdict (pass/fail, confidence, reason) has no home in run
//!     state: `state set --verifier-model/--verifier-agent-id` stores only the
//!     model and agent id.
//!
//! Dependencies of this file (stated, not hidden):
//!   * a REAL `tdd` binary reachable on PATH (or the plugin cache) -- (a1) and
//!     the proof-recording fixture run `tdd red` / `tdd green` / `tdd oracle`
//!     for real. The tests PANIC (not skip) if `tdd` cannot be found, because
//!     a silently skipped test would be a green that observed nothing.
//!   * `git`.
//!
//! Everything else (HOME, repo, state dir) is a tempdir.
//!
//! The durable location is deliberately NOT pinned: (a1) scans the isolated
//! HOME for the proof files outside the removed worktree, and (a2) makes every
//! directory under HOME/.condukt except the worktree tree read-only.
//! The (b) CLI shape (`--verifier-verdict/--verifier-confidence/
//! --verifier-reason`) is one natural choice; the pinned part is the JSON in
//! `state show` (keys `verifier_verdict`, `verifier_confidence`,
//! `verifier_reason`).
//!
//! Written by a disinterested author (CLAUDE.md section 2(a)): no fix exists.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const RUN: &str = "run-teardown";

fn git(dir: &Path, args: &[&str]) -> String {
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
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn find_tdd() -> PathBuf {
    let path = std::env::var("PATH").unwrap_or_default();
    for d in path.split(':') {
        let c = Path::new(d).join("tdd");
        if c.is_file() {
            return c;
        }
    }
    panic!("DEPENDENCY MISSING: no `tdd` binary on PATH; this test needs the real tdd");
}

struct Fx {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    home: PathBuf,
    tdd: PathBuf,
}

impl Fx {
    /// A repo with a run whose tasks are `t5` (kind fix) and `c1` (kind chore).
    fn new() -> Fx {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let repo = base.join("repo");
        let home = base.join("home");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["config", "user.email", "t@t.t"]);
        git(&repo, &["config", "user.name", "t"]);
        git(&repo, &["config", "commit.gpgsign", "false"]);
        std::fs::write(repo.join("base.txt"), "base\n").unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "seed"]);
        let fx = Fx {
            _tmp: tmp,
            repo,
            home,
            tdd: find_tdd(),
        };
        let dec = base.join("dec.json");
        std::fs::write(
            &dec,
            r#"{"goal":"g","tasks":[
              {"id":"t5","title":"a","kind":"fix","reproduction_tests":"test -f flag","touched_files":["a.txt"],"deps":[],"class":"parallel","done_criteria":"d"},
              {"id":"c1","title":"b","kind":"chore","touched_files":["b.txt"],"deps":[],"class":"parallel","done_criteria":"d"}]}"#,
        )
        .unwrap();
        let out = fx.condukt(&[
            "state",
            "init",
            "--run",
            RUN,
            "--file",
            dec.to_str().unwrap(),
        ]);
        assert!(out.status.success(), "state init: {out:?}");
        fx
    }

    fn path_env(&self) -> String {
        format!(
            "{}:{}",
            self.tdd.parent().unwrap().display(),
            std::env::var("PATH").unwrap_or_default()
        )
    }

    fn condukt(&self, args: &[&str]) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_condukt"));
        harness_core::git_env::scrub(&mut cmd);
        cmd.args(args)
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env("PATH", self.path_env())
            .env("CLAUDE_CODE_SESSION_ID", "sess-teardown-records")
            .env("CONDUKT_DEFAULT_BRANCH", "main")
            .env_remove("CONDUKT_DISABLE")
            .output()
            .expect("spawn condukt")
    }

    fn tdd_in(&self, dir: &Path, args: &[&str]) -> Output {
        Command::new(&self.tdd)
            .args(args)
            .current_dir(dir)
            .env("HOME", &self.home)
            .env_remove("TDD_DISABLE")
            .output()
            .expect("spawn tdd")
    }

    /// Create a task worktree for `topic`, register it in run state for `task`.
    fn worktree(&self, task: &str) -> (PathBuf, String) {
        let branch = format!("b-{task}");
        let out = self.condukt(&[
            "worktree", "create", "--run", RUN, "--topic", task, "--branch", &branch,
        ]);
        assert!(out.status.success(), "worktree create: {out:?}");
        let wt = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
        let out = self.condukt(&[
            "state",
            "set",
            "--run",
            RUN,
            "--task",
            task,
            "--status",
            "done",
            "--worktree",
            wt.to_str().unwrap(),
        ]);
        assert!(out.status.success(), "state set worktree: {out:?}");
        (wt, branch)
    }

    /// Record a genuine RED then GREEN with the real tdd inside `wt`.
    fn record_proofs(&self, wt: &Path, task: &str) {
        let red = self.tdd_in(wt, &["red", "--task", task, "--cmd", "test -f flag"]);
        assert!(red.status.success(), "tdd red: {red:?}");
        std::fs::write(wt.join("flag"), "x").unwrap();
        let green = self.tdd_in(wt, &["green", "--task", task, "--cmd", "test -f flag"]);
        assert!(green.status.success(), "tdd green: {green:?}");
        let o = self.tdd_in(wt, &["oracle", "--task", task]);
        assert!(
            o.status.success()
                && String::from_utf8_lossy(&o.stdout).contains("\"valid_fp_oracle\":true"),
            "apparatus: proofs in the worktree must form a valid F->P oracle: {o:?}"
        );
    }

    fn remove(&self, wt: &Path, branch: &str) -> Output {
        self.condukt(&[
            "worktree",
            "remove",
            "--path",
            wt.to_str().unwrap(),
            "--branch",
            branch,
        ])
    }

    /// Files named `name` anywhere under HOME and the repo, excluding `skip`.
    fn find_named(&self, name: &str, skip: &Path) -> Vec<PathBuf> {
        let mut found = Vec::new();
        let mut stack = vec![self.home.clone(), self.repo.clone()];
        while let Some(d) = stack.pop() {
            if d.starts_with(skip) {
                continue;
            }
            let Ok(rd) = std::fs::read_dir(&d) else {
                continue;
            };
            for e in rd.flatten() {
                let p = e.path();
                let Ok(ft) = e.file_type() else { continue };
                if ft.is_dir() {
                    if e.file_name() != ".git" {
                        stack.push(p);
                    }
                } else if e.file_name() == name {
                    found.push(p);
                }
            }
        }
        found
    }
}

fn set_dir_mode(root: &Path, skip: &Path, mode: u32) {
    let mut stack = vec![root.to_path_buf()];
    let mut dirs = Vec::new();
    while let Some(d) = stack.pop() {
        if d.starts_with(skip) {
            continue;
        }
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                stack.push(e.path());
            }
        }
        dirs.push(d);
    }
    for d in dirs {
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(mode)).unwrap();
    }
}

/// CONTROL: removing a worktree that holds no proofs works today, and the
/// worktree directory is gone. Anti-vacuity for the (a) tests' fixture.
#[test]
fn control_remove_deletes_a_clean_worktree() {
    let fx = Fx::new();
    let (wt, branch) = fx.worktree("c1");
    assert!(wt.is_dir());
    let out = fx.remove(&wt, &branch);
    assert!(out.status.success(), "remove of a clean worktree: {out:?}");
    assert!(
        !wt.exists(),
        "worktree dir should be gone: {}",
        wt.display()
    );
}

/// CONTROL: `state set --verifier-model/--verifier-agent-id` is stored today.
#[test]
fn control_state_set_stores_verifier_model() {
    let fx = Fx::new();
    let out = fx.condukt(&[
        "state",
        "set",
        "--run",
        RUN,
        "--task",
        "c1",
        "--status",
        "verified",
        "--verifier-model",
        "opus",
        "--verifier-agent-id",
        "agent-123",
    ]);
    assert!(out.status.success(), "state set verified: {out:?}");
    let show = fx.condukt(&["state", "show", "--run", RUN]);
    let v: serde_json::Value = serde_json::from_slice(&show.stdout).unwrap();
    let t = v["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == "c1")
        .unwrap();
    assert_eq!(t["verifier_model"], "opus", "task json: {t}");
    assert_eq!(t["verifier_agent_id"], "agent-123", "task json: {t}");
}

/// CONTROL for (a1): before removal the real tdd sees a valid F->P oracle in
/// the worktree and `condukt state check-oracle` agrees (so a post-removal
/// `false` is attributable to the removal, not to the fixture).
#[test]
fn control_check_oracle_valid_before_removal() {
    let fx = Fx::new();
    let (wt, _branch) = fx.worktree("t5");
    fx.record_proofs(&wt, "t5");
    let out = fx.condukt(&["state", "check-oracle", "--run", RUN, "--task", "t5"]);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("check-oracle stdout not JSON ({e}): {out:?}"));
    assert_eq!(v["valid_fp_oracle"], true, "before removal: {v}");
}

/// (a1) After `worktree remove`, durable copies of the red+green proofs exist
/// outside the worktree and check-oracle still reports a valid F->P oracle.
#[test]
fn remove_persists_tdd_proofs_durably() {
    let fx = Fx::new();
    let (wt, branch) = fx.worktree("t5");
    fx.record_proofs(&wt, "t5");
    let out = fx.remove(&wt, &branch);
    assert!(out.status.success(), "worktree remove: {out:?}");
    assert!(!wt.exists(), "worktree should be removed");

    let red = fx.find_named("t5.red.json", &wt);
    let green = fx.find_named("t5.green.json", &wt);
    assert!(
        !red.is_empty() && !green.is_empty(),
        "no durable copy of the tdd proofs survives worktree removal \
         (red copies: {red:?}, green copies: {green:?})"
    );

    let chk = fx.condukt(&["state", "check-oracle", "--run", RUN, "--task", "t5"]);
    let v: serde_json::Value = serde_json::from_slice(&chk.stdout)
        .unwrap_or_else(|e| panic!("check-oracle stdout not JSON ({e}): {chk:?}"));
    assert_eq!(
        v["valid_fp_oracle"], true,
        "check-oracle after teardown lost the F->P oracle: {v}"
    );
}

/// (a2) If the proofs cannot be persisted, teardown is refused: non-zero exit
/// and the worktree directory is still there. Persisting is made impossible by
/// making every directory under HOME/.condukt read-only except the worktree
/// tree itself (whose parent must stay writable so a plain removal CAN
/// succeed -- proven by the control below).
#[test]
fn remove_is_refused_when_proofs_cannot_be_persisted() {
    let fx = Fx::new();
    let (wt, branch) = fx.worktree("t5");
    fx.record_proofs(&wt, "t5");
    let condukt_home = fx.home.join(".condukt");
    lock_down(&condukt_home, &wt);
    let out = fx.remove(&wt, &branch);
    unlock(&condukt_home);
    assert!(
        !out.status.success() && wt.is_dir(),
        "teardown proceeded although the proofs could not be persisted: exit={:?} \
         worktree_exists={} stdout={} stderr={}",
        out.status.code(),
        wt.is_dir(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Read-only every dir under `root` that is not inside `keep` and not an
/// ancestor of `keep` (ancestors must stay writable so removal of `keep`'s
/// tree is possible).
fn lock_down(root: &Path, keep: &Path) {
    let mut stack = vec![root.to_path_buf()];
    let mut to_lock = Vec::new();
    while let Some(d) = stack.pop() {
        if d.starts_with(keep) {
            continue;
        }
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                stack.push(e.path());
            }
        }
        if !keep.starts_with(&d) {
            to_lock.push(d);
        }
    }
    for d in to_lock {
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o555)).unwrap();
    }
}

fn unlock(root: &Path) {
    set_dir_mode(root, Path::new("/nonexistent-skip"), 0o755);
}

/// CONTROL for (a2): the same lock-down does not by itself stop removal of a
/// worktree that holds no proofs (nothing to persist), so (a2)'s refusal can
/// only be about the proofs.
#[test]
fn control_lockdown_does_not_block_removal_without_proofs() {
    let fx = Fx::new();
    let (wt, branch) = fx.worktree("c1");
    let condukt_home = fx.home.join(".condukt");
    lock_down(&condukt_home, &wt);
    let out = fx.remove(&wt, &branch);
    unlock(&condukt_home);
    assert!(
        out.status.success() && !wt.exists(),
        "lock-down alone breaks removal (fixture unsound): {out:?}"
    );
}

/// (b1) Applying a verifier verdict records verdict, confidence and reason in
/// run state.
#[test]
fn state_set_records_verifier_verdict_confidence_reason() {
    let fx = Fx::new();
    let out = fx.condukt(&[
        "state",
        "set",
        "--run",
        RUN,
        "--task",
        "c1",
        "--status",
        "verified",
        "--verifier-model",
        "opus",
        "--verifier-agent-id",
        "agent-123",
        "--verifier-verdict",
        "pass",
        "--verifier-confidence",
        "medium",
        "--verifier-reason",
        "criteria met; tests exercise the path",
    ]);
    assert!(
        out.status.success(),
        "state set with verdict flags was not accepted: exit={:?} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    let show = fx.condukt(&["state", "show", "--run", RUN]);
    let v: serde_json::Value = serde_json::from_slice(&show.stdout).unwrap();
    let t = v["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == "c1")
        .unwrap();
    for (key, want) in [
        ("verifier_verdict", "pass"),
        ("verifier_confidence", "medium"),
        ("verifier_reason", "criteria met; tests exercise the path"),
    ] {
        assert_eq!(
            t[key].as_str(),
            Some(want),
            "run state lost `{key}`; task json: {t}"
        );
    }
}
