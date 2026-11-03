//! Thin adapter over `harness_core::gate::runner`: run one [`Check`] as a
//! subprocess and decorate the raw outcome with donegate's metadata (check name,
//! command line, duration, optionality). The dangerous spawn/timeout/bounded-tail
//! logic lives in harness-core; this only maps types and picks the per-run
//! temporary log path.
//!
//! The full combined output is streamed to a temporary log that is unique per
//! process and run (`<state_dir>/tmp/<pid>-<n>-<check>.log`). If that directory
//! or file cannot be created, the log goes to the OS temp dir instead, so the
//! check still RUNS (a log-file problem must never turn into "the check did not
//! run"). A passing check's log is deleted; a failing check's log is handed on as
//! [`Record::Pending`] and moved into the durable session record by
//! [`crate::gate::save_records`].

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Instant;

use harness_core::gate::runner;

use crate::config::Check;
use crate::record::{sanitize, Record};

pub struct Outcome {
    pub name: String,
    pub cmd: String,
    pub passed: bool,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub spawn_error: Option<String>,
    pub duration_secs: f64,
    /// A bounded window of the end of the combined output (at most
    /// `harness_core::gate::runner::TAIL_CAP_BYTES`), or the spawn error. The
    /// display is DERIVED from it ([`crate::record::digest`]); it is never pasted
    /// whole into the block reason.
    pub output_tail: String,
    pub optional: bool,
    /// Where the full output is kept.
    pub record: Record,
}

impl Outcome {
    /// A short status verb for reports.
    pub fn status(&self) -> &'static str {
        if self.passed {
            "ok"
        } else if self.timed_out {
            "timeout"
        } else if self.spawn_error.is_some() {
            "spawn-error"
        } else {
            "fail"
        }
    }
}

static RUN_SEQ: AtomicU32 = AtomicU32::new(0);

/// A writable temporary log path: under `tmp_dir` if it can be created there,
/// else under the OS temp dir.
fn temp_log_path(tmp_dir: &Path, check: &str) -> PathBuf {
    let n = RUN_SEQ.fetch_add(1, Ordering::Relaxed);
    let file = format!("{}-{n}-{}.log", std::process::id(), sanitize(check));
    let primary = tmp_dir.join(&file);
    if std::fs::create_dir_all(tmp_dir).is_ok() && std::fs::File::create(&primary).is_ok() {
        return primary;
    }
    std::env::temp_dir().join(format!("donegate-{file}"))
}

pub fn run_check(check: &Check, root: &Path, default_timeout: u64, tmp_dir: &Path) -> Outcome {
    let timeout = check.timeout_secs.unwrap_or(default_timeout).max(1);
    let workdir = match &check.workdir {
        Some(w) => root.join(w),
        None => root.to_path_buf(),
    };
    let log_path = temp_log_path(tmp_dir, &check.name);

    let start = Instant::now();
    // usize::MAX lines: keep the whole bounded byte window for the digest.
    let raw = runner::run(&check.cmd, &workdir, timeout, usize::MAX, &log_path);

    let record = if raw.passed {
        let _ = std::fs::remove_file(&log_path);
        Record::None
    } else if log_path.exists() {
        Record::Pending(log_path)
    } else {
        Record::NotSaved {
            why: "the output log could not be created".to_string(),
            temp: None,
            tail_lines: crate::record::FALLBACK_MIN_LINES,
        }
    };

    Outcome {
        name: check.name.clone(),
        cmd: check.cmd.clone(),
        passed: raw.passed,
        exit_code: raw.exit_code,
        timed_out: raw.timed_out,
        spawn_error: raw.spawn_error,
        duration_secs: start.elapsed().as_secs_f64(),
        output_tail: raw.output_tail,
        optional: check.optional,
        record,
    }
}
