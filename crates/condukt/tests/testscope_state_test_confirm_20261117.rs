#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! R8 of spec testscope-confirmed-run (backlog 64fe4e19): `condukt state test`
//! confirms its own run, and invalidates its pending row when spawn fails.
//!
//! Written by an independent test author, not the implementer.
//!
//! UNTESTABLE here: "invalidate append fails -> stays pending, stderr error,
//! non-zero". The pending row and the invalidate are appended by the same
//! process with the spawn failure in between; a test cannot make only the
//! second append fail without injecting a fault into the source.
//!
//! DOES NOT PROVE: v2 column layout (only the substrings `confirm` /
//! `invalidate`).

mod testscope_confirm_common;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use testscope_confirm_common::*;

const CMD: &str = "cargo test -p x --test t";

struct Fx {
    env: Env,
    repo: tempfile::TempDir,
    bin: tempfile::TempDir,
    counter: PathBuf,
}

fn run(fx: &Fx, path: &str, args: &[&str]) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_condukt"));
    harness_core::git_env::scrub(&mut c)
        .args(args)
        .current_dir(fx.repo.path())
        .env("HOME", fx.env.home.path())
        .env("CONDUKT_TESTSCOPE_LEDGER", fx.env.ledger())
        .env("CONDUKT_DEFAULT_BRANCH", "main")
        .env("CLAUDE_CODE_SESSION_ID", "s")
        .env("PATH", path)
        .output()
        .unwrap()
}

fn fixture() -> Fx {
    let env = Env::new();
    let repo = repo();
    let cfg = env.home.path().join(".condukt");
    std::fs::create_dir_all(&cfg).unwrap();
    std::fs::write(
        cfg.join("config.toml"),
        format!("[test]\ncommand = \"{CMD}\"\n"),
    )
    .unwrap();
    let dec = serde_json::json!({"goal": "g", "tasks": [{
        "id": "t1", "title": "t1", "touched_files": ["a.txt"], "deps": [],
        "class": "parallel", "done_criteria": "d", "reproduction_tests": CMD}]});
    let p = env.home.path().join("runT.dec.json");
    std::fs::write(&p, dec.to_string()).unwrap();
    let bin = tempfile::tempdir().unwrap();
    let counter = bin.path().join("count.txt");
    let cargo = bin.path().join("cargo");
    std::fs::write(
        &cargo,
        format!("#!/bin/sh\necho x >> '{}'\nexit 0\n", counter.display()),
    )
    .unwrap();
    std::fs::set_permissions(&cargo, std::fs::Permissions::from_mode(0o755)).unwrap();
    let fx = Fx {
        env,
        repo,
        bin,
        counter,
    };
    let init = run(
        &fx,
        &normal_path(&fx),
        &[
            "state",
            "init",
            "--run",
            "runT",
            "--file",
            p.to_str().unwrap(),
        ],
    );
    assert!(init.status.success(), "{init:?}");
    fx
}

fn normal_path(fx: &Fx) -> String {
    format!(
        "{}:{}",
        fx.bin.path().display(),
        std::env::var("PATH").unwrap()
    )
}

fn invocations(p: &Path) -> usize {
    std::fs::read_to_string(p)
        .map(|s| s.lines().count())
        .unwrap_or(0)
}

#[test]
#[ignore = "RED testscope-confirmed-run R8: state test writes no self-confirm row (no ledger.v2.tsv)"]
fn state_test_confirms_its_own_run() {
    let fx = fixture();
    let first = run(&fx, &normal_path(&fx), &["state", "test", "--run", "runT"]);
    assert!(first.status.success(), "{first:?}");
    let v2 = std::fs::read_to_string(fx.env.v2()).expect("ledger.v2.tsv must exist");
    assert!(v2.contains("confirm"), "no confirm row: {v2:?}");
    let second = run(&fx, &normal_path(&fx), &["state", "test", "--run", "runT"]);
    assert!(!second.status.success(), "{second:?}");
    assert_eq!(invocations(&fx.counter), 1);
    // A hook rerun of the same command is Deny (confirmed), not Ask (pending).
    let hook = pre(&fx.env, "h1", CMD, fx.repo.path());
    assert!(
        hook.is_deny(),
        "self-confirmed run must Deny: {}",
        hook.stdout
    );
}

/// A PATH holding git but no `sh`, so the tree identity works and spawning
/// `sh -c` fails. The directory is taken from the real git, and must not hold sh.
fn path_without_sh() -> String {
    let out = Command::new("sh")
        .args(["-c", "command -v git"])
        .output()
        .unwrap();
    let git = String::from_utf8(out.stdout).unwrap();
    let dir = Path::new(git.trim()).parent().unwrap().to_path_buf();
    assert!(
        !dir.join("sh").exists(),
        "environment cannot build a PATH with git but without sh ({}): spawn failure not constructible here",
        dir.display()
    );
    dir.display().to_string()
}

#[test]
#[ignore = "RED testscope-confirmed-run R8: spawn failure appends no invalidate row; the rerun stays pending-blocked"]
fn spawn_failure_invalidates_pending_row_so_rerun_runs() {
    let fx = fixture();
    let failed = run(&fx, &path_without_sh(), &["state", "test", "--run", "runT"]);
    assert!(
        !failed.status.success(),
        "spawn failure must fail: {failed:?}"
    );
    assert_eq!(invocations(&fx.counter), 0);
    let v2 = std::fs::read_to_string(fx.env.v2()).expect("ledger.v2.tsv must exist");
    assert!(v2.contains("invalidate"), "no invalidate record: {v2:?}");
    let rerun = run(&fx, &normal_path(&fx), &["state", "test", "--run", "runT"]);
    assert!(
        rerun.status.success(),
        "rerun after invalidated spawn failure: {rerun:?}"
    );
    assert_eq!(invocations(&fx.counter), 1);
}
