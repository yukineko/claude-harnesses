// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Teardown paths other than `worktree remove` must also persist the task's
//! tdd proofs (`<wt>/.tdd/<task>.red.json` / `.green.json`) before deleting
//! the worktree (backlog ea6547c9; the sibling file
//! `backlog_records_lost_at_teardown_20261107.rs` covers `remove` only).
//!
//! Covered here, for each of `worktree discard`, `worktree reap` and
//! `worktree cleanup --remove`:
//!   (1) after the teardown, durable copies of `t5.red.json` / `t5.green.json`
//!       exist outside the removed worktree and `condukt state check-oracle`
//!       reports `valid_fp_oracle:true` (run/task identity is kept on every one
//!       of these paths in this fixture);
//!   (2) when the proofs CANNOT be persisted the teardown does not proceed:
//!       the worktree directory is still there (and the exit is non-zero where
//!       the command's contract says a refused removal is a failure).
//! plus: a non-default tdd `proof_dir` (home `~/.tdd/config.toml`) must not
//! make `worktree remove` silently lose the proofs.
//!
//! How persisting is made impossible in (2): the RED file's `lock_down`
//! (read-only dirs under HOME/.condukt) is NOT usable here, because discard /
//! reap / cleanup also need to write run state and locks under HOME/.condukt,
//! so a locked-down home would refuse them for a reason unrelated to the
//! proofs. Instead the proof file itself is made unreadable (mode 000): the
//! copy cannot be read, so it cannot be persisted, while everything else works.
//! Each (2) test has a CONTROL: the identical teardown with an unreadable file
//! in `.tdd` that is NOT a proof does proceed, so the refusal is about the
//! proof and nothing else.
//!
//! Dependencies (stated): a REAL `tdd` on PATH (tests PANIC, not skip, when it
//! is missing) and `git`. HOME, repo, worktree base are tempdirs.
//!
//! Only exit codes, file existence and JSON fields are asserted; no prose.
//!
//! Written by a disinterested author (CLAUDE.md section 2(a)).

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const RUN: &str = "run-friends";
const BUCKET: &str = "session-worktrees";

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

