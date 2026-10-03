// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! backlog 2821738c: `condukt state cancel` refuses to move a `verified` task to
//! `cancelled` ("already verified and cannot be cancelled"), but
//! `condukt state set --status cancelled` performs the very same transition and
//! exits 0. The "verified is terminal" guard lives on one of two paths.
//!
//! The control (`state_cancel_refuses_verified`) pins the guarded path so the
//! repro below is not vacuous: if `state cancel` ever stopped refusing, the
//! property would be moot rather than fixed.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_condukt")
}

fn run_git(dir: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("spawn git")
        .status
        .success();
    assert!(ok, "git {args:?} failed");
}

struct Fixture {
    repo: PathBuf,
    home: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let pid = std::process::id();
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let base = std::env::temp_dir().join(format!("condukt-2821738c-{pid}-{tag}-{nonce}"));
        let repo = base.join("repo");
        let home = base.join("home");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        run_git(&repo, &["init", "-q", "-b", "main"]);
        run_git(&repo, &["config", "user.email", "t@t.t"]);
        run_git(&repo, &["config", "user.name", "t"]);
        std::fs::write(repo.join("base.txt"), "base\n").unwrap();
        run_git(&repo, &["add", "."]);
        run_git(&repo, &["commit", "-q", "-m", "init"]);
        Self { repo, home }
    }

    fn condukt(&self, args: &[&str]) -> Output {
        Command::new(bin())
            .args(args)
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env("CLAUDE_CODE_SESSION_ID", "sess-2821738c")
            .output()
            .expect("spawn condukt")
    }

    /// A one-task run whose task is driven to `verified` (a chore task with no
    /// declared kind: the F->P oracle does not apply).
    fn verified_run(&self, run: &str) {
        let decomp = self.repo.join("decomp.json");
        std::fs::write(
            &decomp,
            r#"{"goal":"g","tasks":[{"id":"t1","title":"t","touched_files":["a.txt"],"deps":[],"class":"parallel","done_criteria":"d"}]}"#,
        )
        .unwrap();
        let out = self.condukt(&[
            "state",
            "init",
            "--run",
            run,
            "--file",
            decomp.to_str().unwrap(),
        ]);
        assert!(out.status.success(), "state init failed: {out:?}");
        let out = self.condukt(&[
            "state", "set", "--run", run, "--task", "t1", "--status", "verified",
        ]);
        assert!(
            out.status.success(),
            "precondition: t1 must reach verified: {out:?}"
        );
        assert_eq!(self.status(run), "verified", "precondition");
    }

    fn status(&self, run: &str) -> String {
        let out = self.condukt(&["state", "show", "--run", run]);
        assert!(out.status.success(), "state show failed: {out:?}");
        let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        v["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["id"] == "t1")
            .unwrap()["status"]
            .as_str()
            .unwrap()
            .to_string()
    }
}

#[test]
fn state_cancel_refuses_verified() {
    let f = Fixture::new("cancel");
    f.verified_run("r1");
    let out = f.condukt(&["state", "cancel", "--run", "r1", "--task", "t1"]);
    assert!(
        !out.status.success(),
        "control: `state cancel` must refuse a verified task: {out:?}"
    );
    assert_eq!(f.status("r1"), "verified");
}

#[test]
#[ignore = "backlog 2821738c: open defect, remove ignore when fixed"]
fn state_set_cancelled_refuses_verified_like_state_cancel() {
    let f = Fixture::new("set");
    f.verified_run("r1");
    let out = f.condukt(&[
        "state",
        "set",
        "--run",
        "r1",
        "--task",
        "t1",
        "--status",
        "cancelled",
    ]);
    let after = f.status("r1");
    assert!(
        !out.status.success() && after == "verified",
        "`state set --status cancelled` rewrote a verified task (exit {:?}, status now {after}); \
         `state cancel` refuses the same transition",
        out.status.code()
    );
}
