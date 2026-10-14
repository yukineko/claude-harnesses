// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro for backlog cf70f785: `condukt state set --status verified
//! --worktree W` runs the F->P oracle (`tdd oracle --task <id>`) in the
//! PREVIOUSLY recorded worktree (or, with none recorded, the session cwd)
//! instead of in the `W` passed in the same call.
//!
//! Fixture: real binary, sandboxed `$HOME` (no plugin cache, so `tdd` is
//! resolved from `$PATH`), a fake `tdd` on `$PATH` that reports a valid
//! fail-to-pass oracle (exit 0) ONLY when its cwd contains the marker file
//! `.fp-ok`, and exits 1 everywhere else. The marker lives only in `W`.
//!
//! * RED: no worktree previously recorded; `state set --status verified
//!   --worktree W` must succeed because the oracle in `W` is valid.
//! * control: same, but `W` recorded first by a separate `state set --status
//!   done --worktree W` — the documented workaround — succeeds today, proving
//!   the fake `tdd` and the decomposition are wired correctly.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_condukt")
}

const FAKE_TDD: &str = r#"#!/bin/sh
if [ "$1" = "oracle" ]; then
  if [ -f .fp-ok ]; then
    echo '{"valid_fp_oracle":true,"transition":"fail_to_pass"}'
    exit 0
  fi
  echo '{"valid_fp_oracle":false,"transition":"unknown"}'
  exit 1
fi
exit 0
"#;

struct Fixture {
    base: PathBuf,
    repo: PathBuf,
    home: PathBuf,
    fakebin: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
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

impl Fixture {
    fn new(tag: &str) -> Self {
        use std::os::unix::fs::PermissionsExt;
        let mut base = std::env::temp_dir();
        base.push(format!("condukt-cf70f785-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let repo = base.join("repo");
        let home = base.join("home");
        let fakebin = base.join("fakebin");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&fakebin).unwrap();
        let tdd = fakebin.join("tdd");
        std::fs::write(&tdd, FAKE_TDD).unwrap();
        std::fs::set_permissions(&tdd, std::fs::Permissions::from_mode(0o755)).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["config", "user.email", "t@t.t"]);
        git(&repo, &["config", "user.name", "t"]);
        git(&repo, &["config", "commit.gpgsign", "false"]);
        std::fs::write(repo.join("base.txt"), "base\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-q", "-m", "init"]);
        Self {
            base,
            repo,
            home,
            fakebin,
        }
    }

    fn condukt(&self, args: &[&str]) -> Output {
        let path = format!("{}:/usr/bin:/bin", self.fakebin.display());
        Command::new(bin())
            .args(args)
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env("PATH", path)
            .env("CLAUDE_CODE_SESSION_ID", "sess-cf70f785")
            .env_remove("CONDUKT_DISABLE")
            .output()
            .expect("spawn condukt")
    }

    /// Init a run whose single task is `kind: fix` (so the F->P oracle gate
    /// applies) and create its worktree `W` carrying the `.fp-ok` marker.
    fn setup(&self, run: &str) -> PathBuf {
        let dec = self.base.join(format!("{run}-dec.json"));
        std::fs::write(
            &dec,
            r#"{"goal":"g","tasks":[{"id":"t1","title":"fix x","kind":"fix","reproduction_tests":"cargo test x","touched_files":["x.txt"],"deps":[],"class":"parallel","done_criteria":"d"}]}"#,
        )
        .unwrap();
        let out = self.condukt(&[
            "state",
            "init",
            "--run",
            run,
            "--file",
            dec.to_str().unwrap(),
        ]);
        assert!(out.status.success(), "init: {out:?}");
        let wt = self.base.join(format!("wt-{run}"));
        git(
            &self.repo,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                &format!("b-{run}"),
                wt.to_str().unwrap(),
            ],
        );
        std::fs::write(wt.join(".fp-ok"), "").unwrap();
        // Sanity: the fake tdd answers valid in W and invalid in the main tree.
        let in_wt = Command::new(self.fakebin.join("tdd"))
            .args(["oracle", "--task", "t1"])
            .current_dir(&wt)
            .status()
            .unwrap();
        assert!(in_wt.success(), "fixture: fake tdd must pass in W");
        let in_repo = Command::new(self.fakebin.join("tdd"))
            .args(["oracle", "--task", "t1"])
            .current_dir(&self.repo)
            .status()
            .unwrap();
        assert!(!in_repo.success(), "fixture: fake tdd must fail outside W");
        wt
    }
}

#[test]
#[ignore = "backlog cf70f785: RED repro"]
fn verified_with_worktree_runs_oracle_in_the_passed_worktree() {
    let fx = Fixture::new("red");
    let wt = fx.setup("r1");
    let out = fx.condukt(&[
        "state",
        "set",
        "--run",
        "r1",
        "--task",
        "t1",
        "--status",
        "verified",
        "--worktree",
        wt.to_str().unwrap(),
    ]);
    assert!(
        out.status.success(),
        "state set --status verified --worktree W must consult the oracle in W \
         (where it is valid), not in the session cwd: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn control_worktree_recorded_first_then_verified_passes() {
    let fx = Fixture::new("control");
    let wt = fx.setup("r2");
    let out = fx.condukt(&[
        "state",
        "set",
        "--run",
        "r2",
        "--task",
        "t1",
        "--status",
        "done",
        "--worktree",
        wt.to_str().unwrap(),
    ]);
    assert!(out.status.success(), "done: {out:?}");
    let out = fx.condukt(&[
        "state", "set", "--run", "r2", "--task", "t1", "--status", "verified",
    ]);
    assert!(
        out.status.success(),
        "control: with W recorded the oracle runs in W and passes: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
}