/// The real `tdd` under test: `CONDUKT_TEST_TDD_BIN` if set, otherwise built
/// once from THIS workspace into the same target dir as the condukt binary.
/// Independent of PATH and of any installed plugin-cache version.
fn find_tdd() -> PathBuf {
    static TDD: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    TDD.get_or_init(|| {
        if let Ok(p) = std::env::var("CONDUKT_TEST_TDD_BIN") {
            let p = PathBuf::from(p);
            assert!(
                p.is_file(),
                "CONDUKT_TEST_TDD_BIN={} is not a file",
                p.display()
            );
            return p;
        }
        let exe = PathBuf::from(env!("CARGO_BIN_EXE_condukt"));
        let target = exe
            .parent()
            .and_then(|d| d.parent())
            .expect("target dir above the condukt binary")
            .to_path_buf();
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
        let out = Command::new(cargo)
            .args(["build", "-p", "tdd", "--target-dir"])
            .arg(&target)
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .output()
            .expect("spawn cargo build -p tdd");
        assert!(
            out.status.success(),
            "DEPENDENCY MISSING: could not build the workspace `tdd`: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let bin = target.join("debug").join("tdd");
        assert!(
            bin.is_file(),
            "DEPENDENCY MISSING: {} not built",
            bin.display()
        );
        bin
    })
    .clone()
}

struct Fx {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    home: PathBuf,
    wt_base: PathBuf,
    tdd: PathBuf,
}

impl Fx {
    /// A repo with a run whose tasks are `t5` (kind fix) and `c1` (kind chore).
    fn new() -> Fx {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let repo = base.join("repo");
        let home = base.join("home");
        let wt_base = home.join(".condukt").join("wtbase");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&wt_base).unwrap();
        std::fs::create_dir_all(home.join(".claude").join("projects")).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["config", "user.email", "t@t.t"]);
        git(&repo, &["config", "user.name", "t"]);
        git(&repo, &["config", "commit.gpgsign", "false"]);
        std::fs::write(repo.join("base.txt"), "base\n").unwrap();
        // Proof dirs are git-ignored in real repos; a dirty tree would stop
        // reap / cleanup for a reason unrelated to this test.
        std::fs::write(repo.join(".gitignore"), ".tdd/\ncustom-proofs/\n").unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "seed"]);
        let fx = Fx {
            _tmp: tmp,
            repo,
            home,
            wt_base,
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
            .env("CLAUDE_CODE_SESSION_ID", "sess-teardown-friends")
            .env("CONDUKT_DEFAULT_BRANCH", "main")
            .env("CONDUKT_WORKTREE_BASE", &self.wt_base)
            .env("HARNESS_PROGRESS_WINDOW_SECS", "0")
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

    /// `condukt worktree create` for `task` (branch `b-<task>`), registered in
    /// run state as the task's worktree + branch.
    fn worktree(&self, task: &str) -> PathBuf {
        let branch = format!("b-{task}");
        let out = self.condukt(&[
            "worktree", "create", "--run", RUN, "--topic", task, "--branch", &branch,
        ]);
        assert!(out.status.success(), "worktree create: {out:?}");
        let wt = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
        self.register(task, &wt, None);
        wt
    }

    /// Mark `task` verified so `worktree remove` may tear its worktree down
    /// (backlog 6180333a: an unverified task's worktree is refused). Setup
    /// only: it panics if the verify itself is refused.
    fn verify(&self, task: &str) {
        let out = self.condukt(&[
            "state", "set", "--run", RUN, "--task", task, "--status", "verified",
        ]);
        assert!(
            out.status.success(),
            "fixture: state set verified {task}: {out:?}"
        );
    }

    fn register(&self, task: &str, wt: &Path, branch: Option<&str>) {
        let mut args = vec![
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
        ];
        if let Some(b) = branch {
            args.push("--branch");
            args.push(b);
        }
        let out = self.condukt(&args);
        assert!(out.status.success(), "state set worktree: {out:?}");
    }

    /// Record a genuine RED then GREEN with the real tdd inside `wt`.
    fn record_proofs(&self, wt: &Path, task: &str) {
        let red = self.tdd_in(wt, &["red", "--task", task, "--cmd", "test -f flag"]);
        assert!(red.status.success(), "tdd red: {red:?}");
        std::fs::write(wt.join("flag"), "x").unwrap();
        let green = self.tdd_in(wt, &["green", "--task", task, "--cmd", "test -f flag"]);
        assert!(green.status.success(), "tdd green: {green:?}");
        let o = self.tdd_in(wt, &["oracle", "--task", task]);
        // `flag` is an untracked file; it must not make reap/cleanup see a
        // dirty tree.
        std::fs::remove_file(wt.join("flag")).unwrap();
        assert!(
            o.status.success()
                && String::from_utf8_lossy(&o.stdout).contains("\"valid_fp_oracle\":true"),
            "apparatus: proofs in the worktree must form a valid F->P oracle: {o:?}"
        );
    }

    fn check_oracle(&self, task: &str) -> serde_json::Value {
        let chk = self.condukt(&["state", "check-oracle", "--run", RUN, "--task", task]);
        serde_json::from_slice(&chk.stdout)
            .unwrap_or_else(|e| panic!("check-oracle stdout not JSON ({e}): {chk:?}"))
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

    /// Asserts (1): durable red+green copies outside `wt`, oracle still valid.
    fn assert_durable_and_oracle(&self, wt: &Path, what: &str) {
        assert!(!wt.exists(), "{what}: worktree should be removed");
        let red = self.find_named("t5.red.json", wt);
        let green = self.find_named("t5.green.json", wt);
        assert!(
            !red.is_empty() && !green.is_empty(),
            "{what}: no durable copy of the tdd proofs survives teardown \
             (red copies: {red:?}, green copies: {green:?})"
        );
        let v = self.check_oracle("t5");
        assert_eq!(
            v["valid_fp_oracle"], true,
            "{what}: check-oracle after teardown lost the F->P oracle: {v}"
        );
    }

    /// A reaper/cleanup record binding a dead session to `project` (the
    /// fixture of `reap_backlog_leftover_commit.rs`).
    fn write_dead_record(&self, file: &str, project: &Path) {
        let p = self
            .home
            .join(".backlog")
            .join("drivers")
            .join(BUCKET)
            .join(format!("{file}.driver"));
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        let v = serde_json::json!({
            "session_id": "sess-dead",
            "pid": 1,
            "project": project.to_string_lossy(),
            "registered_at": 0,
            "heartbeat_at": 0,
        });
        std::fs::write(p, serde_json::to_vec_pretty(&v).unwrap()).unwrap();
    }

    /// Run `args` up to 8 times (death needs two probes a window apart) until
    /// `wt` is gone. Returns every run's output.
    fn drive(&self, args: &[&str], wt: &Path) -> Vec<Output> {
        let mut outs = Vec::new();
        for _ in 0..8 {
            outs.push(self.condukt(args));
            if !wt.exists() {
                break;
            }
        }
        outs
    }

    /// A registered, merged, clean `session-*` worktree for task `t5`, whose
    /// branch record carries the run/task identity.
    fn session_worktree(&self) -> PathBuf {
        let name = "session-friends1";
        let wt = self.wt_base.join(name);
        git(
            &self.repo,
            &["worktree", "add", "-q", wt.to_str().unwrap(), "-b", name],
        );
        git(
            &self.repo,
            &[
                "config",
                &format!("branch.{name}.conduktTask"),
                &format!("{RUN}/t5"),
            ],
        );
        let wt = wt.canonicalize().unwrap();
        self.register("t5", &wt, None);
        self.write_dead_record("rec", &wt);
        wt
    }

    /// A directory under the worktree base that git does NOT register but
    /// whose `.git` pointer attributes it to this repo: a byte copy of a
    /// worktree created BEFORE any proofs exist (the original stays, unused).
    fn unregistered_copy(&self) -> PathBuf {
        let orig = self.worktree("t5");
        let copy = self.wt_base.join("stray-copy");
        let out = Command::new("cp")
            .arg("-R")
            .arg(&orig)
            .arg(&copy)
            .output()
            .unwrap();
        assert!(out.status.success(), "cp -R: {out:?}");
        let copy = copy.canonicalize().unwrap();
        self.register("t5", &copy, None);
        self.write_dead_record("rec", &copy);
        copy
    }
}

fn unreadable(p: &Path) {
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o000)).unwrap();
}

