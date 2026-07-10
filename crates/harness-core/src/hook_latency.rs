//! Stop-hook latency ledger — one SHARED central append-only log so that
//! aggregation is a single file read, not a per-gate scrape.
//!
//! The latency-critical gates (the three 600s-timeout Stop hooks:
//! donegate/reviewgate/propguard) each `record` how long their Stop handler took
//! into `<base_dir("harness")>/state/hook-latency.jsonl`. `harness-status hooks`
//! reads that one file, groups by session, and warns when a session's combined
//! Stop-hook wall-time exceeds a configurable budget.
//!
//! Resilience: recording is BEST-EFFORT and mirrors `gate::run::append_jsonl` —
//! every IO/serialization error is swallowed. An observability log must never
//! break the turn it is measuring.

use std::path::{Path, PathBuf};

/// One recorded Stop-hook execution.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
pub struct LatencyEntry {
    pub ts: String,      // rfc3339 local time
    pub hook: String,    // gate name, e.g. "donegate"
    pub session: String, // session id ("" if unknown)
    pub elapsed_ms: u64,
}

/// The single central ledger path: `<base_dir("harness")>/state/hook-latency.jsonl`.
pub fn ledger_path() -> PathBuf {
    crate::config::base_dir("harness")
        .join("state")
        .join("hook-latency.jsonl")
}

/// Append one [`LatencyEntry`] for a completed Stop-hook run to the central
/// ledger. BEST-EFFORT: all IO/serialization errors are swallowed so an
/// observability log can never break the turn it records.
pub fn record(hook: &str, session: &str, elapsed_ms: u64) {
    record_to(&ledger_path(), hook, session, elapsed_ms);
}

/// Testable core of [`record`]: append one entry line to `path`, creating parent
/// dirs. Best-effort — errors are swallowed (mirrors `gate::run::append_jsonl`).
pub fn record_to(path: &Path, hook: &str, session: &str, elapsed_ms: u64) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let entry = LatencyEntry {
        ts: chrono::Local::now().to_rfc3339(),
        hook: hook.to_string(),
        session: session.to_string(),
        elapsed_ms,
    };
    if let Ok(line) = serde_json::to_string(&entry) {
        // Single atomic append (body + '\n' in one write) — see issue #15.
        crate::append::append_line(path, &line);
    }
}

/// Per-session aggregation of Stop-hook latency.
pub struct SessionLatency {
    pub session: String,
    pub total_ms: u64,
    pub per_hook: Vec<(String, u64)>,
}

/// Read the JSONL ledger at `path`, group entries by session, and sum
/// `elapsed_ms` both overall (`total_ms`) and per hook (`per_hook`).
///
/// Repeat policy: if a hook appears multiple times within one session (e.g. the
/// gate blocked, the agent retried, and it ran again on the next Stop) every
/// entry is SUMMED — the per_hook figure is the cumulative wall-time that hook
/// cost that session, not just its latest run. Sessions are sorted by `total_ms`
/// descending (slowest first). Malformed lines are skipped silently, and a
/// missing file yields an empty vec (never panics).
///
/// `budget_ms` is currently unused by the grouping itself; it is accepted so the
/// signature stays aligned with [`over_budget`] and future budget-aware sorting.
pub fn aggregate(path: &Path, _budget_ms: u64) -> Vec<SessionLatency> {
    let body = match std::fs::read_to_string(path) {
        Ok(b) => b,
        Err(_) => return Vec::new(),
    };

    // Preserve first-seen order of sessions and of hooks within a session so the
    // output is deterministic before the final total_ms sort.
    let mut order: Vec<String> = Vec::new();
    let mut totals: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
    let mut per_hook: std::collections::HashMap<String, Vec<(String, u64)>> =
        std::collections::HashMap::new();

    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let entry: LatencyEntry = match serde_json::from_str(line) {
            Ok(e) => e,
            Err(_) => continue, // skip malformed lines silently
        };
        if !totals.contains_key(&entry.session) {
            order.push(entry.session.clone());
        }
        *totals.entry(entry.session.clone()).or_insert(0) += entry.elapsed_ms;
        let hooks = per_hook.entry(entry.session.clone()).or_default();
        if let Some(slot) = hooks.iter_mut().find(|(h, _)| *h == entry.hook) {
            slot.1 += entry.elapsed_ms;
        } else {
            hooks.push((entry.hook.clone(), entry.elapsed_ms));
        }
    }

    let mut out: Vec<SessionLatency> = order
        .into_iter()
        .map(|session| SessionLatency {
            total_ms: totals.get(&session).copied().unwrap_or(0),
            per_hook: per_hook.remove(&session).unwrap_or_default(),
            session,
        })
        .collect();
    // Slowest sessions first; stable so equal totals keep first-seen order.
    out.sort_by_key(|s| std::cmp::Reverse(s.total_ms));
    out
}

