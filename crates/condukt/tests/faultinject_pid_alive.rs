// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! FAULT INJECTION — C2: `condukt::lock::pid_alive` reports a LIVE lock holder
//! as DEAD when it cannot spawn `kill`.
//!
//! `crates/condukt/src/lock.rs:128-141` (at HEAD `05962e2a`):
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
//! Consumed at `lock.rs:290`, inside the acquire retry loop:
//! ```ignore
//! Some(existing) if !pid_alive(existing.pid) => {
//!     let _ = std::fs::remove_file(&path);   // reap someone else's LIVE lock
//!     continue;
//! }
//! ```
//! `unwrap_or(false)` maps "I could not ask the OS" to "the holder is gone" —
//! the strictly less safe answer: the lock is STOLEN from a live holder and the
//! caller proceeds into the very load->check->save window the lock exists to
//! close.
//!
//! ## Why it is driven through the CLI binary
//!
//! `condukt` has no lib target (`[[bin]] name = "condukt" path = "src/main.rs"`),
//! so `mod lock` is not reachable from `tests/`, and making `pid_alive` `pub`
//! just to test it would be a scope violation. Instead the fault is driven
//! through the `condukt state claim-task` subcommand, which serializes on
//! `crate::lock::RunLock` keyed on the reserved `"__claims__"` id
//! (`crate::claim::claim_tasks` -> `RunLock::acquire_or_skip`, 10s deadline).
//! We pre-place a `__claims__.lock` file naming a pid that is unambiguously
//! alive (this test process's own pid) and observe whether `claim-task` steals
//! it (exit 0, hashkey claimed) or respects it and hard-skips (exit 1,
//! hashkey skipped) after the acquisition deadline elapses.
//!
//! ## Why `PATH` and why one `#[test]`
//!
//! `Command::new("kill")` resolves via `PATH` and does NOT go through a shell,
//! so the `kill` shell builtin cannot rescue it — verified on this machine
//! (macOS, aarch64): normal PATH resolves `kill`; an empty-dir PATH makes
//! spawning it fail with `NotFound`. `std::env::set_var("PATH", ..)` is
//! PROCESS-global, and integration test binaries in this workspace run
//! `#[test]`s in parallel threads by default. This file therefore contains
//! exactly ONE `#[test]`, which runs the control phase and the fault phase
//! SEQUENTIALLY (restoring `PATH` between them), so no sibling test in this
//! binary can observe the mutated `PATH`.
//!
//! Each phase waits out `RunLock::DEADLINE` (10s) because the lock-holder pid
//! is alive in both phases (only the ability to *ask* the OS about it
//! differs), so this test takes >20s end to end — mirrors
//! `crates/overwatch/tests/faultinject_pid_alive.rs`, the twin site this
//! defect was ported from.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_condukt"))
}

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("condukt-fi-pidalive-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Mirrors `crate::lock::lock_path` for the reserved `"__claims__"` run id:
/// `<state_dir>/<project_key(repo_root(cwd))>/<safe_session("__claims__")>.lock`,
/// where `state_dir` defaults to `<HOME>/.condukt/state`. Computed from public
/// `harness_core` helpers (already a normal `condukt` dependency) rather than
/// reaching into private condukt internals.
fn claims_lock_path(home: &Path, cwd: &Path) -> PathBuf {
    let state_dir = home.join(".condukt").join("state");
    let project_key = harness_core::projkey::project_key(&harness_core::projkey::repo_root(cwd));
    state_dir.join(project_key).join(format!(
        "{}.lock",
        harness_core::store::safe_session("__claims__")
    ))
}

fn spawn_claim_task(cwd: &Path, home: &Path, path_override: Option<&Path>) -> std::process::Child {
    let mut cmd = Command::new(bin());
    cmd.current_dir(cwd)
        .env("HOME", home)
        .args([
            "state",
            "claim-task",
            "--run",
            "run-fi",
            "--session",
            "sess-fi",
            "--title",
            "fault-injection claim",
            "--hashkey",
            "fi-hashkey",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(p) = path_override {
        cmd.env("PATH", p);
    }
    cmd.spawn()
        .expect("failed to spawn condukt state claim-task")
}

#[test]
fn live_lock_holder_is_stolen_when_kill_cannot_be_spawned() {
    let home = temp_dir("home");
    let cwd = temp_dir("cwd");

    // A lock held by a pid that is unambiguously ALIVE: this very test
    // process. Field names must match `crate::lock::LockInfo` (pid, run_id,
    // acquired_at) — condukt's private struct, mirrored here by JSON shape
    // only (not by importing it).
    let live_pid = std::process::id();
    let lock_path = claims_lock_path(&home, &cwd);
    std::fs::create_dir_all(lock_path.parent().unwrap()).unwrap();
    std::fs::write(
        &lock_path,
        format!(r#"{{"pid":{live_pid},"run_id":"__claims__","acquired_at":0}}"#),
    )
    .unwrap();

    // ---- CONTROL: `kill` resolvable via the inherited PATH. The live holder
    // must be respected, so `claim-task` hard-skips (exit 1) after waiting out
    // the acquisition deadline, and the lock file must survive.
    let t0 = Instant::now();
    let control = spawn_claim_task(&cwd, &home, None)
        .wait_with_output()
        .expect("wait on control claim-task");
    let control_elapsed = t0.elapsed();
    assert_eq!(
        control.status.code(),
        Some(1),
        "control precondition: with `kill` resolvable, a lock held by this \
         live pid must be respected and claim-task hard-skipped (exit 1), got \
         {:?} in {control_elapsed:?}; stdout={:?} stderr={:?}",
        control.status.code(),
        String::from_utf8_lossy(&control.stdout),
        String::from_utf8_lossy(&control.stderr)
    );
    assert!(
        lock_path.exists(),
        "control precondition: the live holder's lock must NOT have been reaped"
    );

    // ---- FAULT: make `kill` unspawnable (empty-dir PATH). Nothing else
    // changes — the lock file (naming the same live pid) is still in place.
    let empty_path_dir = temp_dir("emptypath");

    let t1 = Instant::now();
    let fault = spawn_claim_task(&cwd, &home, Some(&empty_path_dir))
        .wait_with_output()
        .expect("wait on fault claim-task");
    let fault_elapsed = t1.elapsed();
    let lock_survived = lock_path.exists();

    assert_eq!(
        fault.status.code(),
        Some(1),
        "FAIL-OPEN: `kill` could not be spawned (empty PATH), so pid_alive's \
         `.unwrap_or(false)` (or equivalent) reported pid {live_pid} — THIS \
         RUNNING TEST PROCESS — as dead. The live holder's lock was reaped and \
         claim-task claimed the hashkey (exit {:?}) in {fault_elapsed:?} \
         (control took {control_elapsed:?} and correctly hard-skipped with \
         exit 1). 'I cannot determine whether the holder is alive' was \
         resolved to 'the holder is dead' — the permissive side. \
         stdout={:?} stderr={:?}",
        fault.status.code(),
        String::from_utf8_lossy(&fault.stdout),
        String::from_utf8_lossy(&fault.stderr)
    );
    assert!(
        lock_survived,
        "FAIL-OPEN: the LIVE holder's lock file was deleted (reaped) because \
         `kill` could not be spawned."
    );

    std::fs::remove_dir_all(&home).ok();
    std::fs::remove_dir_all(&cwd).ok();
    std::fs::remove_dir_all(&empty_path_dir).ok();
}