fn make_readable(root: &Path) {
    // Best effort so TempDir drop can clean up after a refused teardown.
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                stack.push(p);
            } else {
                let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644));
            }
        }
    }
}

// ───────────────────────────── discard ─────────────────────────────

fn discard(fx: &Fx) -> Output {
    fx.condukt(&["worktree", "discard", "--run", RUN, "--task", "t5"])
}

/// CONTROL: discard of a task whose worktree holds no proofs removes it.
#[test]
fn control_discard_removes_worktree_without_proofs() {
    let fx = Fx::new();
    let wt = fx.worktree("t5");
    let out = discard(&fx);
    assert!(out.status.success(), "discard without proofs: {out:?}");
    assert!(!wt.exists(), "worktree should be gone");
}

/// (1) discard persists the proofs; check-oracle still valid afterwards.
#[test]
fn discard_persists_tdd_proofs_durably() {
    let fx = Fx::new();
    let wt = fx.worktree("t5");
    fx.record_proofs(&wt, "t5");
    let out = discard(&fx);
    assert!(out.status.success(), "worktree discard: {out:?}");
    fx.assert_durable_and_oracle(&wt, "discard");
}

/// CONTROL for (2): an unreadable NON-proof file in `.tdd` does not stop discard.
#[test]
fn control_discard_proceeds_with_unreadable_non_proof_file() {
    let fx = Fx::new();
    let wt = fx.worktree("t5");
    std::fs::create_dir_all(wt.join(".tdd")).unwrap();
    let junk = wt.join(".tdd").join("notes.txt");
    std::fs::write(&junk, "x").unwrap();
    unreadable(&junk);
    let out = discard(&fx);
    make_readable(&wt);
    assert!(out.status.success(), "fixture unsound: {out:?}");
    assert!(!wt.exists());
}

