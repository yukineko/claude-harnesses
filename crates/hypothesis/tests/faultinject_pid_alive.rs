// このファイルは丸ごと integration test なので unwrap/expect を許可する
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! FAULT INJECTION — C2 twin: `hypothesis::lock::pid_alive` reports a LIVE
//! lock holder as DEAD when it cannot spawn `kill`.
//!
//! `crates/hypothesis/src/lock.rs` (pre-fix):
//! ```ignore
//! fn pid_alive(pid: u32) -> bool {
//!     #[cfg(target_os = "linux")]
//!     { if Path::new(&format!("/proc/{pid}")).exists() { return true; } }
//!     std::process::Command::new("kill")
//!         .args(["-0", &pid.to_string()])
//!         ...
//!         .status()
//!         .map(|s| s.success())
//!         .unwrap_or(false)   // <-- spawn failure == "the holder is dead"
//! }
//! ```
//! Consumed in the lock's acquire retry loop:
//! ```ignore
//! Some(existing) if !pid_alive(existing.pid) => {
//!     let _ = std::fs::remove_file(&path);   // reap someone else's LIVE lock
//!     continue;
//! }
//! ```
//! `unwrap_or(false)` maps "I could not ask the OS" to "the holder is gone",
//! which is the strictly less safe answer: the lock is STOLEN from a live
//! holder and the caller proceeds into the very load->mutate->save window the
//! lock exists to close.
//!
//! ## Why it is driven through the binary, not `mod lock` directly
//!
//! `hypothesis` has no `[lib]` target (only `[[bin]] name = "hypothesis"
//! path = "src/main.rs"`), so `mod lock` is not reachable from `tests/` at
//! all, and making it `pub`/adding a lib target just to reach a private
//! internal concurrency primitive would be a production-shaped change this
//! work does not require. Instead the fault is driven through the real CLI
//! binary's `confidence` subcommand, which goes through
//! `Store::load` -> `StoreLock::acquire_or_skip`, the documented hard-skip
//! caller of `pid_alive`: contended (lock respected) -> non-zero exit and
//! the lock file survives; reaped (lock stolen) -> exit 0 and the mutation
//! lands. That difference is the observation.
//!
//! ## Why per-child `PATH`, not `std::env::set_var`
//!
//! Because the fault is driven through a spawned *subprocess* rather than an
//! in-process library call, the faulty `PATH` can be scoped to that one
//! child's `Command::env`, with the test process's own (and every sibling
//! test's) `PATH` left untouched. `Command::new` is given the binary's
//! *absolute* path (`CARGO_BIN_EXE_hypothesis`), so resolving the binary
//! itself never depends on `PATH`; only the child's own `kill -0` spawn
//! (inside `pid_alive`) does. `Command::new("kill")` resolves via `PATH` and
//! does NOT go through a shell, so the `kill` shell builtin cannot rescue it.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_hypothesis"))
}

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("hypothesis-fi-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Runs the real `hypothesis` binary against a sandboxed `HOME`, optionally
/// overriding `PATH` for just this one child process.
fn run_hypothesis(
    home: &Path,
    path_override: Option<&Path>,
    args: &[&str],
) -> std::process::Output {
    let mut cmd = Command::new(bin());
    cmd.env("HOME", home)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(p) = path_override {
        cmd.env("PATH", p);
    }
    cmd.output().expect("failed to run hypothesis")
}

#[test]
fn live_lock_holder_is_stolen_when_kill_cannot_be_spawned() {
    // Sandboxed HOME so this never touches the real ~/.hypothesis.
    let home = temp_dir("pidalive-home");
    let store_dir = home.join(".hypothesis");
    std::fs::create_dir_all(&store_dir).unwrap();

    // Seed one hypothesis (sequential; the lock is not contended for this
    // call, so it is unaffected by anything under test).
    let add_out = run_hypothesis(&home, None, &["add", "h1"]);
    assert!(
        add_out.status.success(),
        "precondition: `add` failed: stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&add_out.stdout),
        String::from_utf8_lossy(&add_out.stderr)
    );
    let id = String::from_utf8_lossy(&add_out.stdout).trim().to_string();
    assert!(!id.is_empty(), "precondition: `add` printed no id");

    let lock_path = store_dir.join("hypotheses.lock");
    // A lock held by a pid that is unambiguously ALIVE: this very test
    // process.
    let live_pid = std::process::id();
    let seed_lock = || {
        std::fs::write(
            &lock_path,
            format!("{{\"pid\":{live_pid},\"acquired_at\":0}}"),
        )
        .unwrap();
    };

    // ---- CONTROL: `kill` resolvable (normal PATH, inherited). The live
    // holder must be respected, so `confidence` hard-skips (bounded wait,
    // then a non-zero exit) rather than reaping and mutating.
    seed_lock();
    let t0 = Instant::now();
    let control = run_hypothesis(&home, None, &["confidence", &id, "0.11"]);
    let control_elapsed = t0.elapsed();
    assert!(
        !control.status.success(),
        "control precondition: with `kill` resolvable, a lock held by this \
         live pid must be respected and `confidence` hard-skipped (status \
         {:?}, elapsed {control_elapsed:?}, stdout={:?} stderr={:?})",
        control.status,
        String::from_utf8_lossy(&control.stdout),
        String::from_utf8_lossy(&control.stderr)
    );
    assert!(
        lock_path.exists(),
        "control precondition: the live holder's lock must NOT have been reaped"
    );

    // ---- FAULT: make `kill` unspawnable for the CHILD only. Nothing else
    // changes: same seeded lock file, same live pid.
    seed_lock();
    let empty_path_dir = temp_dir("pidalive-emptypath");
    let t1 = Instant::now();
    let fault = run_hypothesis(&home, Some(&empty_path_dir), &["confidence", &id, "0.22"]);
    let fault_elapsed = t1.elapsed();
    let lock_survived = lock_path.exists();

    assert!(
        !fault.status.success(),
        "FAIL-OPEN: `kill` could not be spawned (empty PATH), so pid_alive's \
         fail-open mapped that opacity to 'the holder is dead'. Pid {live_pid} \
         — THIS RUNNING TEST PROCESS — was reported dead, its lock reaped, and \
         the guarded `confidence` update went through in {fault_elapsed:?} \
         (control took {control_elapsed:?} and correctly hard-skipped). \
         stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&fault.stdout),
        String::from_utf8_lossy(&fault.stderr),
    );
    assert!(
        lock_survived,
        "FAIL-OPEN: the LIVE holder's lock file was deleted (reaped) because \
         `kill` could not be spawned."
    );
}
