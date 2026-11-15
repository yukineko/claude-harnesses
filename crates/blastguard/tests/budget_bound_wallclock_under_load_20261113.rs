//! RED 2026-11-13: `depth_limit_fail_open::budget_bound_is_still_reachable_with_its_own_reason`
//! asserts a 2s wall-clock bound that a CPU-loaded host exceeds.
//!
//! Observed 2026-11-13 (main 45fd3365, 12 CPUs): unloaded, that test takes
//! 1.6-1.8s (cargo "finished in", debug build) — 80-92% of its 2s bound. With
//! 18 busy `yes` processes it failed 5/5, every time at
//! depth_limit_fail_open.rs:499 (the `elapsed < 2s` assert); its Deny assert
//! was never reached.
//!
//! This file applies the load itself (1.5 x available_parallelism busy-spin
//! threads, stopped by an AtomicBool and a hard deadline, joined before the
//! asserts) and runs the same input through the same public entry point:
//!
//! - the ignored RED case asserts the same `< 2s` bound and fails under load;
//! - the non-ignored control asserts the decision under the same load is still
//!   the exact budget Deny — the budget is a node count
//!   (`MAX_ANALYSIS_NODES`), not time — so the defect is wall-clock only.
//!
//! `budget_hog()` and the Deny reason are copied from depth_limit_fail_open.rs
//! (that file is a test target with no shareable module; it is not modified).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use blastguard::detect::detect;
use blastguard::model::Decision;
use serde_json::json;

fn analyse(cmd: &str) -> Decision {
    detect("Bash", Some(&json!({ "command": cmd })))
}

/// Copied verbatim from depth_limit_fail_open.rs `budget_hog()`.
fn budget_hog() -> String {
    let unit = "find . -exec find . -exec find . -exec echo {} + + +";
    vec![unit; 300].join(" ; ")
}

const BUDGET_DENY: &str = "command is too complex to analyse within the safety budget — blastguard cannot vouch for it either way";

/// Busy-spin threads that stop when `stop` is set or after a hard deadline,
/// whichever comes first, so a panicking caller cannot leave them running.
struct Load {
    stop: Arc<AtomicBool>,
    threads: Vec<JoinHandle<u64>>,
}

impl Load {
    fn start() -> Load {
        let cpus = std::thread::available_parallelism()
            .map(|n| n.get())
            .expect("available_parallelism");
        let n = cpus + cpus.div_ceil(2);
        let stop = Arc::new(AtomicBool::new(false));
        let hard_deadline = Instant::now() + Duration::from_secs(60);
        let threads = (0..n)
            .map(|_| {
                let stop = Arc::clone(&stop);
                std::thread::spawn(move || {
                    let mut spins: u64 = 0;
                    while !stop.load(Ordering::Relaxed) && Instant::now() < hard_deadline {
                        for _ in 0..10_000 {
                            spins = std::hint::black_box(spins.wrapping_add(1));
                        }
                    }
                    spins
                })
            })
            .collect();
        // Let the spinners get scheduled before measuring.
        std::thread::sleep(Duration::from_millis(300));
        Load { stop, threads }
    }

    fn threads(&self) -> usize {
        self.threads.len()
    }
}

impl Drop for Load {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

/// Run `analyse(budget_hog())` while the host is loaded; returns
/// (decision, elapsed, spinner count). The load is stopped and joined before
/// returning.
fn analyse_under_load() -> (Decision, Duration, usize) {
    let cmd = budget_hog();
    let load = Load::start();
    let n = load.threads();
    let start = Instant::now();
    let d = analyse(&cmd);
    let elapsed = start.elapsed();
    drop(load);
    (d, elapsed, n)
}

/// Control: under the same load the decision is unchanged — the exact budget
/// Deny after hardening. The defect is the wall-clock assert, not the verdict.
#[test]
fn control_decision_under_load_is_still_the_exact_budget_deny() {
    let (d, elapsed, n) = analyse_under_load();
    eprintln!("under load ({n} spinners): analysis took {elapsed:?}");
    assert_eq!(
        d.clone().hardened(),
        Decision::deny(BUDGET_DENY),
        "under load ({n} spinners, {elapsed:?}) the decision must still be the budget Deny"
    );
}

#[test]
#[ignore = "RED 2026-11-13: depth_limit_fail_open:499 asserts analysis < 2s wall clock; under CPU load (1.5x cores busy) budget_hog takes longer"]
fn budget_hog_analysis_stays_under_2s_wall_clock_under_load() {
    let (_d, elapsed, n) = analyse_under_load();
    assert!(
        elapsed < Duration::from_secs(2),
        "a PreToolUse hook must never hang the turn: analysis took {elapsed:?} \
         with {n} busy-spin threads"
    );
}
