//! Retrieval-event ledger — the read-side counterpart to the lessons store.
//!
//! Where [`crate::lessons`] records what was *learned* (the write side), this
//! module records what was *retrieved/injected*: each time a run pulls lessons
//! into an interpreter's context, one [`RetrievalEvent`] is appended here. That
//! makes the **retrieval hit rate** (how often an injection actually found a
//! matching lesson) machine-observable — the read-side instrumentation that
//! mirrors capture-rate on the write side (`lessons::stats`).
//!
//! Design deliberately mirrors the lessons store:
//!   * append-only JSONL, one event per line, in the **same** machine-global
//!     dir as the lessons store (sibling `retrieval.jsonl`, honoring the same
//!     `LESSONS_STORE_DIR` absolute override transitively via
//!     [`crate::lessons::store_path`]);
//!   * `record` is **idempotent by `run_id`** — one run contributes at most one
//!     retrieval event, so re-recording the same run is a no-op (the same
//!     idempotency-by-key precedent the lessons/discovery stores use);
//!   * `load` is fail-soft: missing file → empty Vec, malformed lines skipped,
//!     never panics (load-bearing: called from the injection path);
//!   * the append critical section reuses the lessons store's advisory
//!     lockfile helper so concurrent same-machine writers can't interleave.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// One lesson-retrieval/injection event. Serde-serializable to one JSONL line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetrievalEvent {
    /// The run/session the retrieval happened for — the idempotency key
    /// (one run contributes at most one event).
    pub run_id: String,
    /// Short summary of the query used (provenance / debugging).
    pub query_summary: String,
    /// Whether the deterministic search found at least one matching lesson.
    pub hit: bool,
    /// The ids of the lessons that were injected (empty on a zero-hit event).
    pub lesson_ids: Vec<String>,
    /// The `k` cutoff the search used.
    pub k: usize,
    /// Epoch seconds when recorded.
    pub ts: u64,
}

/// Project-INDEPENDENT path to the retrieval ledger: `retrieval.jsonl` sibling
/// of the lessons store (same dir, same `LESSONS_STORE_DIR` override). Falls
/// back to a bare `retrieval.jsonl` only if the lessons path has no parent,
/// which never happens for the real store path.
pub fn store_path() -> PathBuf {
    crate::lessons::store_path()
        .parent()
        .map(|d| d.join("retrieval.jsonl"))
        .unwrap_or_else(|| PathBuf::from("retrieval.jsonl"))
}

/// Record a retrieval event, **idempotent by `run_id`**: if an event for the
/// same run is already stored, nothing is written and the count is unchanged.
/// Fail-soft: on any IO/serialize error (or lock contention past the retry
/// budget) the event is silently dropped — never panics.
pub fn record(event: &RetrievalEvent) {
    record_at(&store_path(), event);
}

/// Internal: append to an explicit path. Used by `record` and by tests.
///
/// The read-check-append critical section (load → run_id-exists check → write)
/// is guarded by the lessons store's advisory lockfile helper so concurrent
/// same-machine processes can't interleave writes or race the idempotency
/// check. Fail-soft throughout.
fn record_at(path: &Path, event: &RetrievalEvent) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let Some(_guard) = crate::lessons::acquire_lock(path) else {
        return;
    };

    // Idempotency-by-run_id: one run = one event. Holding the lock across this
    // check-then-write is what makes the guarantee hold under concurrency.
    if load_at(path).iter().any(|e| e.run_id == event.run_id) {
        return;
    }

    let Ok(json) = serde_json::to_string(event) else {
        return;
    };
    // Single atomic append (body + '\n' in one write) — see issue #15.
    crate::append::append_line(path, &json);
}

/// Load all retrieval events. Missing file → empty Vec, blank/corrupt lines
/// skipped. Never panics (fail-soft).
pub fn load() -> Vec<RetrievalEvent> {
    load_at(&store_path())
}

/// Internal: load from an explicit path. Used by `load` and by tests.
fn load_at(path: &Path) -> Vec<RetrievalEvent> {
    let mut events = Vec::new();

    let Ok(contents) = std::fs::read_to_string(path) else {
        return events;
    };

    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(e) = serde_json::from_str::<RetrievalEvent>(line) {
            events.push(e);
        }
    }

    events
}

/// A deterministic roll-up of the retrieval ledger, for hit-rate observability.
///
/// `hits` counts events whose search actually found a lesson, so the retrieval
/// **hit rate = `hits` / `total`** is derivable from counts alone.
/// `distinct_runs` is the number of separate runs that recorded a retrieval —
/// the read-side counterpart of the lessons store's capture-rate `source_runs`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RetrievalStats {
    pub total: usize,
    pub hits: usize,
    pub distinct_runs: usize,
}

