//! End-to-end coverage for `condukt guard main-tree` — the machine enforcement
//! of CLAUDE.md §8 (do not share-edit the primary working tree).
//!
//! These runs are end-to-end for the *gate*: a real temp git repo with a real
//! linked worktree, real staged files, real integration markers, and the real
//! built binary, judged by its real process exit status.
//!
//! They are **not** end-to-end for the *liveness input*. A second live Claude
//! session cannot be spawned from a test, so `overwatch` and `backlog` are
//! replaced by stub executables placed first on `PATH`, which print the exact
//! documents the real tools print. That is stated here rather than implied: the
//! parsing and the decision are exercised against real process boundaries, but
//! "another session is live" is injected.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_condukt")
}

fn run_git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The directory holding the real `git`, so a stub-only `PATH` still has git.
fn git_dir_on_path() -> PathBuf {
    let out = Command::new("sh")
        .args(["-c", "command -v git"])
        .output()
        .expect("which git");
    let path = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim().to_string());
    path.parent().expect("git has a parent dir").to_path_buf()
}

struct Fixture {
    base: PathBuf,
    repo: PathBuf,
    worktree: PathBuf,
    stubs: PathBuf,
    home: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let pid = std::process::id();
        let mut base = std::env::temp_dir();
        base.push(format!("condukt-maintree-{pid}-{tag}"));
        let _ = std::fs::remove_dir_all(&base);
        let repo = base.join("repo");
        let worktree = base.join("wt");
        let stubs = base.join("stubs");
        let home = base.join("home");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&stubs).unwrap();
        std::fs::create_dir_all(&home).unwrap();

        run_git(&repo, &["init", "-q", "-b", "main"]);
        run_git(&repo, &["config", "user.email", "t@t.t"]);
        run_git(&repo, &["config", "user.name", "t"]);
        std::fs::write(repo.join("seed.txt"), "seed\n").unwrap();
        run_git(&repo, &["add", "seed.txt"]);
        run_git(&repo, &["commit", "-q", "-m", "seed"]);
        run_git(
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                worktree.to_str().unwrap(),
                "-b",
                "side",
            ],
        );

        Self {
            base,
            repo,
            worktree,
            stubs,
            home,
        }
    }

    /// Write an executable stub that prints `stdout` and exits `code`.
    ///
    /// Uses `printf` (a shell builtin) rather than `cat`, because the guard is
    /// run with a deliberately narrow `PATH` that holds only the stubs and git.
    fn stub(&self, name: &str, stdout: &str, code: i32) {
        assert!(
            !stdout.contains('\''),
            "stub payload must not contain a quote"
        );
        let path = self.stubs.join(name);
        std::fs::write(
            &path,
            format!("#!/bin/sh\nprintf '%s\\n' '{stdout}'\nexit {code}\n"),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    /// No live peer, from either input.
    fn stub_quiet(&self) {
        self.stub(
            "overwatch",
            r#"{"backlog":{"pending":0,"done":0,"deferred":0}}"#,
            0,
        );
        self.stub("backlog", "none", 0);
    }

    /// One live peer session, reported by overwatch.
    fn stub_peer(&self) {
        self.stub(
            "overwatch",
            r#"{"sessions":[{"session_id":"peer-session","leases":[],"live_count":1}]}"#,
            0,
        );
        self.stub("backlog", "none", 0);
    }

    fn stage_a_change(&self, dir: &Path) {
        std::fs::write(dir.join("edited.txt"), "change\n").unwrap();
        run_git(dir, &["add", "edited.txt"]);
    }

    /// Run the guard in `dir` with a PATH containing only the stubs and git.
    fn guard(&self, dir: &Path, envs: &[(&str, &str)]) -> Output {
        let path = format!("{}:{}", self.stubs.display(), git_dir_on_path().display());
        let mut cmd = Command::new(bin());
        cmd.args(["guard", "main-tree"])
            .current_dir(dir)
            .env("PATH", path)
            .env("HOME", &self.home)
            .env("CLAUDE_CODE_SESSION_ID", "this-session")
            .env_remove("CONDUKT_MAINTREE_OVERRIDE");
        for (k, v) in envs {
            cmd.env(k, v);
        }
        cmd.output().expect("guard runs")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

fn code(out: &Output) -> i32 {
    out.status.code().expect("the guard exits with a code")
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// The DoD observation, blocking half: primary tree + staged change + a live
/// peer (injected) => non-zero.
#[test]
fn blocks_in_the_primary_tree_when_a_peer_session_is_live() {
    let f = Fixture::new("block");
    f.stub_peer();
    f.stage_a_change(&f.repo);
    let out = f.guard(&f.repo, &[]);
    assert_eq!(code(&out), 1, "stderr: {}", stderr(&out));
    let err = stderr(&out);
    assert!(err.contains("BLOCKED"), "{err}");
    assert!(err.contains("peer-session"), "{err}");
}

/// The DoD observation, passing half: same repo, same staged change, no peer
/// => zero.
#[test]
fn passes_in_the_primary_tree_when_no_peer_session_is_live() {
    let f = Fixture::new("solo");
    f.stub_quiet();
    f.stage_a_change(&f.repo);
    let out = f.guard(&f.repo, &[]);
    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));
}

#[test]
fn passes_when_the_commit_comes_from_a_linked_worktree() {
    let f = Fixture::new("worktree");
    f.stub_peer();
    f.stage_a_change(&f.worktree);
    let out = f.guard(&f.worktree, &[]);
    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));
}

