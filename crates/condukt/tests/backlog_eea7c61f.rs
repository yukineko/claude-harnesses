// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Independent black-box tests for backlog eea7c61f (user ruling): when
//! `condukt worktree reap` removes a worktree whose branch is merged into
//! main, it also deletes that branch with the SAFE delete (`git branch -d`).
//!
//! - merged branch of a reaped worktree: gone afterwards;
//! - unmerged branch: kept (guard: passes on the pre-feature code too);
//! - unrelated branches (no worktree / worktree not reaped): never touched
//!   (guard: passes on the pre-feature code too);
//! - a branch-delete failure is surfaced (non-zero exit or a reported line),
//!   never reported as plain success.
//!
//! Real git, real built binary, temp `$HOME`, unique temp root per test.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BUCKET: &str = "session-worktrees";

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_condukt")
}

fn run_git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
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

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

struct Fx {
    base: PathBuf,
    repo: PathBuf,
    wt_base: PathBuf,
    home: PathBuf,
}

impl Fx {
    fn new(tag: &str) -> Self {
        let base =
            std::env::temp_dir().join(format!("condukt-eea7c61f-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let base = base.canonicalize().unwrap();
        let repo = base.join("repo");
        let wt_base = base.join("worktrees");
        let home = base.join("home");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&wt_base).unwrap();
        std::fs::create_dir_all(home.join(".condukt").join("state")).unwrap();
        std::fs::create_dir_all(home.join(".claude").join("projects")).unwrap();
        run_git(&repo, &["init", "-q", "-b", "main"]);
        run_git(&repo, &["config", "user.email", "t@t.t"]);
        run_git(&repo, &["config", "user.name", "t"]);
        std::fs::write(repo.join("seed.txt"), "seed\n").unwrap();
        run_git(&repo, &["add", "seed.txt"]);
        run_git(&repo, &["commit", "-q", "-m", "seed"]);
        Fx {
            base,
            repo,
            wt_base,
            home,
        }
    }

    /// A worktree on branch `name` with one commit; merged into main if `merged`.
    fn worktree(&self, name: &str, merged: bool) -> PathBuf {
        let wt = self.wt_base.join(name);
        run_git(
            &self.repo,
            &["worktree", "add", "-q", wt.to_str().unwrap(), "-b", name],
        );
        std::fs::write(wt.join(format!("{name}.txt")), "w\n").unwrap();
        run_git(&wt, &["add", &format!("{name}.txt")]);
        run_git(&wt, &["commit", "-q", "-m", "work"]);
        if merged {
            run_git(&self.repo, &["merge", "-q", "--no-ff", "-m", "merge", name]);
        }
        wt
    }

    /// A merged branch with no worktree at all.
    fn bare_merged_branch(&self, name: &str) {
        run_git(&self.repo, &["branch", name, "main"]);
    }

    /// An aged-out registration naming `wt` (so reap may judge it Dead).
    fn register_aged_out(&self, file: &str, wt: &Path) {
        let p = self
            .home
            .join(".backlog")
            .join("drivers")
            .join(BUCKET)
            .join(format!("{file}.driver"));
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        let old = now() - 5000;
        let v = serde_json::json!({
            "session_id": format!("sess-{file}"),
            "pid": 1,
            "project": wt.to_string_lossy(),
            "registered_at": old,
            "heartbeat_at": old,
        });
        std::fs::write(p, serde_json::to_vec_pretty(&v).unwrap()).unwrap();
    }

    fn reap(&self) -> Output {
        Command::new(bin())
            .args(["worktree", "reap"])
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env("CONDUKT_WORKTREE_BASE", &self.wt_base)
            .env("HARNESS_PROGRESS_WINDOW_SECS", "0")
            .env_remove("CONDUKT_DISABLE")
            .output()
            .unwrap()
    }

    /// Death needs two probes a window apart (window collapsed to 0).
    fn reap_settled(&self) -> Output {
        let _ = self.reap();
        self.reap()
    }

    fn branch_exists(&self, name: &str) -> bool {
        Command::new("git")
            .args(["rev-parse", "--verify", "-q", &format!("refs/heads/{name}")])
            .current_dir(&self.repo)
            .output()
            .unwrap()
            .status
            .success()
    }
}

impl Drop for Fx {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

fn text(o: &Output) -> String {
    format!(
        "exit={:?}\nstdout:\n{}\nstderr:\n{}",
        o.status.code(),
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

#[test]
fn reaped_worktree_on_merged_branch_has_its_branch_deleted() {
    let f = Fx::new("merged");
    let wt = f.worktree("session-mrg00001", true);
    f.register_aged_out("mrg", &wt);
    let out = f.reap_settled();
    // Precondition (not the feature): the worktree really was reaped.
    assert!(
        !wt.exists(),
        "precondition: worktree reaped\n{}",
        text(&out)
    );
    assert!(out.status.success(), "reap succeeds\n{}", text(&out));
    assert!(
        !f.branch_exists("session-mrg00001"),
        "branch of a reaped, merged worktree must be deleted\n{}",
        text(&out)
    );
}

/// GUARD: passes on pre-feature code too (the branch is kept today); it
/// protects the unmerged-keep behaviour against an over-eager implementation.
#[test]
fn unmerged_branch_is_kept_and_worktree_is_kept() {
    let f = Fx::new("unmerged");
    let wt = f.worktree("session-unm00001", false);
    f.register_aged_out("unm", &wt);
    let out = f.reap_settled();
    assert!(
        wt.exists(),
        "unmerged worktree is kept as today\n{}",
        text(&out)
    );
    assert!(
        f.branch_exists("session-unm00001"),
        "unmerged branch must survive\n{}",
        text(&out)
    );
    assert!(
        run_git(&f.repo, &["log", "-1", "--format=%s", "session-unm00001"]) == "work",
        "unmerged commit intact"
    );
}

/// GUARD: passes on pre-feature code too. Only branches of worktrees reap
/// actually removed may be deleted. Includes a merged branch with no worktree,
/// a merged branch whose worktree is live (fresh heartbeat), and a merged
/// non-session branch whose worktree is not reapable, next to a reaped one.
#[test]
fn unrelated_merged_branches_are_never_touched() {
    let f = Fx::new("unrelated");
    f.bare_merged_branch("stray-merged");
    let live = f.worktree("session-liv00001", true);
    // Fresh heartbeat: reap must keep this worktree (and so its branch).
    let p = f
        .home
        .join(".backlog")
        .join("drivers")
        .join(BUCKET)
        .join("liv.driver");
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    let v = serde_json::json!({
        "session_id": "sess-liv", "pid": 1, "project": live.to_string_lossy(),
        "registered_at": now(), "heartbeat_at": now(),
    });
    std::fs::write(&p, serde_json::to_vec(&v).unwrap()).unwrap();
    let noreg = f.worktree("session-nor00001", true); // no registration at all
    let nonsession = f.worktree("feature-x", true);
    f.register_aged_out("feat", &nonsession); // registered, but not session-*
                                              // And one genuinely reaped worktree, so the run does remove something.
    let dead = f.worktree("session-ded00001", true);
    f.register_aged_out("ded", &dead);

    let out = f.reap_settled();
    assert!(
        !dead.exists(),
        "precondition: dead one reaped\n{}",
        text(&out)
    );
    for (b, w) in [
        ("stray-merged", None),
        ("session-liv00001", Some(&live)),
        ("session-nor00001", Some(&noreg)),
        ("feature-x", Some(&nonsession)),
    ] {
        assert!(
            f.branch_exists(b),
            "unrelated branch {b} must not be deleted\n{}",
            text(&out)
        );
        if let Some(w) = w {
            assert!(w.exists(), "worktree of {b} untouched\n{}", text(&out));
        }
    }
    assert!(f.branch_exists("main"));
}

/// `git branch -d` cannot delete the branch (a stale ref lock file makes the
/// ref update fail). The worktree removal still happens; the failed branch
/// delete must be visible: non-zero exit, or a reported non-summary line that
/// names the branch problem. A plain "removed ..." with exit 0 is a lie.
#[test]
fn branch_delete_failure_is_surfaced_not_silent() {
    let f = Fx::new("delfail");
    let wt = f.worktree("session-fail0001", true);
    f.register_aged_out("fail", &wt);
    // Settling probe first so the lock exists only across the removing run.
    let _ = f.reap();
    let git_dir = PathBuf::from(run_git(&f.repo, &["rev-parse", "--git-common-dir"]));
    let git_dir = if git_dir.is_absolute() {
        git_dir
    } else {
        f.repo.join(git_dir)
    };
    let lock = git_dir.join("refs/heads/session-fail0001.lock");
    std::fs::write(&lock, "").unwrap();

    let out = f.reap();
    assert!(
        !wt.exists(),
        "precondition: worktree reaped (removal is independent of branch delete)\n{}",
        text(&out)
    );
    assert!(
        f.branch_exists("session-fail0001"),
        "precondition: branch still present because delete was blocked\n{}",
        text(&out)
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let reported = combined
        .lines()
        .filter(|l| !l.starts_with("reap:"))
        .any(|l| l.to_lowercase().contains("branch"));
    assert!(
        out.status.code() != Some(0) || reported,
        "branch-delete failure must be visible (non-zero exit or a line naming the branch)\n{}",
        text(&out)
    );
}
