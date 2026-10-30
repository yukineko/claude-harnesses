// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! User ruling (2026-10-24): `.backlog` store changes are left UNCOMMITTED and
//! ride along with the next work or merge commit. Only a session that ENDS
//! with a leftover `.backlog` diff gets a single `.backlog` commit, made by
//! `condukt worktree reap`.
//!
//! Scenario: a dead registered `session-*` worktree whose branch is merged
//! into main and whose ONLY dirtiness is an uncommitted change to a tracked
//! `.backlog/` file. Reap (run until death is established, then twice more)
//! must leave exactly ONE new commit touching `.backlog` on main, main's tree
//! must contain the change, and the worktree must be gone from
//! `git worktree list`. Fixtures mirror `session_heartbeat_reap_impl.rs`.
//!
//! Written by an independent test author (CLAUDE.md 2(a)).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_condukt")
}

fn run_git(dir: &Path, args: &[&str]) -> String {
    let out = harness_core::git_env::scrub(&mut Command::new("git"))
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?} in {} failed: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

const BUCKET: &str = "session-worktrees";
const STORE: &str = ".backlog/tasks.toml";
const STORE_AFTER: &str = "# store v1: leftover from a finished session\n";

struct Fx {
    repo: PathBuf,
    wt_base: PathBuf,
    home: PathBuf,
}

impl Fx {
    fn new(tag: &str) -> Self {
        let base =
            std::env::temp_dir().join(format!("condukt-reapbl-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let base = base.canonicalize().unwrap();
        let repo = base.join("repo");
        let wt_base = base.join("worktrees");
        let home = base.join("home");
        std::fs::create_dir_all(repo.join(".backlog")).unwrap();
        std::fs::create_dir_all(&wt_base).unwrap();
        std::fs::create_dir_all(home.join(".condukt").join("state")).unwrap();
        std::fs::create_dir_all(home.join(".claude").join("projects")).unwrap();
        run_git(&repo, &["init", "-q", "-b", "main"]);
        run_git(&repo, &["config", "user.email", "t@t.t"]);
        run_git(&repo, &["config", "user.name", "t"]);
        std::fs::write(repo.join("seed.txt"), "seed\n").unwrap();
        std::fs::write(repo.join(STORE), "# store v0\n").unwrap();
        run_git(&repo, &["add", "seed.txt", STORE]);
        run_git(&repo, &["commit", "-q", "-m", "seed"]);
        Fx {
            repo,
            wt_base,
            home,
        }
    }

    /// A session-* worktree whose one commit is merged into main.
    fn merged_session_wt(&self, name: &str) -> PathBuf {
        let wt = self.wt_base.join(name);
        run_git(
            &self.repo,
            &["worktree", "add", "-q", wt.to_str().unwrap(), "-b", name],
        );
        std::fs::write(wt.join(format!("{name}.txt")), "w\n").unwrap();
        run_git(&wt, &["add", &format!("{name}.txt")]);
        run_git(&wt, &["commit", "-q", "-m", "work"]);
        run_git(&self.repo, &["merge", "-q", "--no-ff", "-m", "merge", name]);
        wt
    }

    fn write_record(&self, file: &str, sid: &str, project: &Path, hb: i64) {
        let p = self
            .home
            .join(".backlog")
            .join("drivers")
            .join(BUCKET)
            .join(format!("{file}.driver"));
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        let v = serde_json::json!({
            "session_id": sid,
            "pid": 1,
            "project": project.to_string_lossy(),
            "registered_at": hb,
            "heartbeat_at": hb,
        });
        std::fs::write(p, serde_json::to_vec_pretty(&v).unwrap()).unwrap();
    }

    fn reap(&self) -> Output {
        harness_core::git_env::scrub(&mut Command::new(bin()))
            .args(["worktree", "reap"])
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env("CONDUKT_WORKTREE_BASE", &self.wt_base)
            .env("HARNESS_PROGRESS_WINDOW_SECS", "0")
            .env("CONDUKT_DEFAULT_BRANCH", "main")
            .env_remove("CONDUKT_DISABLE")
            .output()
            .unwrap()
    }
}

#[test]
fn reap_commits_a_leftover_backlog_diff_exactly_once() {
    let f = Fx::new("once");
    let wt = f.merged_session_wt("session-leftover1");
    let base = run_git(&f.repo, &["rev-parse", "main"]);
    f.write_record("rec", "sess-dead", &wt, 0);
    std::fs::write(wt.join(STORE), STORE_AFTER).unwrap();
    // Precondition (observed): the ONLY dirtiness is the tracked store file.
    assert_eq!(
        run_git(&wt, &["status", "--porcelain=v1", "--untracked-files=all"]),
        format!("M {STORE}"),
        "fixture precondition: only the store is dirty"
    );

    // Establish death (two-probe), then twice more.
    let mut transcript = String::new();
    for i in 0..6 {
        let out = f.reap();
        let text = format!(
            "reap #{i} exit={:?}\n{}{}",
            out.status.code(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        eprintln!("{text}");
        transcript.push_str(&text);
    }

    let range = format!("{base}..main");
    let commits = run_git(&f.repo, &["log", "--format=%H", &range, "--", ".backlog"]);
    assert_eq!(
        commits.lines().count(),
        1,
        "exactly ONE new commit touching .backlog must be on main; got {commits:?}\n{transcript}"
    );
    let on_main = run_git(&f.repo, &["show", &format!("main:{STORE}")]);
    assert_eq!(
        on_main.trim(),
        STORE_AFTER.trim(),
        "main's tree must contain the leftover .backlog change\n{transcript}"
    );
    let list = run_git(&f.repo, &["worktree", "list", "--porcelain"]);
    assert!(
        !list.contains("session-leftover1"),
        "the reaped worktree must be gone from `git worktree list`:\n{list}\n{transcript}"
    );
}