#[test]
fn passes_for_an_integration_commit_in_the_primary_tree() {
    let f = Fixture::new("merge");
    f.stub_peer();
    f.stage_a_change(&f.repo);
    // A real merge marker in the real git dir.
    let head = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(&f.repo)
        .output()
        .unwrap();
    std::fs::write(
        f.repo.join(".git").join("MERGE_HEAD"),
        String::from_utf8_lossy(&head.stdout).trim(),
    )
    .unwrap();
    let out = f.guard(&f.repo, &[]);
    assert_eq!(
        code(&out),
        0,
        "§8 permits integration in the main tree; stderr: {}",
        stderr(&out)
    );
}

#[test]
fn passes_when_nothing_is_staged() {
    let f = Fixture::new("empty");
    f.stub_peer();
    let out = f.guard(&f.repo, &[]);
    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));
}

/// A liveness input that is absent is "could not check", not "nobody is live".
#[test]
fn missing_overwatch_is_undetermined_and_blocks_with_exit_2() {
    let f = Fixture::new("noovr");
    f.stub("backlog", "none", 0);
    f.stage_a_change(&f.repo);
    let out = f.guard(&f.repo, &[]);
    assert_eq!(code(&out), 2, "stderr: {}", stderr(&out));
    assert!(stderr(&out).contains("UNDETERMINED"), "{}", stderr(&out));
}

/// A liveness input that ran but failed is also "could not check".
#[test]
fn a_failing_liveness_input_is_undetermined_and_blocks() {
    let f = Fixture::new("ovrfail");
    f.stub("overwatch", "boom", 3);
    f.stub("backlog", "none", 0);
    f.stage_a_change(&f.repo);
    let out = f.guard(&f.repo, &[]);
    assert_eq!(code(&out), 2, "stderr: {}", stderr(&out));
}

/// The backlog run lock is the second, independent liveness input.
#[test]
fn an_active_backlog_lock_held_by_another_session_blocks() {
    let f = Fixture::new("locked");
    f.stub(
        "overwatch",
        r#"{"backlog":{"pending":0,"done":0,"deferred":0}}"#,
        0,
    );
    f.stub(
        "backlog",
        r#"{"session_id":"other-session","pid":99,"project":"/repo"}"#,
        0,
    );
    f.stage_a_change(&f.repo);
    let out = f.guard(&f.repo, &[]);
    assert_eq!(code(&out), 1, "stderr: {}", stderr(&out));
    assert!(stderr(&out).contains("other-session"), "{}", stderr(&out));
}

#[test]
fn a_blank_override_does_not_unlock_the_gate() {
    let f = Fixture::new("blankovr");
    f.stub_peer();
    f.stage_a_change(&f.repo);
    let out = f.guard(&f.repo, &[("CONDUKT_MAINTREE_OVERRIDE", "   ")]);
    assert_eq!(code(&out), 1, "stderr: {}", stderr(&out));
}

#[test]
fn an_override_with_a_reason_unlocks_and_prints_the_reason() {
    let f = Fixture::new("ovr");
    f.stub_peer();
    f.stage_a_change(&f.repo);
    let out = f.guard(
        &f.repo,
        &[("CONDUKT_MAINTREE_OVERRIDE", "hand-integrating a hotfix")],
    );
    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));
    let err = stderr(&out);
    assert!(err.contains("BYPASSED"), "{err}");
    assert!(err.contains("hand-integrating a hotfix"), "{err}");
    assert!(
        err.contains("peer-session"),
        "the finding must still be shown: {err}"
    );
}

/// The gate does not create any skip file (CLAUDE.md §5: a project-root shared
/// skip file would be consumed once and wave through another session's gate).
#[test]
fn the_gate_writes_no_skip_file() {
    let f = Fixture::new("noskip");
    f.stub_peer();
    f.stage_a_change(&f.repo);
    let before: Vec<_> = std::fs::read_dir(&f.repo)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    let _ = f.guard(
        &f.repo,
        &[("CONDUKT_MAINTREE_OVERRIDE", "checking for side effects")],
    );
    let after: Vec<_> = std::fs::read_dir(&f.repo)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(before, after, "the guard must leave no marker behind");
}

/// `--json` reports the verdict the checks reached, not a rewritten one: an
/// override exits 0 while the recorded verdict stays a violation.
#[test]
fn json_reports_the_real_verdict_under_an_override() {
    let f = Fixture::new("json");
    f.stub_peer();
    f.stage_a_change(&f.repo);
    let path = format!("{}:{}", f.stubs.display(), git_dir_on_path().display());
    let out = Command::new(bin())
        .args(["guard", "main-tree", "--json"])
        .current_dir(&f.repo)
        .env("PATH", path)
        .env("HOME", &f.home)
        .env("CLAUDE_CODE_SESSION_ID", "this-session")
        .env("CONDUKT_MAINTREE_OVERRIDE", "documented bypass")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    let doc: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("--json emits one JSON document");
    assert_eq!(doc["verdict"], "violation");
    assert_eq!(doc["override_reason"], "documented bypass");
    assert_eq!(doc["exit_code"], 0);
}
