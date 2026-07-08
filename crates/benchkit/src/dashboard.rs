//! Append-only JSONL run dashboard for benchkit.
//!
//! Each SWE-bench run produces one [`RunRecord`] (resolution rate, per-instance
//! pass/fail, model, cost, and a caller-supplied timestamp). [`append_run`]
//! writes it as exactly one JSON line to a benchkit-owned `runs.jsonl` store
//! under a caller-supplied state dir; [`query_runs`] reads them back (optionally
//! filtered by a timestamp range) for later use by the CI gate.
//!
//! Design invariant: this path is pure and deterministic — the timestamp is an
//! input parameter, never read from the clock inside [`append_run`], so tests
//! stay hermetic (no network, no clock).
//!
//! ## Why not reuse `harness-core`'s `gate::run::append_jsonl`?
//!
//! harness-core exposes `append_jsonl(state_dir: &Path, entry: &serde_json::Value)`,
//! whose `(state_dir, Value)` shape is close but does not fit here for two
//! concrete reasons:
//!   1. It hardcodes the filename to `<state_dir>/log.jsonl` — it is the *shared
//!      Stop-gate event-log sink* (donegate/reviewgate/tdd). benchkit needs its
//!      own distinct `runs.jsonl` store; routing dashboard rows into the gates'
//!      `log.jsonl` would collide with unrelated observability events.
//!   2. It is deliberately best-effort: it swallows every serialization/IO error
//!      so an observability log can never break the turn it records. A dashboard
//!      append that feeds a CI gate must instead surface IO failures (we return
//!      `anyhow::Result`).
//!
//! Hence this small benchkit-local append, rather than adding a harness-core
//! dependency for a helper whose fixed filename and error-swallowing semantics
//! we cannot use.

use std::path::Path;

use anyhow::Context;
use serde::{Deserialize, Serialize};

/// Per-instance outcome within a single run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceResult {
    /// SWE-bench instance identifier (e.g. `django__django-12345`).
    pub instance_id: String,
    /// Whether the harness resolved (passed) this instance.
    pub resolved: bool,
}

/// One dashboard row: the summary of a single benchkit run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunRecord {
    /// Caller-supplied run timestamp as epoch seconds. Passed in (never read
    /// from the clock here) so the append path stays deterministic/testable.
    pub timestamp: i64,
    /// Fraction of instances resolved, in `[0.0, 1.0]`.
    pub resolution_rate: f64,
    /// Per-instance pass/fail results for this run.
    pub instances: Vec<InstanceResult>,
    /// Model identifier the run used.
    pub model: String,
    /// Reported cost of the run (currency-agnostic).
    pub cost: f64,
}

/// Filename of the benchkit-owned append-only run store, under the state dir.
const RUNS_FILE: &str = "runs.jsonl";

/// Append `record` as exactly one JSON line to `<state_dir>/runs.jsonl`,
/// creating the state dir if needed. Append-only: writing N runs yields N
/// lines, never clobbering earlier rows.
pub fn append_run(state_dir: &Path, record: &RunRecord) -> anyhow::Result<()> {
    std::fs::create_dir_all(state_dir)
        .with_context(|| format!("creating state dir {}", state_dir.display()))?;
    let path = state_dir.join(RUNS_FILE);
    let line = serde_json::to_string(record).context("serializing RunRecord")?;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("opening {} for append", path.display()))?;
    use std::io::Write;
    writeln!(f, "{line}").with_context(|| format!("appending to {}", path.display()))?;
    Ok(())
}

/// Read back all [`RunRecord`]s from `<state_dir>/runs.jsonl`, optionally
/// filtered to those whose `timestamp` falls within the inclusive `range`
/// `(start, end)`. Returns an empty vec when the store does not exist yet.
pub fn query_runs(state_dir: &Path, range: Option<(i64, i64)>) -> anyhow::Result<Vec<RunRecord>> {
    let path = state_dir.join(RUNS_FILE);
    let body = match std::fs::read_to_string(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => {
            return Err(e).with_context(|| format!("reading {}", path.display()));
        }
    };
    let mut out = Vec::new();
    for (i, line) in body.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let rec: RunRecord = serde_json::from_str(line)
            .with_context(|| format!("parsing {} line {}", path.display(), i + 1))?;
        if let Some((start, end)) = range {
            if rec.timestamp < start || rec.timestamp > end {
                continue;
            }
        }
        out.push(rec);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(ts: i64, model: &str) -> RunRecord {
        RunRecord {
            timestamp: ts,
            resolution_rate: 0.5,
            instances: vec![
                InstanceResult {
                    instance_id: "django__django-1".to_string(),
                    resolved: true,
                },
                InstanceResult {
                    instance_id: "flask__flask-2".to_string(),
                    resolved: false,
                },
            ],
            model: model.to_string(),
            cost: 1.25,
        }
    }

    #[test]
    fn append_two_runs_yields_two_lines_and_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("nested"); // parent does not exist yet
        let r1 = sample(100, "opus");
        let r2 = sample(200, "sonnet");
        append_run(&state, &r1).unwrap();
        append_run(&state, &r2).unwrap();

        // Exactly one line per run, no clobber.
        let body = std::fs::read_to_string(state.join("runs.jsonl")).unwrap();
        assert_eq!(
            body.lines().count(),
            2,
            "appending two runs yields exactly two lines"
        );

        let got = query_runs(&state, None).unwrap();
        assert_eq!(got.len(), 2);
        // Round-trip equality: resolution_rate, per-instance list, model, cost.
        assert_eq!(got[0], r1);
        assert_eq!(got[1], r2);
    }

    #[test]
    fn query_filters_by_timestamp_range() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path();
        append_run(state, &sample(100, "a")).unwrap();
        append_run(state, &sample(200, "b")).unwrap();
        append_run(state, &sample(300, "c")).unwrap();

        let mid = query_runs(state, Some((150, 250))).unwrap();
        assert_eq!(mid.len(), 1);
        assert_eq!(mid[0].timestamp, 200);

        let all = query_runs(state, None).unwrap();
        assert_eq!(all.len(), 3);
    }

    #[test]
    fn query_missing_store_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let got = query_runs(&dir.path().join("absent"), None).unwrap();
        assert!(got.is_empty());
    }
}