/// (2) discard is refused (non-zero, worktree kept) when a proof cannot be read.
#[test]
fn discard_is_refused_when_proofs_cannot_be_persisted() {
    let fx = Fx::new();
    let wt = fx.worktree("t5");
    fx.record_proofs(&wt, "t5");
    unreadable(&wt.join(".tdd").join("t5.red.json"));
    let out = discard(&fx);
    make_readable(&wt);
    assert!(
        !out.status.success() && wt.is_dir(),
        "discard proceeded although a proof could not be persisted: exit={:?} \
         worktree_exists={} stderr={}",
        out.status.code(),
        wt.is_dir(),
        String::from_utf8_lossy(&out.stderr)
    );
}

// ────────────────────────────── reap ───────────────────────────────

const REAP: &[&str] = &["worktree", "reap"];

/// CONTROL: the reap fixture really reaps a proof-less session worktree
/// within the probe budget (so (1)/(2) observe the proofs, not the fixture).
#[test]
fn control_reap_removes_session_worktree_without_proofs() {
    let fx = Fx::new();
    let wt = fx.session_worktree();
    let outs = fx.drive(REAP, &wt);
    assert!(
        !wt.exists(),
        "fixture unsound: reap never removed a clean merged dead session worktree: {outs:?}"
    );
}

/// (1) reap persists the proofs before removing; check-oracle still valid.
#[test]
fn reap_persists_tdd_proofs_durably() {
    let fx = Fx::new();
    let wt = fx.session_worktree();
    fx.record_proofs(&wt, "t5");
    let outs = fx.drive(REAP, &wt);
    assert!(
        !wt.exists(),
        "fixture unsound: reap did not remove the worktree: {outs:?}"
    );
    fx.assert_durable_and_oracle(&wt, "reap");
}

/// CONTROL for (2): an unreadable non-proof file in `.tdd` does not stop reap.
#[test]
fn control_reap_proceeds_with_unreadable_non_proof_file() {
    let fx = Fx::new();
    let wt = fx.session_worktree();
    std::fs::create_dir_all(wt.join(".tdd")).unwrap();
    let junk = wt.join(".tdd").join("notes.txt");
    std::fs::write(&junk, "x").unwrap();
    unreadable(&junk);
    let outs = fx.drive(REAP, &wt);
    make_readable(&wt);
    assert!(
        !wt.exists(),
        "fixture unsound: reap kept the worktree for a non-proof file: {outs:?}"
    );
}

/// (2) reap keeps the worktree, and exits non-zero (an authorised removal that
/// failed), when a proof cannot be persisted.
#[test]
fn reap_keeps_worktree_when_proofs_cannot_be_persisted() {
    let fx = Fx::new();
    let wt = fx.session_worktree();
    fx.record_proofs(&wt, "t5");
    unreadable(&wt.join(".tdd").join("t5.red.json"));
    let outs = fx.drive(REAP, &wt);
    make_readable(&wt);
    let codes: Vec<_> = outs.iter().map(|o| o.status.code()).collect();
    assert!(
        wt.is_dir(),
        "reap removed the worktree although a proof could not be persisted: exits={codes:?}"
    );
    assert!(
        codes.iter().any(|c| *c != Some(0)),
        "reap kept the worktree but never reported the refused removal: exits={codes:?}"
    );
}

// ─────────────────────── cleanup --remove ──────────────────────────

const CLEANUP: &[&str] = &["worktree", "cleanup", "--remove"];

/// CONTROL: the cleanup fixture's unregistered dir really is removed within
/// the probe budget when it holds no proofs.
#[test]
fn control_cleanup_removes_unregistered_dir_without_proofs() {
    let fx = Fx::new();
    let stray = fx.unregistered_copy();
    let outs = fx.drive(CLEANUP, &stray);
    assert!(
        !stray.exists(),
        "fixture unsound: cleanup --remove never removed the stray dir: {outs:?}"
    );
}