/// The sessions whose combined Stop-hook `total_ms` strictly exceeds `budget_ms`.
pub fn over_budget(sessions: &[SessionLatency], budget_ms: u64) -> Vec<&SessionLatency> {
    sessions.iter().filter(|s| s.total_ms > budget_ms).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        std::env::temp_dir()
            .join(format!("hc-hooklat-{}-{tag}", std::process::id()))
            .join("state")
            .join("hook-latency.jsonl")
    }

    #[test]
    fn record_to_writes_one_line_per_call_and_creates_dirs() {
        let path = tmp("record");
        let _ = std::fs::remove_dir_all(path.parent().unwrap().parent().unwrap());
        assert!(!path.exists(), "parent dirs do not exist yet");
        record_to(&path, "donegate", "sess-a", 100);
        record_to(&path, "reviewgate", "sess-a", 50);
        let body = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines.len(), 2, "each call appends exactly one line");
        assert!(lines[0].contains("\"donegate\"") && lines[0].contains("100"));
        assert!(lines[1].contains("\"reviewgate\""));
        let _ = std::fs::remove_dir_all(path.parent().unwrap().parent().unwrap());
    }

    #[test]
    fn aggregate_groups_sums_and_sorts() {
        let path = tmp("agg");
        let _ = std::fs::remove_dir_all(path.parent().unwrap().parent().unwrap());
        // sess-a: donegate 100 + reviewgate 50 = 150
        record_to(&path, "donegate", "sess-a", 100);
        record_to(&path, "reviewgate", "sess-a", 50);
        // sess-b: donegate 200 + donegate 100 (repeat → SUM) = 300 (slower → first)
        record_to(&path, "donegate", "sess-b", 200);
        record_to(&path, "donegate", "sess-b", 100);

        let agg = aggregate(&path, 0);
        assert_eq!(agg.len(), 2);
        assert_eq!(agg[0].session, "sess-b", "slowest session sorts first");
        assert_eq!(agg[0].total_ms, 300);
        assert_eq!(agg[0].per_hook, vec![("donegate".to_string(), 300)]);
        assert_eq!(agg[1].session, "sess-a");
        assert_eq!(agg[1].total_ms, 150);
        assert_eq!(
            agg[1].per_hook,
            vec![
                ("donegate".to_string(), 100),
                ("reviewgate".to_string(), 50)
            ]
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap().parent().unwrap());
    }

    #[test]
    fn over_budget_filters_strictly() {
        let path = tmp("budget");
        let _ = std::fs::remove_dir_all(path.parent().unwrap().parent().unwrap());
        record_to(&path, "donegate", "sess-a", 150);
        record_to(&path, "donegate", "sess-b", 300);
        let agg = aggregate(&path, 0);
        let over = over_budget(&agg, 200);
        assert_eq!(over.len(), 1);
        assert_eq!(over[0].session, "sess-b");
        // budget exactly at total is NOT over (strict >).
        assert!(over_budget(&agg, 300).is_empty());
        let _ = std::fs::remove_dir_all(path.parent().unwrap().parent().unwrap());
    }

    #[test]
    fn aggregate_skips_malformed_lines() {
        let path = tmp("malformed");
        let _ = std::fs::remove_dir_all(path.parent().unwrap().parent().unwrap());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "not json\n\
             {\"ts\":\"t\",\"hook\":\"donegate\",\"session\":\"s\",\"elapsed_ms\":42}\n\
             {broken\n",
        )
        .unwrap();
        let agg = aggregate(&path, 0);
        assert_eq!(agg.len(), 1);
        assert_eq!(agg[0].total_ms, 42);
        let _ = std::fs::remove_dir_all(path.parent().unwrap().parent().unwrap());
    }

    #[test]
    fn aggregate_missing_file_is_empty_never_panics() {
        let path = tmp("missing");
        let _ = std::fs::remove_dir_all(path.parent().unwrap().parent().unwrap());
        assert!(!path.exists());
        assert!(aggregate(&path, 30_000).is_empty());
    }
}
