//! Thin adapter over `harness_core::gate::runner`: run a test command as a
//! subprocess and map the raw outcome onto tdd's `Outcome`. Shared by `tdd red`
//! and `tdd green`. The dangerous spawn/timeout/bounded-tail logic lives in
//! harness-core; this only picks a unique per-run log path (pid + nanos +
//! sequence, created exclusively with `create_new`, removed after the tail is
//! read) and maps types.

use std::fs::OpenOptions;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use harness_core::gate::runner;

pub struct Outcome {
    pub passed: bool,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub spawn_error: Option<String>,
    pub output_tail: String,
}

/// Run `cmd` in `root` with a timeout, returning the outcome and a bounded tail.
pub fn run_cmd(cmd: &str, root: &Path, timeout: u64, tail_lines: usize, tmp_dir: &Path) -> Outcome {
    // Unique per-run log: a fixed path would be truncated/shared by concurrent runs.
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let _ = std::fs::create_dir_all(tmp_dir);
    let mut log_path = None;
    for _ in 0..16 {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let cand = tmp_dir.join(format!(
            "tdd-run-{}-{}-{}.log",
            std::process::id(),
            nanos,
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        if OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&cand)
            .is_ok()
        {
            log_path = Some(cand);
            break;
        }
    }
    let Some(log_path) = log_path else {
        // No test ran. passed=false, so `tdd green` rejects this. `tdd red`'s
        // judge_red does not inspect spawn_error and would still record a RED
        // (pre-existing, tracked separately); the message is carried in
        // output_tail so such an artifact says why.
        let msg = format!("could not create a unique run log in {}", tmp_dir.display());
        return Outcome {
            passed: false,
            exit_code: None,
            timed_out: false,
            spawn_error: Some(msg.clone()),
            output_tail: msg,
        };
    };
    let raw = runner::run(cmd, root, timeout, tail_lines, &log_path);
    let _ = std::fs::remove_file(&log_path);
    Outcome {
        passed: raw.passed,
        exit_code: raw.exit_code,
        timed_out: raw.timed_out,
        spawn_error: raw.spawn_error,
        output_tail: raw.output_tail,
    }
}