/// (1) cleanup --remove persists the proofs; check-oracle still valid.
#[test]
fn cleanup_remove_persists_tdd_proofs_durably() {
    let fx = Fx::new();
    let stray = fx.unregistered_copy();
    fx.record_proofs(&stray, "t5");
    let outs = fx.drive(CLEANUP, &stray);
    assert!(
        !stray.exists(),
        "fixture unsound: cleanup --remove did not remove the dir: {outs:?}"
    );
    fx.assert_durable_and_oracle(&stray, "cleanup --remove");
}

/// CONTROL for (2): an unreadable non-proof file does not stop cleanup.
#[test]
fn control_cleanup_proceeds_with_unreadable_non_proof_file() {
    let fx = Fx::new();
    let stray = fx.unregistered_copy();
    std::fs::create_dir_all(stray.join(".tdd")).unwrap();
    let junk = stray.join(".tdd").join("notes.txt");
    std::fs::write(&junk, "x").unwrap();
    unreadable(&junk);
    let outs = fx.drive(CLEANUP, &stray);
    make_readable(&stray);
    assert!(
        !stray.exists(),
        "fixture unsound: cleanup kept the dir for a non-proof file: {outs:?}"
    );
}

/// (2) cleanup --remove leaves the directory (and fails) when a proof cannot
/// be persisted.
#[test]
fn cleanup_remove_keeps_dir_when_proofs_cannot_be_persisted() {
    let fx = Fx::new();
    let stray = fx.unregistered_copy();
    fx.record_proofs(&stray, "t5");
    unreadable(&stray.join(".tdd").join("t5.red.json"));
    let outs = fx.drive(CLEANUP, &stray);
    make_readable(&stray);
    let codes: Vec<_> = outs.iter().map(|o| o.status.code()).collect();
    assert!(
        stray.is_dir(),
        "cleanup --remove deleted the dir although a proof could not be persisted: \
         exits={codes:?}"
    );
    assert!(
        codes.iter().any(|c| *c != Some(0)),
        "cleanup kept the dir but never reported the refused removal: exits={codes:?}"
    );
}

// ─────────────────── custom tdd proof_dir (remove) ──────────────────

/// A non-default `proof_dir` in the home tdd config: `tdd red/green` write the
/// proofs there, and `worktree remove` must not silently lose them. Either
/// durable copies + a valid oracle survive, or the removal is refused.
#[test]
fn remove_does_not_silently_lose_proofs_in_a_custom_proof_dir() {
    let fx = Fx::new();
    let cfg_dir = fx.home.join(".tdd");
    std::fs::create_dir_all(&cfg_dir).unwrap();
    std::fs::write(
        cfg_dir.join("config.toml"),
        "proof_dir = \"custom-proofs\"\n",
    )
    .unwrap();
    let wt = fx.worktree("t5");
    fx.record_proofs(&wt, "t5");
    // Apparatus: the config really moved the proofs.
    assert!(
        wt.join("custom-proofs").join("t5.red.json").is_file()
            && !wt.join(".tdd").join("t5.red.json").exists(),
        "fixture unsound: tdd did not honor the custom proof_dir"
    );
    fx.verify("t5");
    let branch = "b-t5";
    let out = fx.condukt(&[
        "worktree",
        "remove",
        "--path",
        wt.to_str().unwrap(),
        "--branch",
        branch,
    ]);
    let refused = !out.status.success() && wt.is_dir();
    if refused {
        return;
    }
    assert!(
        !wt.exists(),
        "remove exited 0 but the worktree is still there"
    );
    let red = fx.find_named("t5.red.json", &wt);
    let green = fx.find_named("t5.green.json", &wt);
    assert!(
        !red.is_empty() && !green.is_empty(),
        "worktree removed silently with its proofs lost (exit={:?}; red copies: {red:?}, \
         green copies: {green:?})",
        out.status.code()
    );
    let v = fx.check_oracle("t5");
    assert_eq!(
        v["valid_fp_oracle"], true,
        "check-oracle lost the F->P oracle after teardown: {v}"
    );
}
