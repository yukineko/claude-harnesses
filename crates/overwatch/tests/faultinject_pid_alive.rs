// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! FAULT INJECTION — C2: `overwatch::lock::pid_alive` reports a LIVE lock
//! holder as DEAD when it cannot spawn `kill`.
//!
//! `crates/overwatch/src/lock.rs:78-91`:
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
//! Consumed at `lock.rs:273`, inside the acquire retry loop:
//! ```ignore
//! Some(existing) if !pid_alive(existing.pid) => {
//!     let _ = std::fs::remove_file(&path);   // reap someone else's LIVE lock
//!     continue;
//! }
//! ```
//! `unwrap_or(false)` maps "I could not ask the OS" to "the holder is gone",
//! which is the strictly less safe answer: the lock is STOLEN from a live
//! holder and the caller proceeds into the very load->check->save window the
//! lock exists to close.
//!
//! ## Why it is driven through a caller
//!
//! `pid_alive` is private, and `mod lock` is private in `lib.rs`
//! ("Kept private: it is an internal concurrency primitive, not public API"),
//! so neither is reachable from `tests/`. Making either public would be a
//! PRODUCTION CHANGE, which this work explicitly forbids. Instead the fault is
//! driven through `store::append_disposition` (pub), which is one of the
//! documented `acquire_or_skip` callers: contended -> `SkippedContended`
//! (hard-skip), reaped -> `Recorded`. That difference is the observation.
//!
//! ## Why `PATH` and why one `#[test]`
//!
//! `Command::new("kill")` resolves via `PATH` and does NOT go through a shell,
//! so the `kill` shell builtin cannot rescue it. Verified empirically on this
//! machine (macOS, aarch64) before writing this test:
//!   normal PATH:       Ok(ExitStatus(unix_wait_status(0)))  -> alive=true
//!   empty-dir PATH:    Err(Os { code: 2, kind: NotFound })  -> alive=false
//!   empty-string PATH: Err(Os { code: 2, kind: NotFound })  -> alive=false
//!
//! `std::env::set_var` is PROCESS-global, and integration tests inside one test
//! binary run in parallel threads. This file therefore contains exactly ONE
//! `#[test]`, which runs the control phase and the fault phase SEQUENTIALLY, so
//! no sibling test can observe the mutated `PATH`/`HOME`. Do not add a second
//! `#[test]` to this file: it would race the `set_var` window.

use overwatch::disposition::{Disposition, DispositionVerdict};
use overwatch::store::{self, AppendOutcome};
use std::path::PathBuf;
use std::time::Instant;

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("overwatch-fi-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn live_lock_holder_is_stolen_when_kill_cannot_be_spawned() {
    let home = temp_dir("pidalive-home");
    let cwd = temp_dir("pidalive-cwd");
    // Sandbox the HOME-derived storage root so this never touches the real
    // ~/.overwatch. Safe to set_var here: single-`#[test]` file (see module doc).
    std::env::set_var("HOME", &home);

    // A lock held by a pid that is unambiguously ALIVE: this very process.
    let live_pid = std::process::id();
    let lock_path = store::leases_path(&cwd).unwrap().with_extension("lock");
    std::fs::create_dir_all(lock_path.parent().unwrap()).unwrap();
    std::fs::write(
        &lock_path,
        format!("{{\"pid\":{live_pid},\"acquired_at\":0}}"),
    )
    .unwrap();

    let d = |id: &str| {
        Disposition::new(
            id.to_string(),
            DispositionVerdict::Confirmed,
            "tester".to_string(),
            1_000,
        )
    };

    // ---- CONTROL: `kill` resolvable. The live holder must be respected, so
    // the append HARD-SKIPS (nothing persisted) after the 10s deadline.
    let t0 = Instant::now();
    let control = store::append_disposition(&cwd, &d("finding-control")).unwrap();
    let control_elapsed = t0.elapsed();
    assert_eq!(
        control,
        AppendOutcome::SkippedContended,
        "control precondition: with `kill` resolvable, a lock held by this live \
         pid must be respected and the append hard-skipped (elapsed {control_elapsed:?})"
    );
    assert!(
        lock_path.exists(),
        "control precondition: the live holder's lock must NOT have been reaped"
    );

    // ---- FAULT: make `kill` unspawnable. Nothing else changes.
    let empty_path_dir = temp_dir("pidalive-emptypath");
    let prev_path = std::env::var_os("PATH");
    std::env::set_var("PATH", &empty_path_dir);

    let t1 = Instant::now();
    let fault = store::append_disposition(&cwd, &d("finding-fault"));
    let fault_elapsed = t1.elapsed();
    let lock_survived = lock_path.exists();

    // Restore PATH BEFORE asserting, so a failing assert cannot leave the rest
    // of this process (tempdir cleanup, panic machinery) without a PATH.
    match prev_path {
        Some(p) => std::env::set_var("PATH", p),
        None => std::env::remove_var("PATH"),
    }

    let fault = fault.unwrap();
    assert_eq!(
        fault,
        AppendOutcome::SkippedContended,
        "FAIL-OPEN: `kill` could not be spawned (empty PATH), so pid_alive's \
         `.unwrap_or(false)` reported pid {live_pid} — THIS RUNNING PROCESS — as \
         dead. The live holder's lock was reaped and the guarded append went \
         through in {fault_elapsed:?} (control took {control_elapsed:?} and \
         correctly skipped). 'I cannot determine whether the holder is alive' \
         was resolved to 'the holder is dead' — the permissive side."
    );
    assert!(
        lock_survived,
        "FAIL-OPEN: the LIVE holder's lock file was deleted (reaped) because \
         `kill` could not be spawned."
    );
}
