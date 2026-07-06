//! Per-run span store: append-only JSONL on top of the shared span model.
//!
//! The [`Span`] schema and its defensive loader now live in
//! [`harness_core::spans`] — the single source of truth shared with replaykit,
//! so a recorded span round-trips between the two crates. This module keeps the
//! tracekit-only storage layer: where a run's spans live on disk
//! (`~/.tracekit/<run_id>/spans.jsonl`), and the append-only writer.
//!
//! Spans are appended as JSONL — append-only so concurrent phase completions
//! never clobber each other, and a partial run is still readable.

use std::path::PathBuf;

use anyhow::{Context, Result};
use harness_core::config::base_dir;

pub use harness_core::spans::{load_from, Span};

/// Directory holding one run's spans: `~/.tracekit/<run_id>/`.
pub fn run_dir(run_id: &str) -> PathBuf {
    base_dir("tracekit").join(sanitize(run_id))
}

/// `~/.tracekit/<run_id>/spans.jsonl`.
pub fn spans_path(run_id: &str) -> PathBuf {
    run_dir(run_id).join("spans.jsonl")
}

/// Keep a run id filesystem-safe (it flows in from CLI / condukt RIDs).
fn sanitize(run_id: &str) -> String {
    run_id
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Append one span to its run's JSONL store (creating the run dir on first use).
pub fn append(span: &Span) -> Result<PathBuf> {
    use std::io::Write;
    let dir = run_dir(&span.run_id);
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let path = spans_path(&span.run_id);
    let line = serde_json::to_string(span).context("serializing span")?;
    let mut opts = std::fs::OpenOptions::new();
    opts.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts
        .open(&path)
        .with_context(|| format!("opening {}", path.display()))?;
    writeln!(f, "{line}").with_context(|| format!("appending to {}", path.display()))?;
    Ok(path)
}

/// Load all spans recorded for a run, in record order. Malformed lines are
/// skipped (a corrupt line shouldn't sink the whole trace) but counted in the
/// returned `skipped` so the caller can warn.
pub fn load(run_id: &str) -> Result<(Vec<Span>, usize)> {
    let path = spans_path(run_id);
    load_from(&path).with_context(|| format!("reading {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_strips_path_separators() {
        assert_eq!(sanitize("a/b c:d"), "a_b_c_d");
        assert_eq!(sanitize("run-123_x"), "run-123_x");
    }

    #[cfg(unix)]
    #[test]
    fn append_creates_store_file_with_owner_only_mode() {
        use std::os::unix::fs::PermissionsExt;

        let run_id = format!(
            "tracekit-mode-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system time")
                .as_nanos()
        );
        let span = Span {
            run_id: run_id.clone(),
            span_id: "s1".to_string(),
            parent_id: None,
            name: "test".to_string(),
            phase: "test".to_string(),
            model: None,
            task_id: None,
            ms: 0,
            cost_usd: None,
            status: "ok".to_string(),
            end_unix_ms: 0,
        };

        let path = append(&span).expect("append should succeed");
        let mode = std::fs::metadata(&path)
            .expect("reading metadata")
            .permissions()
            .mode();
        // Clean up the real ~/.tracekit/<run_id> dir this test created, regardless
        // of the assertion outcome below.
        let _ = std::fs::remove_dir_all(run_dir(&run_id));

        assert_eq!(mode & 0o777, 0o600);
    }
}
