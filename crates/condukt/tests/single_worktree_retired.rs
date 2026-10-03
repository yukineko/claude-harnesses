//! Backlog 5e5cf0a9 (round 3), USER RULING 2026-10-03: single-worktree mode is
//! RETIRED. Every task gets its own worktree. The binary must therefore never
//! report single-worktree mode, however the old switch is set, and must say so
//! out loud: a silently ignored setting lets an operator believe a mode is
//! active when it is not (CLAUDE.md sections 3 and 4).
//!
//! HOME is pointed at a temp dir, so `~/.condukt/config.toml` is the fixture's
//! and the real one is never read or written.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;
use std::process::{Command, Output};

fn run_mode_check(home: &Path, cwd: &Path, env: &[(&str, &str)]) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_condukt"));
    c.args(["state", "worktree-mode-check"])
        .current_dir(cwd)
        .env("HOME", home)
        .env_remove("CONDUKT_DISABLE")
        .env_remove("CONDUKT_SINGLE_WORKTREE");
    for (k, v) in env {
        c.env(k, v);
    }
    c.output().expect("spawn condukt")
}

fn fixture() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let cwd = tmp.path().join("cwd");
    std::fs::create_dir_all(home.join(".condukt")).unwrap();
    std::fs::create_dir_all(&cwd).unwrap();
    (tmp, home, cwd)
}

fn s(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

fn assert_per_task(o: &Output, what: &str) {
    assert_eq!(
        o.status.code(),
        Some(1),
        "{what}: must exit 1 (per-task mode); stdout={} stderr={}",
        s(&o.stdout),
        s(&o.stderr)
    );
    assert!(
        s(&o.stdout).contains("\"single_worktree\":false"),
        "{what}: stdout must say single_worktree:false, got {}",
        s(&o.stdout)
    );
}

fn assert_retired_notice(o: &Output, what: &str) {
    let err = s(&o.stderr);
    assert!(
        err.contains("single_worktree") || err.contains("SINGLE_WORKTREE"),
        "{what}: stderr must NAME the retired setting, got {err:?}"
    );
    assert!(
        err.contains("retired") || err.contains("廃止"),
        "{what}: stderr must say the setting is retired, got {err:?}"
    );
}

// ---- controls: the helpers can tell the contract from its absence ----------

#[test]
fn control_default_is_per_task_and_silent() {
    let (_t, home, cwd) = fixture();
    let o = run_mode_check(&home, &cwd, &[]);
    assert_per_task(&o, "default");
    let err = s(&o.stderr);
    assert!(
        !err.contains("retired") && !err.contains("廃止"),
        "no setting present, so no retired notice expected; got {err:?}"
    );
}

#[test]
fn control_notice_matcher_rejects_unrelated_stderr() {
    let r = std::panic::catch_unwind(|| {
        let o = Output {
            status: Default::default(),
            stdout: vec![],
            stderr: b"something else entirely".to_vec(),
        };
        assert_retired_notice(&o, "ctl");
    });
    assert!(r.is_err(), "matcher must fail on stderr that names nothing");
}

#[test]
fn control_unrelated_config_key_gives_no_notice() {
    let (_t, home, cwd) = fixture();
    std::fs::write(home.join(".condukt/config.toml"), "max_parallel = 2\n").unwrap();
    let o = run_mode_check(&home, &cwd, &[]);
    assert_per_task(&o, "unrelated key");
    assert!(
        !s(&o.stderr).contains("retired"),
        "stderr: {}",
        s(&o.stderr)
    );
}

// ---- the ruling (RED while the old mode still works) ------------------------

#[test]
fn env_switch_cannot_turn_single_worktree_on_and_is_called_retired() {
    for v in ["1", "true"] {
        let (_t, home, cwd) = fixture();
        let o = run_mode_check(&home, &cwd, &[("CONDUKT_SINGLE_WORKTREE", v)]);
        assert_per_task(&o, &format!("CONDUKT_SINGLE_WORKTREE={v}"));
        assert_retired_notice(&o, &format!("CONDUKT_SINGLE_WORKTREE={v}"));
    }
}

#[test]
fn config_file_cannot_turn_single_worktree_on_and_is_called_retired() {
    let (_t, home, cwd) = fixture();
    std::fs::write(
        home.join(".condukt/config.toml"),
        "single_worktree = true\n",
    )
    .unwrap();
    let o = run_mode_check(&home, &cwd, &[]);
    assert_per_task(&o, "config single_worktree=true");
    assert_retired_notice(&o, "config single_worktree=true");
}

#[test]
fn env_beats_nothing_both_set_still_per_task() {
    let (_t, home, cwd) = fixture();
    std::fs::write(
        home.join(".condukt/config.toml"),
        "single_worktree = true\n",
    )
    .unwrap();
    let o = run_mode_check(&home, &cwd, &[("CONDUKT_SINGLE_WORKTREE", "1")]);
    assert_per_task(&o, "env + config");
    assert_retired_notice(&o, "env + config");
}

#[test]
fn config_true_with_env_false_is_still_per_task_and_config_is_called_retired() {
    let (_t, home, cwd) = fixture();
    std::fs::write(
        home.join(".condukt/config.toml"),
        "single_worktree = true\n",
    )
    .unwrap();
    let o = run_mode_check(&home, &cwd, &[("CONDUKT_SINGLE_WORKTREE", "0")]);
    assert_per_task(&o, "config true + env 0");
    assert_retired_notice(&o, "config true + env 0");
    let err = s(&o.stderr);
    assert!(
        err.contains("config"),
        "stderr must name the config file as the source, got {err:?}"
    );
}