/// Deterministically aggregate a retrieval-event slice (a **pure function — no
/// AI, no IO**). An empty slice yields `RetrievalStats { total: 0, hits: 0,
/// distinct_runs: 0 }`, the same fail-soft zero shape a missing ledger produces
/// (its [`load`] returns `[]`).
pub fn retrieval_stats(events: &[RetrievalEvent]) -> RetrievalStats {
    let mut runs: BTreeSet<&str> = BTreeSet::new();
    let mut hits = 0usize;
    for e in events {
        runs.insert(e.run_id.as_str());
        if e.hit {
            hits += 1;
        }
    }
    RetrievalStats {
        total: events.len(),
        hits,
        distinct_runs: runs.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(run: &str, hit: bool, ids: &[&str]) -> RetrievalEvent {
        RetrievalEvent {
            run_id: run.to_string(),
            query_summary: "some query".to_string(),
            hit,
            lesson_ids: ids.iter().map(|s| s.to_string()).collect(),
            k: 3,
            ts: 1000,
        }
    }

    #[test]
    fn retrieval_stats_empty_is_fail_soft_zero_shape() {
        let s = retrieval_stats(&[]);
        assert_eq!(s.total, 0);
        assert_eq!(s.hits, 0);
        assert_eq!(s.distinct_runs, 0);
        // and it serializes to exactly the documented zero shape.
        let j = serde_json::to_string(&s).unwrap();
        assert_eq!(j, r#"{"total":0,"hits":0,"distinct_runs":0}"#);
    }

    #[test]
    fn retrieval_stats_counts_total_hits_and_distinct_runs() {
        // 4 events from 3 distinct runs; 3 of them hit, 1 missed.
        let events = vec![
            ev("r1", true, &["a", "b"]),
            ev("r1", true, &["a"]), // same run again (distinct_runs still counts r1 once)
            ev("r2", false, &[]),
            ev("r3", true, &["c"]),
        ];
        let s = retrieval_stats(&events);
        assert_eq!(s.total, 4, "total is the raw event count");
        assert_eq!(s.hits, 3, "hits = events whose search found a lesson");
        assert_eq!(
            s.distinct_runs, 3,
            "distinct_runs collapses repeated run_ids"
        );
        // hit rate = hits/total is derivable from the counts alone.
        assert_eq!(s.hits as f64 / s.total as f64, 0.75);
    }

    #[test]
    fn retrieval_stats_all_misses_has_zero_hits() {
        let events = vec![ev("r1", false, &[]), ev("r2", false, &[])];
        let s = retrieval_stats(&events);
        assert_eq!(s.total, 2);
        assert_eq!(s.hits, 0, "a zero-hit ledger has hit rate 0");
        assert_eq!(s.distinct_runs, 2);
    }

    #[test]
    fn record_is_idempotent_by_run_id() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("retrieval.jsonl");

        record_at(&path, &ev("run-A", true, &["l1"]));
        // Re-record the SAME run_id (even with a different body) must NOT grow
        // the ledger — one run contributes at most one event.
        record_at(&path, &ev("run-A", false, &[]));

        let loaded = load_at(&path);
        assert_eq!(
            loaded.len(),
            1,
            "same run_id re-record must not increase count"
        );
        // The first write wins (no overwrite).
        assert!(loaded[0].hit, "first-write-wins: original hit=true kept");

        // A genuinely new run does append.
        record_at(&path, &ev("run-B", true, &["l2"]));
        assert_eq!(load_at(&path).len(), 2);
    }

    #[test]
    fn load_is_fail_soft_missing_and_corrupt() {
        // Missing file → empty Vec, never a panic.
        assert!(load_at(Path::new("/nonexistent/retrieval.jsonl")).is_empty());

        // Corrupt lines are skipped, valid ones kept.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("retrieval.jsonl");
        let valid = serde_json::to_string(&ev("v", true, &["x"])).unwrap();
        let content = format!("{valid}\n{{ not json\n\n{valid}\n");
        std::fs::write(&path, content).unwrap();
        // Two identical valid lines — load keeps both (dedup is record's job).
        let loaded = load_at(&path);
        assert_eq!(loaded.len(), 2, "corrupt/blank lines skipped, valid kept");
    }

    #[test]
    fn round_trip_event_serde() {
        let e = ev("run-X", true, &["l1", "l2"]);
        let j = serde_json::to_string(&e).unwrap();
        let back: RetrievalEvent = serde_json::from_str(&j).unwrap();
        assert_eq!(back, e);
    }

    #[test]
    fn store_path_is_sibling_of_lessons_store() {
        // Same dir as the lessons store, filename retrieval.jsonl.
        let rp = store_path();
        assert!(
            rp.ends_with("retrieval.jsonl"),
            "retrieval path tail: {rp:?}"
        );
        assert_eq!(
            rp.parent(),
            crate::lessons::store_path().parent(),
            "retrieval ledger lives beside the lessons store"
        );
    }
}
