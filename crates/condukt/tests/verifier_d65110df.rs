// Independent verifier test for backlog d65110df (written by the condukt
// verifier, not by the implementing worker). Integration test: unwrap allowed.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! `state abandon --all-stuck` must exit 3 when the uncommitted-work veto
//! cannot read a confirmed-stalled task's worktree dirtiness, exit 0 when the
//! worktree is OBSERVED dirty, and still abandon a clean stalled task — driven
//! end to end against the real binary in throwaway git repos with an isolated
//! `$HOME`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Fx {
    base: PathBuf,
    repo: PathBuf,
    home: PathBuf,
}

impl Fx {
    fn new(tag: &str) -> Self {
        let base = std::env::temp_dir().join(format!(
            "condukt-verifier-d65110df-{}-{tag}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&base);
        let repo = base.join("repo");
        let home = base.join("home");
        std::fs::create_dir_all(home.join(".condukt").join("state")).unwrap();
        init_repo(&repo);
        Self { base, repo, home }
    }

    fn run(&self, args: &[&str]) -> Output {
        harness_core::git_env::scrub(&mut Command::new(env!("CARGO_BIN_EXE_condukt")))
            .args(args)
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env("HARNESS_PROGRESS_WINDOW_SECS", "0")
            .output()
            .expect("spawn condukt")
    }

    fn state_file(&self, rid: &str) -> PathBuf {
        let dir = self.home.join(".condukt").join("state");
        for e in std::fs::read_dir(&dir).unwrap() {
            let p = e.unwrap().path().join(format!("{rid}.json"));
            if p.exists() {
                return p;
            }
        }
        panic!("no run-state file for {rid}");
    }

    fn json(&self, rid: &str) -> serde_json::Value {
        serde_json::from_str(&std::fs::read_to_string(self.state_file(rid)).unwrap()).unwrap()
    }

    fn status_of(&self, rid: &str, id: &str) -> String {
        let v = self.json(rid);
        v["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["id"] == id)
            .unwrap()["status"]
            .as_str()
            .unwrap()
            .to_string()
    }

    /// Init a run with `ids`, put each into `running` on its own fresh git
    /// worktree, and backdate `updated_at` far past the stuck TTL.
    fn stalled_run(&self, ids: &[&str]) -> (String, Vec<PathBuf>) {
        let tasks: Vec<String> = ids
            .iter()
            .map(|id| {
                format!(
                    r#"{{"id":"{id}","title":"x","touched_files":["{id}.rs"],"deps":[],"class":"parallel","done_criteria":"d"}}"#
                )
            })
            .collect();
        let decomp = self.repo.join("decomp.json");
        std::fs::write(
            &decomp,
            format!(r#"{{"goal":"g","tasks":[{}]}}"#, tasks.join(",")),
        )
        .unwrap();
        let init = self.run(&["state", "init", "--file", decomp.to_str().unwrap()]);
        assert!(init.status.success(), "init: {init:?}");
        let text = format!(
            "{}\n{}",
            String::from_utf8_lossy(&init.stdout),
            String::from_utf8_lossy(&init.stderr)
        );
        let rid = text
            .lines()
            .rev()
            .map(str::trim)
            .find(|l| l.starts_with("run-"))
            .expect("run id")
            .to_string();
        let mut wts = Vec::new();
        for id in ids {
            let wt = self.base.join(format!("wt-{id}"));
            init_repo(&wt);
            let set = self.run(&[
                "state",
                "set",
                "--run",
                &rid,
                "--task",
                id,
                "--status",
                "running",
                "--worktree",
                wt.to_str().unwrap(),
                "--branch",
                &format!("feat/{id}"),
            ]);
            assert!(set.status.success(), "set {id}: {set:?}");
            wts.push(wt);
        }
        let path = self.state_file(&rid);
        let mut v = self.json(&rid);
        for t in v["tasks"].as_array_mut().unwrap() {
            t["updated_at"] = serde_json::json!(1_000);
        }
        std::fs::write(&path, serde_json::to_string_pretty(&v).unwrap()).unwrap();
        (rid, wts)
    }

    fn abandon(&self, rid: &str) -> Output {
        self.run(&["state", "abandon", "--run", rid, "--all-stuck"])
    }
}

impl Drop for Fx {
    fn drop(&mut self) {
        // restore perms so the temp dir can be removed
        let _ = Command::new("chmod")
            .args(["-R", "u+rwx"])
            .arg(&self.base)
            .status();
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

fn git(dir: &Path, args: &[&str]) -> Output {
    harness_core::git_env::scrub(&mut Command::new("git"))
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap()
}

fn init_repo(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    for a in [
        &["init", "-q", "-b", "main"][..],
        &["config", "user.email", "v@v.v"],
        &["config", "user.name", "v"],
    ] {
        assert!(git(dir, a).status.success());
    }
    std::fs::write(dir.join("base.txt"), "base\n").unwrap();
    assert!(git(dir, &["add", "."]).status.success());
    assert!(git(dir, &["commit", "-q", "-m", "init"]).status.success());
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// HEAD readable, `git status` fails (corrupt index) → exit 3, task kept,
/// veto line still printed.
#[test]
fn verifier_corrupt_index_stalled_task_exits_3() {
    let fx = Fx::new("corrupt");
    let (rid, wts) = fx.stalled_run(&["t1"]);
    std::fs::write(wts[0].join(".git").join("index"), b"garbage").unwrap();
    assert!(
        !git(&wts[0], &["status", "--porcelain"]).status.success(),
        "fixture void"
    );
    assert!(
        git(&wts[0], &["rev-parse", "HEAD"]).status.success(),
        "fixture void"
    );

    let a1 = fx.abandon(&rid);
    assert_eq!(
        a1.status.code(),
        Some(0),
        "sample #1 is AwaitingSample: {a1:?}"
    );
    let a2 = fx.abandon(&rid);
    let e = err(&a2);
    assert_eq!(
        a2.status.code(),
        Some(3),
        "dirtiness unreadable must exit 3; stderr:\n{e}"
    );
    assert!(
        e.contains("NOT abandoning running task 't1'"),
        "veto line missing:\n{e}"
    );
    assert_eq!(
        fx.status_of(&rid, "t1"),
        "running",
        "cannot-determine must KEEP the task"
    );
}

/// Index made unreadable by permissions (another `git status` failure mode
/// with HEAD still readable) → exit 3.
#[test]
fn verifier_unreadable_index_stalled_task_exits_3() {
    let fx = Fx::new("unreadable");
    let (rid, wts) = fx.stalled_run(&["t1"]);
    let idx = wts[0].join(".git").join("index");
    assert!(Command::new("chmod")
        .arg("000")
        .arg(&idx)
        .status()
        .unwrap()
        .success());
    if git(&wts[0], &["status", "--porcelain"]).status.success() {
        // running as a user that bypasses permissions: the fixture cannot be
        // built; fail loudly rather than pass vacuously.
        panic!("fixture void: git status still succeeds on a chmod-000 index");
    }
    assert!(
        git(&wts[0], &["rev-parse", "HEAD"]).status.success(),
        "fixture void"
    );

    let a1 = fx.abandon(&rid);
    assert_eq!(a1.status.code(), Some(0), "{a1:?}");
    let a2 = fx.abandon(&rid);
    let e = err(&a2);
    assert_eq!(a2.status.code(), Some(3), "stderr:\n{e}");
    assert!(e.contains("NOT abandoning running task 't1'"), "{e}");
    assert_eq!(fx.status_of(&rid, "t1"), "running");
}

/// Worktree removed between samples → still not clean (exit 3), kept.
#[test]
fn verifier_removed_worktree_exits_3() {
    let fx = Fx::new("removed");
    let (rid, wts) = fx.stalled_run(&["t1"]);
    let a1 = fx.abandon(&rid);
    assert_eq!(a1.status.code(), Some(0), "{a1:?}");
    std::fs::remove_dir_all(&wts[0]).unwrap();
    let a2 = fx.abandon(&rid);
    assert_eq!(a2.status.code(), Some(3), "stderr:\n{}", err(&a2));
    assert_eq!(fx.status_of(&rid, "t1"), "running");
}

/// Observed dirty → exit 0 (control for the asymmetry), kept, veto line.
#[test]
fn verifier_dirty_worktree_exits_0_and_is_kept() {
    let fx = Fx::new("dirty");
    let (rid, wts) = fx.stalled_run(&["t1"]);
    std::fs::write(wts[0].join("wip.rs"), "fn x(\n").unwrap();
    let a1 = fx.abandon(&rid);
    assert_eq!(a1.status.code(), Some(0), "{a1:?}");
    let a2 = fx.abandon(&rid);
    let e = err(&a2);
    assert_eq!(
        a2.status.code(),
        Some(0),
        "observed dirty must exit 0; stderr:\n{e}"
    );
    assert!(e.contains("NOT abandoning running task 't1'"), "{e}");
    assert!(
        !e.contains("UNDETERMINED"),
        "dirty must not be undetermined:\n{e}"
    );
    assert_eq!(fx.status_of(&rid, "t1"), "running");
}

/// Clean stalled → abandoned (pending), exit 0.
#[test]
fn verifier_clean_stalled_worktree_is_abandoned() {
    let fx = Fx::new("clean");
    let (rid, _wts) = fx.stalled_run(&["t1"]);
    let a1 = fx.abandon(&rid);
    assert_eq!(a1.status.code(), Some(0), "{a1:?}");
    let a2 = fx.abandon(&rid);
    assert_eq!(a2.status.code(), Some(0), "stderr:\n{}", err(&a2));
    assert_eq!(
        fx.status_of(&rid, "t1"),
        "pending",
        "clean stalled must be abandoned"
    );
}

/// Mixed run: clean t1 is still abandoned, dirty t2 kept, corrupt t3 kept, and
/// the scan exits 3 because of t3 alone.
#[test]
fn verifier_mixed_run_abandons_clean_and_exits_3_for_unreadable() {
    let fx = Fx::new("mixed");
    let (rid, wts) = fx.stalled_run(&["t1", "t2", "t3"]);
    std::fs::write(wts[1].join("wip.rs"), "fn x(\n").unwrap();
    std::fs::write(wts[2].join(".git").join("index"), b"garbage").unwrap();
    let a1 = fx.abandon(&rid);
    assert_eq!(a1.status.code(), Some(0), "{a1:?}");
    let a2 = fx.abandon(&rid);
    let e = err(&a2);
    assert_eq!(a2.status.code(), Some(3), "stderr:\n{e}");
    assert_eq!(fx.status_of(&rid, "t1"), "pending", "{e}");
    assert_eq!(fx.status_of(&rid, "t2"), "running", "{e}");
    assert_eq!(fx.status_of(&rid, "t3"), "running", "{e}");
    assert!(
        e.contains("t3") && e.contains("uncommitted-work-undetermined"),
        "{e}"
    );
}

/// Mixed dirty + clean (no undetermined) → exit 0.
#[test]
fn verifier_mixed_dirty_and_clean_exits_0() {
    let fx = Fx::new("mixed0");
    let (rid, wts) = fx.stalled_run(&["t1", "t2"]);
    std::fs::write(wts[1].join("wip.rs"), "fn x(\n").unwrap();
    let a1 = fx.abandon(&rid);
    assert_eq!(a1.status.code(), Some(0), "{a1:?}");
    let a2 = fx.abandon(&rid);
    assert_eq!(a2.status.code(), Some(0), "stderr:\n{}", err(&a2));
    assert_eq!(fx.status_of(&rid, "t1"), "pending");
    assert_eq!(fx.status_of(&rid, "t2"), "running");
}
