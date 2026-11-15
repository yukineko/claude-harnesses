#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro (2026-11-16): `tdd red` records a RED proof when the test command
//! was KILLED BY A SIGNAL (`exit_code: None`, no `spawn_error`, not timed out).
//! `crates/tdd/src/proof.rs` `red()` = `judge_ran(&out)` (checks only
//! `spawn_error`) then `judge_red(out.passed)` (records whenever
//! `passed == false`); the success line then prints the status as "killed".
//!
//! Reading (a), asserted here: a crash/kill is not a demonstrated assertion
//! failure -> cannot determine -> no RED recorded (non-zero exit, no
//! `<task>.red.json`). Reading (b): a crashing test is a failing test, so RED is
//! correct. Under (b) this test is wrong. They are told apart only by a ruling:
//! (a) is what CLAUDE.md 2(b)/3 ("crash/panic is undetermined") suggests, but a
//! Rust test that panics exits 101 (not a signal), so (b) only concerns runs the
//! harness/OS killed (SIGKILL, OOM, SIGSEGV).
//!
//! Control: the same command that exits 1 records RED (not vacuous).
//! DOES NOT PROVE: the timeout path (`timed_out: true`), `tdd green`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

struct Dir(PathBuf);
impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn setup(tag: &str) -> (Dir, PathBuf, PathBuf) {
    let d = Dir(std::env::temp_dir().join(format!("tdd-red-kill-{tag}-{}", std::process::id())));
    let _ = std::fs::remove_dir_all(&d.0);
    let home = d.0.join("home");
    let work = d.0.join("work");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&work).unwrap();
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

// test-weakening-justified: new RED for an unfixed defect (backlog pending, 2026-11-16); the ignore is removed in the fixing commit.
#[ignore]
#[test]
fn red_is_not_recorded_when_the_command_was_killed_by_a_signal() {
    let (_d, home, work) = setup("sig");
    let out = red(&home, &work, "touch ran.marker; kill -KILL $$");
    assert!(
        work.join("ran.marker").exists(),
        "setup: the command must have started"
    );
    let proof = work.join(".tdd").join("t1.red.json");
    assert!(
        !proof.exists() && out.status.code() != Some(0),
        "command was killed by SIGKILL (no assertion observed) yet tdd red exit={:?}, RED proof exists={}\nstdout: {}\nstderr: {}",
        out.status.code(),
        proof.exists(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn control_command_exiting_1_records_red() {
    let (_d, home, work) = setup("ctl");
    let out = red(&home, &work, "touch ran.marker; exit 1");
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(work.join(".tdd").join("t1.red.json").exists());
}
