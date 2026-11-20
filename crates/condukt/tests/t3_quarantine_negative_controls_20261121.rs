// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Negative controls for backlog 4503c90d (quarantine of a part-way-removed
//! worktree). The fix must quarantine-move a directory ONLY when it is proven
//! to be a condukt task worktree that lost its `.git`. "No .git, not listed by
//! `git worktree list`" is satisfied by ANY plain directory, so an over-broad
//! fix would rename arbitrary directories. These tests pin today's behaviour:
//! `worktree remove --path` / `discard` on a non-worktree directory is refused,
//! the directory stays where it is, and no `*.quarantine` directory appears.
//!
//! DOES NOT PROVE: that the part-way-removed shape IS quarantined (that is the
//! RED in backlog_20261117_worktree_partial_remove_leftover_discard.rs); only
//! that things which are not that shape are not moved.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const RUN: &str = "run-neg";

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

struct Fx {
    base: PathBuf,
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    home: PathBuf,
    wt_base: PathBuf,
}

impl Fx {
    fn new() -> Fx {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let repo = base.join("repo");
        let home = base.join("home");
        let wt_base = base.join("wtbase");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&wt_base).unwrap();
        std::fs::create_dir_all(home.join(".claude").join("projects")).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["config", "user.email", "t@t.t"]);
        git(&repo, &["config", "user.name", "t"]);
        git(&repo, &["config", "commit.gpgsign", "false"]);
        std::fs::write(repo.join("base.txt"), "base\n").unwrap();
        std::fs::create_dir_all(repo.join("subdir")).unwrap();
        std::fs::write(repo.join("subdir").join("marker.txt"), "keep me\n").unwrap();
        git(&repo, &["add", "base.txt", "subdir/marker.txt"]);
        git(&repo, &["commit", "-q", "-m", "seed"]);
        let fx = Fx {
            base: base.clone(),
            _tmp: tmp,
            repo,
            home,
            wt_base,
        };
        let dec = base.join("dec.json");
        std::fs::write(
            &dec,
            r#"{"goal":"g","tasks":[
              {"id":"t2","title":"a","kind":"chore","touched_files":["specs/a.toml"],"deps":[],"class":"parallel","done_criteria":"d"}]}"#,
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

    fn condukt(&self, args: &[&str]) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_condukt"));
        harness_core::git_env::scrub(&mut cmd);
        cmd.args(args)
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env("CLAUDE_CODE_SESSION_ID", "sess-t3-negative")
            .env("CONDUKT_DEFAULT_BRANCH", "main")
            .env("CONDUKT_WORKTREE_BASE", &self.wt_base)
            .env("HARNESS_PROGRESS_WINDOW_SECS", "0")
            .env_remove("CONDUKT_DISABLE")
            .output()
            .expect("spawn condukt")
    }

    fn remove_path(&self, p: &Path) -> Output {
        self.condukt(&["worktree", "remove", "--path", p.to_str().unwrap()])
    }

    /// Every directory whose name ends in `.quarantine` anywhere under the tmp tree.
    fn quarantine_dirs(&self) -> Vec<PathBuf> {
        let mut found = Vec::new();
        let mut stack = vec![self.base.clone()];
        while let Some(d) = stack.pop() {
            let Ok(rd) = std::fs::read_dir(&d) else {
                continue;
            };
            for e in rd.flatten() {
                let p = e.path();
                let Ok(ft) = e.file_type() else { continue };
                if ft.is_dir() {
                    if p.file_name()
                        .is_some_and(|n| n.to_string_lossy().ends_with(".quarantine"))
                    {
                        found.push(p.clone());
                    }
                    stack.push(p);
                }
            }
        }
        found
    }
}

fn describe(out: &Output) -> String {
    format!(
        "exit={:?}\nstdout: {}\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn assert_refused_and_untouched(fx: &Fx, out: &Output, dir: &Path, marker: &Path) {
    let d = describe(out);
    assert!(
        !out.status.success(),
        "expected refusal for non-worktree dir {}\n{d}",
        dir.display()
    );
    assert!(
        dir.exists() && marker.exists(),
        "directory/marker moved or deleted: {} / {}\n{d}",
        dir.display(),
        marker.display()
    );
    let q = fx.quarantine_dirs();
    assert!(q.is_empty(), "quarantine dir(s) created: {q:?}\n{d}");
}

/// N1: a tracked subdirectory of the primary repo is not a worktree.
#[test]
fn n1_remove_primary_repo_subdir_is_refused() {
    let fx = Fx::new();
    let dir = fx.repo.join("subdir");
    let marker = dir.join("marker.txt");
    let out = fx.remove_path(&dir);
    assert_refused_and_untouched(&fx, &out, &dir, &marker);
}

/// N2a: a plain directory outside the worktree base, never registered.
#[test]
fn n2a_remove_plain_dir_outside_base_is_refused() {
    let fx = Fx::new();
    let dir = fx.base.join("plain-outside");
    std::fs::create_dir_all(&dir).unwrap();
    let marker = dir.join("marker.txt");
    std::fs::write(&marker, "keep me\n").unwrap();
    let out = fx.remove_path(&dir);
    assert_refused_and_untouched(&fx, &out, &dir, &marker);
}

/// N2b: a plain directory under the worktree base that condukt never created.
#[test]
fn n2b_remove_plain_dir_under_base_is_refused() {
    let fx = Fx::new();
    let dir = fx.wt_base.join("plain-inside");
    std::fs::create_dir_all(&dir).unwrap();
    let marker = dir.join("marker.txt");
    std::fs::write(&marker, "keep me\n").unwrap();
    let out = fx.remove_path(&dir);
    assert_refused_and_untouched(&fx, &out, &dir, &marker);
}

/// N2c: discard of a task whose registered worktree path is a plain directory
/// (no .git, not a git worktree, never created by condukt).
#[test]
fn n2c_discard_task_pointing_at_plain_dir_is_refused() {
    let fx = Fx::new();
    let dir = fx.wt_base.join("plain-registered");
    std::fs::create_dir_all(&dir).unwrap();
    let marker = dir.join("marker.txt");
    std::fs::write(&marker, "keep me\n").unwrap();
    let out = fx.condukt(&[
        "state",
        "set",
        "--run",
        RUN,
        "--task",
        "t2",
        "--status",
        "done",
        "--worktree",
        dir.to_str().unwrap(),
        "--branch",
        "b-t2",
    ]);
    assert!(out.status.success(), "state set: {}", describe(&out));
    let out = fx.condukt(&["worktree", "discard", "--run", RUN, "--task", "t2"]);
    assert_refused_and_untouched(&fx, &out, &dir, &marker);
}

/// N3: a live, intact, registered worktree addressed through a symlink alias
/// must never be quarantine-moved (removal via git, if any, is not a move).
#[cfg(unix)]
#[test]
fn n3_intact_worktree_via_symlink_alias_is_not_quarantined() {
    let fx = Fx::new();
    let out = fx.condukt(&[
        "worktree", "create", "--run", RUN, "--topic", "t2", "--branch", "b-t2",
    ]);
    assert!(out.status.success(), "worktree create: {}", describe(&out));
    let wt = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
    let alias = fx.base.join("alias-to-wt");
    std::os::unix::fs::symlink(&wt, &alias).unwrap();
    let out = fx.remove_path(&alias);
    let d = describe(&out);
    let q = fx.quarantine_dirs();
    assert!(q.is_empty(), "quarantine dir(s) created: {q:?}\n{d}");
    if !out.status.success() {
        assert!(wt.exists(), "refused but worktree vanished: {d}");
    }
}
