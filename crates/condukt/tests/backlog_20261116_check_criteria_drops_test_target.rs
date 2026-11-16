// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro (observed 2026-11-16, main 0be4629b0, run-20261116-164820-25374
//! t1): done_criteria `cargo test -p condukt --test worktree_remove_orphan_leftover
//! -- --include-ignored exits 0` made `condukt state check-criteria` emit
//! `cmd = ["cargo","test","-p","condukt"]` (whole suite). `--test <name>` and
//! `-- --include-ignored` were dropped. Cause: verify/mechanical.rs
//! `mechanical_cmd` prose fallback rebuilds only `cargo test [-p X]`.
//!
//! Pinned (RED): the executed argv must contain `--test` followed by the named
//! target. Observed via a fake `cargo` on PATH that records its argv (nothing
//! heavy runs). Control (not ignored): the fake cargo is really invoked.
//!
//! Does NOT prove: backtick-form criteria; `--include-ignored` passthrough
//! (only the `--test` target is pinned); non-cargo runners.
//!
//! Written as a RED repro for the backlog row.

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_condukt")
}

struct Fx {
    base: PathBuf,
    home: PathBuf,
    cwd: PathBuf,
    wt: PathBuf,
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

impl Fx {
    fn new(tag: &str) -> Fx {
        let base = std::env::temp_dir().join(format!("condukt-dropt-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let cwd = base.join("cwd");
        let home = base.join("home");
        let wt = base.join("wt");
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        git(&cwd, &["init", "-q", "-b", "main"]);
        git(&cwd, &["config", "user.email", "t@t.t"]);
        git(&cwd, &["config", "user.name", "t"]);
        git(&cwd, &["config", "commit.gpgsign", "false"]);
        std::fs::write(cwd.join("base.txt"), "base\n").unwrap();
        git(&cwd, &["add", "."]);
        git(&cwd, &["commit", "-q", "-m", "init"]);
        git(
            &cwd,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "condukt/nw",
                wt.to_str().unwrap(),
            ],
        );
        Fx {
            base,
            home,
            cwd,
            wt,
        }
    }

    fn condukt(&self, args: &[&str]) -> Output {
        common::prime_if_running(args, &self.home, "sess-dropt");
        harness_core::git_env::scrub(&mut Command::new(bin()))
            .args(args)
            .current_dir(&self.cwd)
            .env("HOME", &self.home)
            .env(
                "CONDUKT_TESTSCOPE_LEDGER",
                common::testscope_ledger(&self.home),
            )
            .env("CLAUDE_CODE_SESSION_ID", "sess-dropt")
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

    fn run_with(&self) -> String {
        let dec = self.base.join("dec.json");
        std::fs::write(
            &dec,
            serde_json::json!({
                "goal": "g",
                "tasks": [{
                    "id": "t1", "title": "t", "touched_files": ["work.txt"],
                    "deps": [], "class": "parallel",
                    "done_criteria": "cargo test -p condukt --test worktree_remove_orphan_leftover -- --include-ignored exits 0",
                    "is_behavioral": false
                }]
            })
            .to_string(),
        )
        .unwrap();
        self.ok(&[
            "state",
            "init",
            "--run",
            "r",
            "--file",
            dec.to_str().unwrap(),
        ]);
        self.ok(&[
            "state",
            "set",
            "--run",
            "r",
            "--task",
            "t1",
            "--status",
            "running",
            "--worktree",
            self.wt.to_str().unwrap(),
        ]);
        "r".to_string()
    }

    /// Put a fake `cargo` first on PATH that records argv and exits 0.
    fn check(&self, run: &str) -> Output {
        use std::os::unix::fs::PermissionsExt;
        let bindir = self.base.join("fakebin");
        std::fs::create_dir_all(&bindir).unwrap();
        let fake = bindir.join("cargo");
        std::fs::write(
            &fake,
            format!(
                "#!/bin/sh\necho \"$@\" > {}\nexit 0\n",
                self.base.join("argv.txt").display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = format!(
            "{}:{}",
            bindir.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        common::prime_if_running(&["state"], &self.home, "sess-dropt");
        harness_core::git_env::scrub(&mut Command::new(bin()))
            .args(["state", "check-criteria", "--run", run, "--task", "t1"])
            .current_dir(&self.cwd)
            .env("HOME", &self.home)
            .env("PATH", path)
            .env(
                "CONDUKT_TESTSCOPE_LEDGER",
                common::testscope_ledger(&self.home),
            )
            .env("CLAUDE_CODE_SESSION_ID", "sess-dropt")
            .env("CONDUKT_AUTONOMOUS", "0")
            .env_remove("CONDUKT_DISABLE")
            .stdin(std::process::Stdio::null())
            .output()
            .expect("spawn condukt")
    }
}

#[test]
fn control_fake_cargo_is_invoked() {
    let fx = Fx::new("ctl");
    let run = fx.run_with();
    let out = fx.check(&run);
    assert!(
        fx.base.join("argv.txt").exists(),
        "control: fake cargo must be executed; stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
#[ignore = "backlog row: check-criteria drops `--test <name>` and `-- --include-ignored` from prose done_criteria and runs the whole crate suite (verify/mechanical.rs prose fallback)"]
fn check_criteria_keeps_named_test_target() {
    let fx = Fx::new("red");
    let run = fx.run_with();
    let out = fx.check(&run);
    let argv = std::fs::read_to_string(fx.base.join("argv.txt")).unwrap_or_default();
    assert!(
        argv.contains("--test worktree_remove_orphan_leftover"),
        "executed argv dropped the named --test target; argv={argv:?} stdout={}",
        String::from_utf8_lossy(&out.stdout)
    );
}
