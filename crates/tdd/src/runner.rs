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

// Declared here, not in `main.rs`: `tests/fault_injection.rs` pulls `proof.rs`
// and `runner.rs` in through `#[path]` with no crate root of its own, so the
// evidence code must be reachable from `runner` alone.
#[path = "evidence.rs"]
pub mod evidence;

pub struct Outcome {
    pub passed: bool,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub spawn_error: Option<String>,
    pub output_tail: String,
    /// The run log read before it is removed, capped at [`FULL_OUTPUT_CAP`]
    /// bytes (lossy UTF-8). Empty when the command never ran or the log could
    /// not be read; only used for the refused-GREEN record file.
    pub full_output: String,
    /// Per-test results by name, parsed from the full run log
    /// ([`crate::evidence::parse_test_log`]).
    pub tests: serde_json::Value,
    /// Counts from the libtest `test result:` lines; `determined: false`
    /// when none could be parsed (never `0 passed`).
    pub counts: serde_json::Value,
}

/// Upper bound on the log bytes kept in [`Outcome::full_output`].
pub const FULL_OUTPUT_CAP: u64 = 4 * 1024 * 1024;

fn read_capped(path: &Path) -> String {
    use std::io::Read;
    let mut buf = Vec::new();
    if let Ok(f) = std::fs::File::open(path) {
        let _ = f.take(FULL_OUTPUT_CAP).read_to_end(&mut buf);
    }
    String::from_utf8_lossy(&buf).into_owned()
}

fn no_parse(why: &str) -> (serde_json::Value, serde_json::Value) {
    (
        serde_json::json!({}),
        serde_json::json!({"determined": false, "reason": why}),
    )
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
        // No test ran. passed=false, but both `tdd red` and `tdd green` check
        // spawn_error via `judge_ran` and refuse (no artifact written); the
        // message is carried in output_tail so any artifact says why.
        let msg = format!("could not create a unique run log in {}", tmp_dir.display());
        let (tests, counts) = no_parse("the test command never ran");
        return Outcome {
            passed: false,
            exit_code: None,
            timed_out: false,
            spawn_error: Some(msg.clone()),
            output_tail: msg,
            full_output: String::new(),
            tests,
            counts,
        };
    };
    let raw = runner::run(cmd, root, timeout, tail_lines, &log_path);
    // Parse the FULL log before it is removed (the tail is bounded).
    let (tests, counts) = if raw.spawn_error.is_some() {
        no_parse("the test command never ran")
    } else {
        evidence::parse_test_log(&log_path)
    };
    let full_output = read_capped(&log_path);
    let _ = std::fs::remove_file(&log_path);
    Outcome {
        full_output,
        tests,
        counts,
        passed: raw.passed,
        exit_code: raw.exit_code,
        timed_out: raw.timed_out,
        spawn_error: raw.spawn_error,
        output_tail: raw.output_tail,
    }
}
