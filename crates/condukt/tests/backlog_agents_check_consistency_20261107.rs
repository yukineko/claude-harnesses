// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repros (2026-11-07): `condukt agents check` accepts inconsistent
//! overwatch output (fail-open). Only exit code and JSON `clean` are asserted.
//! Fixture style copied from backlog_agents_check_20261107.rs; overwatch is
//! faked by a shell script first on PATH.
//! Written by a disinterested author (CLAUDE.md section 2(a)).

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const RUN: &str = "run-agents";
const SESSION: &str = "sess-agents-check";

fn git(dir: &Path, args: &[&str]) {
    let out = harness_core::git_env::scrub(&mut Command::new("git"))
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("spawn git");
    assert!(out.status.success(), "git {args:?} failed: {out:?}");
}

struct Fx {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    home: PathBuf,
    bin: PathBuf,
}

impl Fx {
    fn new() -> Fx {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let repo = base.join("repo");
        let home = base.join("home");
        let bin = base.join("bin");
        for d in [&repo, &home, &bin] {
            std::fs::create_dir_all(d).unwrap();
        }
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["config", "user.email", "t@t.t"]);
        git(&repo, &["config", "user.name", "t"]);
        git(&repo, &["config", "commit.gpgsign", "false"]);
        std::fs::write(repo.join("base.txt"), "base\n").unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "seed"]);
        let fx = Fx {
            _tmp: tmp,
            repo,
            home,
            bin,
        };
        let dec = base.join("dec.json");
        std::fs::write(
            &dec,
            r#"{"goal":"g","tasks":[
              {"id":"t1","title":"a","kind":"chore","touched_files":["a.txt"],"deps":[],"class":"parallel","done_criteria":"d"}]}"#,
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

    /// Install a fake `overwatch` printing `stdout` and exiting `code`.
    fn fake_overwatch(&self, stdout: &str, code: i32) {
        let payload = self.bin.join("payload.txt");
        std::fs::write(&payload, stdout).unwrap();
        let script = self.bin.join("overwatch");
        std::fs::write(
            &script,
            format!("#!/bin/sh\ncat '{}'\nexit {code}\n", payload.display()),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn condukt(&self, args: &[&str]) -> Output {
        let path = format!(
            "{}:{}",
            self.bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_condukt"));
        harness_core::git_env::scrub(&mut cmd);
        cmd.args(args)
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env("PATH", path)
            .env("CLAUDE_CODE_SESSION_ID", SESSION)
            .env("CONDUKT_DEFAULT_BRANCH", "main")
            .env_remove("CONDUKT_DISABLE")
            .output()
            .expect("spawn condukt")
    }

    fn check(&self, extra: &[&str]) -> Output {
        let mut args = vec!["agents", "check", "--session", SESSION];
        args.extend_from_slice(extra);
        self.condukt(&args)
    }
}

fn code(o: &Output) -> i32 {
    o.status.code().unwrap_or(-1)
}

fn json(o: &Output) -> serde_json::Value {
    serde_json::from_slice(&o.stdout).unwrap_or_else(|e| panic!("stdout not JSON ({e}): {o:?}"))
}

fn body(agents: &[(&str, &str)], clean: bool) -> String {
    let items: Vec<String> = agents
        .iter()
        .map(|(id, st)| format!(r#"{{"agent_id":"{id}","state":"{st}"}}"#))
        .collect();
    format!(
        r#"{{"clean":{clean},"agents":[{}],"error":null}}"#,
        items.join(",")
    )
}

/// A: exit 1 with no unselected non-ok agent is unexplained.
#[test]
fn a_exit1_without_unselected_non_ok_is_not_clean() {
    let fx = Fx::new();
    fx.fake_overwatch(&body(&[("X", "done")], false), 1);
    let o = fx.check(&["--agent", "X"]);
    assert_ne!(code(&o), 0, "{o:?}");
    assert_eq!(json(&o)["clean"], false, "{o:?}");
}

/// B: empty set with no selection is not clean.
#[test]
fn b_empty_agent_set_is_not_clean() {
    let fx = Fx::new();
    fx.fake_overwatch(&body(&[], true), 0);
    let o = fx.check(&[]);
    assert_ne!(code(&o), 0, "{o:?}");
    assert_eq!(json(&o)["clean"], false, "{o:?}");
}

/// C: duplicate agent ids are undeterminable.
#[test]
fn c_duplicate_agent_ids_are_not_clean() {
    let fx = Fx::new();
    fx.fake_overwatch(&body(&[("X", "done"), ("X", "stalled")], true), 0);
    let o = fx.check(&["--agent", "X"]);
    assert_ne!(code(&o), 0, "{o:?}");
    assert_eq!(json(&o)["clean"], false, "{o:?}");
}

/// D (control, passes today): exit 1 explained by unselected stalled Y.
#[test]
fn d_control_unselected_stalled_is_clean() {
    let fx = Fx::new();
    fx.fake_overwatch(&body(&[("X", "done"), ("Y", "stalled")], false), 1);
    let o = fx.check(&["--agent", "X"]);
    assert_eq!(code(&o), 0, "{o:?}");
    assert_eq!(json(&o)["clean"], true, "{o:?}");
}

/// E: body clean:false disagrees with exit 0.
#[test]
fn e_body_clean_false_with_exit0_is_not_clean() {
    let fx = Fx::new();
    fx.fake_overwatch(&body(&[("X", "done")], false), 0);
    let o = fx.check(&["--agent", "X"]);
    assert_ne!(code(&o), 0, "{o:?}");
}
