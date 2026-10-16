#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro: `condukt state release-task --run X` ignores --run. Run B can
//! release a task claim held by run A, and reports "released ... for run 'B'".

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Fx {
    base: PathBuf,
    repo: PathBuf,
    home: PathBuf,
}

impl Drop for Fx {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

fn git(dir: &Path, args: &[&str]) {
    let o = Command::new("git").args(args).current_dir(dir).output().unwrap();
    assert!(o.status.success(), "git {args:?}: {o:?}");
}

impl Fx {
    fn new(tag: &str) -> Self {
        let base = std::env::temp_dir().join(format!("condukt-relrun-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let repo = base.join("repo");
        let home = base.join("home");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["config", "user.email", "t@t.t"]);
        git(&repo, &["config", "user.name", "t"]);
        std::fs::write(repo.join("a.txt"), "a\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-q", "-m", "init"]);
        Self { base, repo, home }
    }
    fn c(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_condukt"))
            .args(args)
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env("CONDUKT_WORKTREE_BASE", self.base.join("wtb"))
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .output()
            .unwrap()
    }
}

#[test]
#[ignore = "RED repro: condukt state release-task ignores --run"]
fn release_task_by_another_run_does_not_release_the_holders_claim() {
    let f = Fx::new("other-run");
    let o = f.c(&["state", "claim-task", "--run", "run-A", "--hashkey", "hk-held"]);
    assert!(o.status.success(), "apparatus: claim failed: {o:?}");
    assert_eq!(f.c(&["state", "is-claimed", "--hashkey", "hk-held"]).status.code(), Some(0));

    let _ = f.c(&["state", "release-task", "--run", "run-B", "--hashkey", "hk-held"]);

    let still = f.c(&["state", "is-claimed", "--hashkey", "hk-held"]);
    assert_eq!(
        still.status.code(),
        Some(0),
        "run-B released run-A's claim via release-task --run run-B: {still:?}"
    );
}
