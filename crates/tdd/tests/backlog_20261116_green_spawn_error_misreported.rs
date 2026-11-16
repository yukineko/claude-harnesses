#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro (2026-11-16): `tdd green` reports "tests still failing" when the
//! test command never ran. `crates/tdd/src/proof.rs` `green()` calls
//! `judge_green(out.passed, has_red)` without `judge_ran(&out)` (which `red()`
//! calls since 9c58c590d), and the runner maps a spawn / run-log failure to
//! `passed: false, spawn_error: Some(..)`. GREEN is refused either way (no
//! fail-open), but the message tells the implementer their tests fail and to
//! "keep implementing", which misattributes an environment failure to the code.
//!
//! Method (same trigger as backlog_20261115_red_spawn_error_records_red.rs):
//! record a real RED with a usable state dir, then put a regular FILE at
//! `<HOME>/.tdd/state` so the run log cannot be created and the command is not
//! run (marker file asserted absent).
//!
//! Reading (a), asserted: a run that never happened must be named as such
//! (the spawn error / "never ran"), not as "tests still failing".
//! Reading (b): the refusal is all that matters, the wording is cosmetic. Both
//! agree GREEN must be refused (asserted as a control too).
//!
//! DOES NOT PROVE: the real `Command::spawn` ENOENT path, the timeout path.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

struct Dir(PathBuf);
impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn run(home: &Path, cwd: &Path, phase: &str, cmd: &str) -> Output {
    harness_core::git_env::scrub(&mut Command::new(env!("CARGO_BIN_EXE_tdd")))
        .args([phase, "--task", "t1", "--cmd", cmd])
        .current_dir(cwd)
        .env("HOME", home)
        .stdin(Stdio::null())
        .output()
        .expect("tdd runs")
}

#[test]
fn green_names_the_spawn_failure_not_failing_tests() {
    let d = Dir(std::env::temp_dir().join(format!("tdd-green-spawn-{}", std::process::id())));
    let _ = std::fs::remove_dir_all(&d.0);
    let home = d.0.join("home");
    let work = d.0.join("work");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&work).unwrap();

    let red = run(&home, &work, "red", "exit 1");
    assert_eq!(
        red.status.code(),
        Some(0),
        "control: RED must be recorded: {}",
        String::from_utf8_lossy(&red.stderr)
    );

    // Break the run log: a regular file where the state dir must be.
    std::fs::remove_dir_all(home.join(".tdd").join("state")).ok();
    std::fs::create_dir_all(home.join(".tdd")).unwrap();
    std::fs::write(home.join(".tdd").join("state"), "not a dir").unwrap();

    let out = run(&home, &work, "green", "touch ran.marker; exit 0");
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        !work.join("ran.marker").exists(),
        "setup invalid: the command actually ran"
    );
    assert_ne!(out.status.code(), Some(0), "control: GREEN must be refused");
    assert!(
        !work.join(".tdd").join("t1.green.json").exists(),
        "control: no GREEN proof"
    );
    assert!(
        !stderr.contains("tests still failing"),
        "the command never ran, yet tdd green said the tests are still failing.\nstdout: {stdout}\nstderr: {stderr}"
    );
}
