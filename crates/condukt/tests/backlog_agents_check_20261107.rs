// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repros (2026-11-07): `condukt agents check` does not exist yet.
//!
//! Pinned CLI: `condukt agents check --session <sid> [--run <rid>]
//! [--agent <id>]... [--stall-secs N]`. It shells out to
//! `overwatch subagents --session <sid> --stall-secs N --json` (looked up on
//! PATH) and prints `{"clean":bool,"blocking":[{"agent_id","state",...}]}`.
//! Exit 0 only if overwatch exited 0, its stdout parsed, and every SELECTED
//! agent is done|progressing. Anything undeterminable is non-zero.
//!
//! overwatch is FAKED by a shell script placed first on PATH, so these tests do
//! not depend on the real overwatch implementation. Only exit codes and JSON
//! fields are asserted (no prose).
//!
//! Written by a disinterested author (CLAUDE.md section 2(a)): no fix exists.

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
    /// When false the fake overwatch dir is not put on PATH.
    overwatch_on_path: bool,
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
            overwatch_on_path: true,
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
        let path = if self.overwatch_on_path {
            format!(
                "{}:{}",
                self.bin.display(),
                std::env::var("PATH").unwrap_or_default()
            )
        } else {
            // No overwatch anywhere: a minimal system PATH only.
            "/usr/bin:/bin".to_string()
        };
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

fn ow(agents: &[(&str, &str)]) -> String {
    let items: Vec<String> = agents
        .iter()
        .map(|(id, st)| format!(r#"{{"agent_id":"{id}","state":"{st}"}}"#))
        .collect();
    let clean = agents
        .iter()
        .all(|(_, s)| *s == "done" || *s == "progressing");
    format!(r#"{{"clean":{clean},"agents":[{}]}}"#, items.join(","))
}

fn code(o: &Output) -> i32 {
    o.status.code().unwrap_or(-1)
}

fn json(o: &Output) -> serde_json::Value {
    serde_json::from_slice(&o.stdout).unwrap_or_else(|e| panic!("stdout not JSON ({e}): {o:?}"))
}

/// A non-zero exit must be a real verdict (JSON `clean:false`), not a clap
/// "unknown subcommand" usage error that is non-zero for the wrong reason.
fn assert_undetermined_json(o: &Output) {
    assert_eq!(json(o)["clean"], false, "{o:?}");
}

fn blocking(v: &serde_json::Value) -> Vec<(String, String)> {
    let mut b: Vec<(String, String)> = v["blocking"]
        .as_array()
        .unwrap_or_else(|| panic!("blocking not an array: {v}"))
        .iter()
        .map(|e| {
            (
                e["agent_id"].as_str().unwrap().to_string(),
                e["state"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    b.sort();
    b
}

fn set_running(fx: &Fx, extra: &[&str]) {
    let mut args = vec![
        "state", "set", "--run", RUN, "--task", "t1", "--status", "running",
    ];
    args.extend_from_slice(extra);
    let o = fx.condukt(&args);
    assert!(o.status.success(), "state set: {o:?}");
}

/// Control (passes today): the recording half of the contract already works.
#[test]
fn control_state_set_agent_id_is_recorded() {
    let fx = Fx::new();
    set_running(
        &fx,
        &["--agent-id", "agent-X", "--verifier-agent-id", "ver-V"],
    );
    let o = fx.condukt(&["state", "show", "--run", RUN]);
    assert!(o.status.success(), "state show: {o:?}");
    let s = String::from_utf8_lossy(&o.stdout);
    assert!(s.contains("agent-X"), "agent_id not visible: {s}");
    assert!(s.contains("ver-V"), "verifier_agent_id not visible: {s}");
}

#[test]
fn t1_all_done_or_progressing_is_clean() {
    let fx = Fx::new();
    fx.fake_overwatch(&ow(&[("a1", "done"), ("a2", "progressing")]), 0);
    let o = fx.check(&[]);
    assert_eq!(code(&o), 0, "{o:?}");
    let v = json(&o);
    assert_eq!(v["clean"], true);
    assert!(blocking(&v).is_empty());
}

#[test]
fn t2_stalled_and_ended_undelivered_block() {
    let fx = Fx::new();
    fx.fake_overwatch(
        &ow(&[
            ("a1", "done"),
            ("a2", "stalled"),
            ("a3", "ended_undelivered"),
        ]),
        1,
    );
    let o = fx.check(&[]);
    assert_eq!(code(&o), 1, "{o:?}");
    let v = json(&o);
    assert_eq!(v["clean"], false);
    assert_eq!(
        blocking(&v),
        vec![
            ("a2".to_string(), "stalled".to_string()),
            ("a3".to_string(), "ended_undelivered".to_string())
        ]
    );
}

#[test]
fn t3_overwatch_missing_is_nonzero() {
    let mut fx = Fx::new();
    fx.overwatch_on_path = false;
    let o = fx.check(&[]);
    assert_ne!(code(&o), 0, "{o:?}");
    assert_undetermined_json(&o);
}

#[test]
fn t4_garbage_output_is_nonzero() {
    let fx = Fx::new();
    fx.fake_overwatch("this is not json {{{", 0);
    let o = fx.check(&[]);
    assert_ne!(code(&o), 0, "{o:?}");
    assert_undetermined_json(&o);
}

#[test]
fn t5_missing_selected_agent_is_undetermined() {
    let fx = Fx::new();
    fx.fake_overwatch(&ow(&[("a1", "done")]), 0);
    let o = fx.check(&["--agent", "X"]);
    assert_eq!(code(&o), 1, "{o:?}");
    let v = json(&o);
    assert_eq!(v["clean"], false);
    assert_eq!(
        blocking(&v),
        vec![("X".to_string(), "undetermined".to_string())]
    );
}

#[test]
fn t6_run_selection_uses_recorded_agent_ids() {
    let fx = Fx::new();
    set_running(&fx, &["--agent-id", "X"]);
    fx.fake_overwatch(&ow(&[("X", "stalled"), ("Y", "done")]), 1);
    let o = fx.check(&["--run", RUN]);
    assert_eq!(code(&o), 1, "{o:?}");
    let v = json(&o);
    assert_eq!(blocking(&v), vec![("X".to_string(), "stalled".to_string())]);

    // X progressing; unrelated Z (not in the run) stalled -> overwatch exits 1
    // but the selected set is clean.
    fx.fake_overwatch(&ow(&[("X", "progressing"), ("Z", "stalled")]), 1);
    let o = fx.check(&["--run", RUN]);
    assert_eq!(code(&o), 0, "{o:?}");
    let v = json(&o);
    assert_eq!(v["clean"], true);
    assert!(blocking(&v).is_empty());
}

#[test]
fn t7_run_without_recorded_ids_is_not_clean() {
    let fx = Fx::new();
    fx.fake_overwatch(&ow(&[("a1", "done")]), 0);
    let o = fx.check(&["--run", RUN]);
    assert_ne!(code(&o), 0, "{o:?}");
    assert_undetermined_json(&o);
}
