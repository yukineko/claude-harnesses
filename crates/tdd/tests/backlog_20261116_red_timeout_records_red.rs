#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro (2026-11-16, backlog b423c0c1): `tdd red` records a RED proof when
//! the test command TIMED OUT (`timed_out: true`, `exit_code: None`).
//!
//! User ruling 2026-11-16 (reading (a), extended to timeouts): a kill or crash
//! with no observed assertion failure is undetermined, so `tdd red` must refuse
//! (non-zero exit) and write no `<task>.red.json` when the command timed out.
//! A command that runs and exits non-zero (e.g. exit 1) still records RED.
//!
//! The timeout is set through the home-level config `~/.tdd/config.toml`
//! (`default_timeout_secs = 1`), which `Config::load` honors without the
//! workspace-trust requirement that a project `tdd.toml` has.
//!
//! The test asserts the command actually started (marker), that tdd returned
//! well before the command's own `sleep 30` would have finished (so the run was
//! cut by the timeout, not a config error), and that stderr carries no config
//! refusal. The control (same config, `exit 1`) records RED, proving the config
//! setup does not itself block recording.
//!
//! DOES NOT PROVE: the signal-kill path (see
//! `backlog_20261116_red_killed_by_signal_records_red.rs`), `tdd green`, or
//! that a timeout inside a nested process group is reaped.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

struct Dir(PathBuf);
impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn setup(tag: &str) -> (Dir, PathBuf, PathBuf) {
    let d = Dir(std::env::temp_dir().join(format!("tdd-red-to-{tag}-{}", std::process::id())));
    let _ = std::fs::remove_dir_all(&d.0);
    let home = d.0.join("home");
    let work = d.0.join("work");
    std::fs::create_dir_all(home.join(".tdd")).unwrap();
    std::fs::create_dir_all(&work).unwrap();
    std::fs::write(
        home.join(".tdd").join("config.toml"),
        "default_timeout_secs = 1\n",
    )
    .unwrap();
    (d, home, work)
}

fn red(home: &Path, cwd: &Path, cmd: &str) -> Output {
    harness_core::git_env::scrub(&mut Command::new(env!("CARGO_BIN_EXE_tdd")))
        .args(["red", "--task", "t1", "--cmd", cmd])
        .current_dir(cwd)
        .env("HOME", home)
        .stdin(Stdio::null())
        .output()
        .expect("tdd runs")
}

#[test]
fn red_is_not_recorded_when_the_command_timed_out() {
    let (_d, home, work) = setup("to");
    let started = Instant::now();
    let out = red(&home, &work, "touch ran.marker; sleep 30");
    let elapsed = started.elapsed();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        work.join("ran.marker").exists(),
        "setup: the command must have started\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        elapsed < Duration::from_secs(25),
        "setup: the run must have been cut by the 1s timeout, took {elapsed:?}"
    );
    assert!(
        !stderr.contains("REFUSING TO JUDGE") && !stderr.contains("parse failed"),
        "setup: config must have loaded cleanly\nstderr: {stderr}"
    );
    let proof = work.join(".tdd").join("t1.red.json");
    assert!(
        !proof.exists() && out.status.code() != Some(0),
        "command timed out (no assertion observed) yet tdd red exit={:?}, RED proof exists={}\nstdout: {stdout}\nstderr: {stderr}",
        out.status.code(),
        proof.exists(),
    );
}

#[test]
fn control_command_exiting_1_records_red_under_the_same_config() {
    let (_d, home, work) = setup("ctl");
    let out = red(&home, &work, "touch ran.marker; exit 1");
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(work.join("ran.marker").exists());
    assert!(work.join(".tdd").join("t1.red.json").exists());
}
