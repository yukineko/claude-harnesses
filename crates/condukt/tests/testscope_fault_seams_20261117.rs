#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Fault-injection seams of spec testscope-confirmed-run (backlog 64fe4e19),
//! per the user ruling 2026-11-17: `CONDUKT_TESTSCOPE_FAULT=confirm_panic`
//! (R5, a panic inside the confirm hook) and `invalidate_append_fail` (R8, the
//! invalidate append after a spawn failure).
//!
//! Written by an independent test author, not the implementer. Every
//! expectation is taken from the spec's R5 / R8 acceptance text, not from the
//! implementation.
//!
//! Each seam has one fault test and one control that runs the same scenario
//! without the env var and asserts the normal outcome.
//!
//! DOES NOT PROVE: the exact wording of the messages (only the statements the
//! spec requires: "not recorded" / "could not be invalidated" + "rerun will
//! ask"), nor the v2 column layout.

mod testscope_confirm_common;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use testscope_confirm_common::*;

const FAULT: &str = "CONDUKT_TESTSCOPE_FAULT";
const CMD: &str = "cargo test -p x --test t";

// ── R5: confirm hook panic ──────────────────────────────────────────────────

/// Like `post`, but with `CONDUKT_TESTSCOPE_FAULT` set to `fault` (or removed).
fn post_with_fault(env: &Env, id: &str, cmd: &str, cwd: &Path, fault: Option<&str>) -> Out {
    let payload = serde_json::json!({
        "hook_event_name": "PostToolUse",
        "session_id": "s",
        "tool_name": "Bash",
        "tool_use_id": id,
        "tool_input": {"command": cmd},
        "cwd": cwd.to_str().unwrap(),
    });
    let mut c = Command::new(env!("CARGO_BIN_EXE_condukt"));
    harness_core::git_env::scrub(&mut c)
        .args(hook_args("PostToolUse"))
        .env("HOME", env.home.path())
        .env("CONDUKT_TESTSCOPE_LEDGER", env.ledger())
        .env("CONDUKT_DEFAULT_BRANCH", "main")
        .env_remove("CONDUKT_DISABLE")
        .env_remove("CONDUKT_WORKTREE_BASE")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .env_remove(FAULT)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(f) = fault {
        c.env(FAULT, f);
    }
    let mut child = c.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.to_string().as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    Out {
        code: out.status.code(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

fn pre_pending(env: &Env, repo: &Path) {
    let first = pre(env, "A", CMD, repo);
    assert!(
        first.is_allow(),
        "first Pre on a fresh ledger must allow: {}",
        first.stdout
    );
}

#[test]
fn r5_confirm_hook_panic_surfaces_system_message_never_blocks_and_rerun_asks() {
    let env = Env::new();
    let repo = repo();
    pre_pending(&env, repo.path());

    let out = post_with_fault(&env, "A", CMD, repo.path(), Some("confirm_panic"));

    // A single JSON object on stdout, with a systemMessage saying NOT recorded.
    let v: serde_json::Value = serde_json::from_str(out.stdout.trim()).unwrap_or_else(|e| {
        panic!(
            "stdout must be one JSON object ({e}): stdout={:?} stderr={:?}",
            out.stdout, out.stderr
        )
    });
    assert!(
        v.is_object(),
        "stdout must be a JSON object: {}",
        out.stdout
    );
    let msg = v["systemMessage"]
        .as_str()
        .unwrap_or_else(|| panic!("no systemMessage in {}", out.stdout));
    assert!(
        msg.to_lowercase().contains("not recorded"),
        "systemMessage must say the confirmation was not recorded: {msg}"
    );
    // Nothing in stdout may block (a Post cannot stop a call that already ran).
    assert!(
        !out.stdout.contains("\"decision\":\"block\"")
            && !out.stdout.contains("\"decision\": \"block\""),
        "must not block: {}",
        out.stdout
    );
    assert!(
        v.get("decision").is_none(),
        "no decision key: {}",
        out.stdout
    );
    assert_eq!(
        out.code,
        Some(0),
        "exit code must be 0: stderr={:?}",
        out.stderr
    );

    // No confirm row was written; a following identical Pre is Ask.
    let v2 = std::fs::read_to_string(env.v2()).unwrap_or_default();
    assert!(
        !v2.lines().any(|l| l.starts_with("confirm")),
        "a confirm row was written despite the panic: {v2:?}"
    );
    let again = pre(&env, "B", CMD, repo.path());
    assert!(
        again.is_ask(),
        "rerun after an unrecorded confirmation must be Ask (not Deny, not allow): {}",
        again.stdout
    );
}

#[test]
fn r5_control_without_fault_confirms_silently_and_rerun_denies() {
    let env = Env::new();
    let repo = repo();
    pre_pending(&env, repo.path());

    let out = post_with_fault(&env, "A", CMD, repo.path(), None);
    assert_eq!(out.code, Some(0), "stderr={:?}", out.stderr);
    assert!(
        out.is_allow(),
        "a successful confirmation prints nothing: {}",
        out.stdout
    );
    let v2 = std::fs::read_to_string(env.v2()).expect("ledger.v2.tsv must exist");
    assert!(
        v2.lines().any(|l| l.starts_with("confirm")),
        "no confirm row: {v2:?}"
    );
    let again = pre(&env, "B", CMD, repo.path());
    assert!(
        again.is_deny(),
        "confirmed rerun must Deny: {}",
        again.stdout
    );
}

// ── R8: invalidate append failure after a spawn failure ─────────────────────

struct Fx {
    env: Env,
    repo: tempfile::TempDir,
    bin: tempfile::TempDir,
    counter: PathBuf,
}

fn run(fx: &Fx, path: &str, fault: Option<&str>, args: &[&str]) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_condukt"));
    harness_core::git_env::scrub(&mut c)
        .args(args)
        .current_dir(fx.repo.path())
        .env("HOME", fx.env.home.path())
        .env("CONDUKT_TESTSCOPE_LEDGER", fx.env.ledger())
        .env("CONDUKT_DEFAULT_BRANCH", "main")
        .env("CLAUDE_CODE_SESSION_ID", "s")
        .env("PATH", path)
        .env_remove(FAULT);
    if let Some(f) = fault {
        c.env(FAULT, f);
    }
    c.output().unwrap()
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
        None,
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

/// A PATH holding git but no `sh`: the tree identity works, spawning `sh -c`
/// fails. Fails loudly if the environment cannot build it.
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
fn r8_invalidate_append_failure_is_reported_leaves_row_pending_and_rerun_asks() {
    let fx = fixture();
    let failed = run(
        &fx,
        &path_without_sh(),
        Some("invalidate_append_fail"),
        &["state", "test", "--run", "runT"],
    );
    assert!(!failed.status.success(), "must exit non-zero: {failed:?}");
    let stderr = String::from_utf8_lossy(&failed.stderr).to_lowercase();
    assert!(
        stderr.contains("could not be invalidated"),
        "stderr must say the attempt could not be invalidated: {stderr}"
    );
    assert!(
        stderr.contains("rerun will ask"),
        "stderr must say a rerun will ask: {stderr}"
    );
    assert_eq!(invocations(&fx.counter), 0, "the test must not have run");

    let v2 = std::fs::read_to_string(fx.env.v2()).expect("pending row must be in ledger.v2.tsv");
    assert!(
        !v2.lines().any(|l| l.starts_with("invalidate")),
        "an invalidate row exists although the append was forced to fail: {v2:?}"
    );

    // The row is still pending: a rerun on the same tree asks (not runs, not denies).
    let rerun = run(
        &fx,
        &normal_path(&fx),
        None,
        &["state", "test", "--run", "runT"],
    );
    assert!(!rerun.status.success(), "rerun must not run: {rerun:?}");
    assert_eq!(
        invocations(&fx.counter),
        0,
        "rerun must not execute the test"
    );
    let hook = pre(&fx.env, "h1", CMD, fx.repo.path());
    assert!(
        hook.is_ask(),
        "a hook Pre of the same command on the same tree must be Ask: {}",
        hook.stdout
    );
}

#[test]
fn r8_control_without_fault_spawn_failure_invalidates_and_rerun_runs() {
    let fx = fixture();
    let failed = run(
        &fx,
        &path_without_sh(),
        None,
        &["state", "test", "--run", "runT"],
    );
    assert!(
        !failed.status.success(),
        "spawn failure must fail: {failed:?}"
    );
    let stderr = String::from_utf8_lossy(&failed.stderr).to_lowercase();
    assert!(
        !stderr.contains("could not be invalidated"),
        "without the fault the invalidation succeeds: {stderr}"
    );
    let v2 = std::fs::read_to_string(fx.env.v2()).expect("ledger.v2.tsv must exist");
    assert!(
        v2.lines().any(|l| l.starts_with("invalidate")),
        "no invalidate row: {v2:?}"
    );
    let rerun = run(
        &fx,
        &normal_path(&fx),
        None,
        &["state", "test", "--run", "runT"],
    );
    assert!(rerun.status.success(), "rerun must run: {rerun:?}");
    assert_eq!(invocations(&fx.counter), 1);
}
