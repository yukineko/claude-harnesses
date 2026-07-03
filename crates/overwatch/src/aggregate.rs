/// Event aggregation and state projection.
use crate::store::{self, LeaseRegistry};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

/// Liveness status of a lease.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LeaseInfo {
    /// Task content key.
    pub key: String,
    /// Human-readable task title.
    pub title: String,
    /// Time since last heartbeat in seconds.
    pub heartbeat_age_secs: i64,
    /// Is this lease stale (past TTL)?
    pub is_stale: bool,
}

/// Session roster: all leases for one session.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionRoster {
    /// Session ID.
    pub session_id: String,
    /// Leases in this session.
    pub leases: Vec<LeaseInfo>,
    /// Live lease count.
    pub live_count: usize,
}

/// Backlog summary.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BacklogSummary {
    /// Total pending items.
    pub pending: usize,
    /// Total done items.
    pub done: usize,
    /// Total deferred items.
    pub deferred: usize,
    /// Pending count per priority (e.g. {"P0": 5, "P1": 3}).
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub pending_by_priority: BTreeMap<String, usize>,
}

/// PDO hypothesis bucket.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HypoBuckets {
    /// Open hypotheses count.
    pub open: usize,
    /// Awaiting measurement count.
    pub awaiting_measurement: usize,
    /// Validated count.
    pub validated: usize,
    /// Rejected count.
    pub rejected: usize,
}

/// Condukt run row.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RunRow {
    /// Run ID.
    pub run_id: String,
    /// Tasks completed.
    pub done: usize,
    /// Tasks total.
    pub total: usize,
    /// Goal description (optional).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub goal: Option<String>,
}

/// The aggregated progress view.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProgressView {
    /// Overwatch ledger: per-session rosters of live leases.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub sessions: Vec<SessionRoster>,
    /// Backlog summary.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backlog: Option<BacklogSummary>,
    /// PDO hypotheses buckets.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hypotheses: Option<HypoBuckets>,
    /// Condukt runs.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub runs: Vec<RunRow>,
    /// Compass gap (north_star / current gap).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compass_gap: Option<String>,
}

/// Build a session roster from a lease registry.
pub fn roster_from_leases(leases: &LeaseRegistry, now: i64) -> Vec<SessionRoster> {
    let mut by_session: BTreeMap<String, Vec<LeaseInfo>> = BTreeMap::new();

    for lease in leases.values() {
        let is_stale = store::is_stale(lease, now);
        let heartbeat_age_secs = (now - lease.heartbeat_at).max(0);
        let info = LeaseInfo {
            key: lease.key.clone(),
            title: lease.title.clone(),
            heartbeat_age_secs,
            is_stale,
        };
        by_session
            .entry(lease.session_id.clone())
            .or_default()
            .push(info);
    }

    by_session
        .into_iter()
        .map(|(session_id, leases)| {
            let live_count = leases.iter().filter(|l| !l.is_stale).count();
            SessionRoster {
                session_id,
                leases,
                live_count,
            }
        })
        .collect()
}

/// Parse backlog JSON (from `backlog list --json`).
/// Expected format: array of items with fields like {key, title, status, priority}.
pub fn parse_backlog(json: &str) -> BacklogSummary {
    #[derive(Deserialize)]
    struct BacklogItem {
        #[serde(default)]
        status: Option<String>,
        #[serde(default)]
        priority: Option<String>,
    }

    match serde_json::from_str::<Vec<BacklogItem>>(json) {
        Ok(items) => {
            let mut summary = BacklogSummary::default();
            let mut pending_by_priority: BTreeMap<String, usize> = BTreeMap::new();

            for item in items {
                match item.status.as_deref() {
                    Some("done") => summary.done += 1,
                    Some("deferred") => summary.deferred += 1,
                    _ => {
                        summary.pending += 1;
                        if let Some(pri) = item.priority {
                            *pending_by_priority.entry(pri).or_insert(0) += 1;
                        }
                    }
                }
            }

            summary.pending_by_priority = pending_by_priority;
            summary
        }
        Err(_) => BacklogSummary::default(),
    }
}

/// Parse PDO hypothesis JSON (from `hypothesis list --json`).
/// Expected format: array of items with a field like {status: "open"|"awaiting-measurement"|"validated"|"rejected"}.
pub fn bucket_hypotheses(json: &str) -> HypoBuckets {
    #[derive(Deserialize)]
    struct HypoItem {
        #[serde(default)]
        status: Option<String>,
    }

    match serde_json::from_str::<Vec<HypoItem>>(json) {
        Ok(items) => {
            let mut buckets = HypoBuckets::default();
            for item in items {
                match item.status.as_deref() {
                    Some("open") => buckets.open += 1,
                    Some("awaiting-measurement") => buckets.awaiting_measurement += 1,
                    Some("validated") => buckets.validated += 1,
                    Some("rejected") => buckets.rejected += 1,
                    _ => {}
                }
            }
            buckets
        }
        Err(_) => HypoBuckets::default(),
    }
}

/// Parse condukt runs TSV (from `condukt state list`).
/// Expected format: tab-separated, one run per line: `run_id<TAB>done/total<TAB>goal`
pub fn parse_condukt_runs(tsv: &str) -> Vec<RunRow> {
    let mut rows = Vec::new();
    for line in tsv.lines() {
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() >= 2 {
            let run_id = parts[0].trim().to_string();
            let progress_parts: Vec<&str> = parts[1].split('/').collect();
            let done = progress_parts
                .first()
                .and_then(|s| s.trim().parse::<usize>().ok())
                .unwrap_or(0);
            let total = progress_parts
                .get(1)
                .and_then(|s| s.trim().parse::<usize>().ok())
                .unwrap_or(0);
            let goal = parts
                .get(2)
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
            rows.push(RunRow {
                run_id,
                done,
                total,
                goal,
            });
        }
    }
    rows
}

/// Shell a command, returning stdout if successful, else None (fail-soft).
fn shell_soft(cmd: &str, args: &[&str]) -> Option<String> {
    use std::process::Command;

    let output = Command::new(cmd).args(args).output().ok()?;
    if output.status.success() {
        String::from_utf8(output.stdout).ok()
    } else {
        None
    }
}

/// Build the full ProgressView (infallible, fail-soft).
pub fn build(cwd: &Path) -> ProgressView {
    let now = store::now();
    let mut view = ProgressView::default();

    // 1. Overwatch ledger: load live leases, reap stale, build session rosters.
    if let Ok(mut leases) = store::load_leases(cwd) {
        store::reap_stale(&mut leases, now);
        view.sessions = roster_from_leases(&leases, now);
    }

    // 2. Backlog: try `backlog list --json`, fall back to plain text parsing.
    if let Some(json_output) = shell_soft("backlog", &["list", "--json"]) {
        view.backlog = Some(parse_backlog(&json_output));
    }

    // 3. PDO hypotheses: try `hypothesis list --json`.
    if let Some(json_output) = shell_soft("hypothesis", &["list", "--json"]) {
        view.hypotheses = Some(bucket_hypotheses(&json_output));
    }

    // 4. Condukt runs: shell `condukt state list` (TAB-separated).
    if let Some(output) = shell_soft("condukt", &["state", "list"]) {
        view.runs = parse_condukt_runs(&output);
    }

    // 5. Compass gap: shell `compass gap` (plain text, optional).
    if let Some(gap) = shell_soft("compass", &["gap"]) {
        let gap_str = gap.trim().to_string();
        if !gap_str.is_empty() {
            view.compass_gap = Some(gap_str);
        }
    }

    view
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Lease;
    use std::collections::BTreeMap;

    #[test]
    fn test_roster_from_leases_empty() {
        let leases = LeaseRegistry::new();
        let roster = roster_from_leases(&leases, 1000);
        assert!(roster.is_empty());
    }

    #[test]
    fn test_roster_from_leases_single_session() {
        let mut leases = LeaseRegistry::new();
        let now = 2000i64;

        leases.insert(
            "task-1".to_string(),
            Lease {
                key: "task-1".to_string(),
                title: "Task 1".to_string(),
                session_id: "session-a".to_string(),
                run_id: "run-1".to_string(),
                claimed_at: 1000,
                heartbeat_at: 1900,
            },
        );

        leases.insert(
            "task-2".to_string(),
            Lease {
                key: "task-2".to_string(),
                title: "Task 2".to_string(),
                session_id: "session-a".to_string(),
                run_id: "run-1".to_string(),
                claimed_at: 1000,
                heartbeat_at: 1850,
            },
        );

        let roster = roster_from_leases(&leases, now);
        assert_eq!(roster.len(), 1);
        assert_eq!(roster[0].session_id, "session-a");
        assert_eq!(roster[0].leases.len(), 2);
        assert_eq!(roster[0].live_count, 2);
    }

    #[test]
    fn test_roster_from_leases_multiple_sessions() {
        let mut leases = LeaseRegistry::new();
        let now = 2000i64;

        for i in 0..2 {
            leases.insert(
                format!("task-a{}", i),
                Lease {
                    key: format!("task-a{}", i),
                    title: format!("Task A{}", i),
                    session_id: "session-a".to_string(),
                    run_id: "run-1".to_string(),
                    claimed_at: 1000,
                    heartbeat_at: 1900,
                },
            );
        }

        leases.insert(
            "task-b1".to_string(),
            Lease {
                key: "task-b1".to_string(),
                title: "Task B1".to_string(),
                session_id: "session-b".to_string(),
                run_id: "run-2".to_string(),
                claimed_at: 1000,
                heartbeat_at: 1850,
            },
        );

        let roster = roster_from_leases(&leases, now);
        assert_eq!(roster.len(), 2);
        assert_eq!(roster[0].live_count, 2); // session-a (sorted first)
        assert_eq!(roster[1].live_count, 1); // session-b
    }

    #[test]
    fn test_roster_marks_stale_leases() {
        let mut leases = LeaseRegistry::new();
        let now = 2000i64;

        leases.insert(
            "fresh".to_string(),
            Lease {
                key: "fresh".to_string(),
                title: "Fresh".to_string(),
                session_id: "s1".to_string(),
                run_id: "r1".to_string(),
                claimed_at: 1000,
                heartbeat_at: 1900,
            },
        );

        leases.insert(
            "stale".to_string(),
            Lease {
                key: "stale".to_string(),
                title: "Stale".to_string(),
                session_id: "s1".to_string(),
                run_id: "r1".to_string(),
                claimed_at: 0,
                heartbeat_at: 0,
            },
        );

        let roster = roster_from_leases(&leases, now);
        assert_eq!(roster.len(), 1);
        assert_eq!(roster[0].leases.len(), 2);

        // Find fresh and stale in the roster
        let fresh_info = roster[0].leases.iter().find(|l| l.key == "fresh").unwrap();
        let stale_info = roster[0].leases.iter().find(|l| l.key == "stale").unwrap();

        assert!(!fresh_info.is_stale);
        assert!(stale_info.is_stale);
        assert_eq!(roster[0].live_count, 1); // Only fresh is live
    }

    #[test]
    fn test_parse_backlog_empty() {
        let json = "[]";
        let summary = parse_backlog(json);
        assert_eq!(summary.pending, 0);
        assert_eq!(summary.done, 0);
        assert_eq!(summary.deferred, 0);
    }

    #[test]
    fn test_parse_backlog_mixed_statuses() {
        let json = r#"[
            {"status": "done", "priority": "P0"},
            {"status": "done", "priority": "P1"},
            {"status": "pending", "priority": "P0"},
            {"status": "pending", "priority": "P0"},
            {"status": "pending", "priority": "P1"},
            {"status": "deferred", "priority": "P2"}
        ]"#;

        let summary = parse_backlog(json);
        assert_eq!(summary.done, 2);
        assert_eq!(summary.pending, 3);
        assert_eq!(summary.deferred, 1);
        assert_eq!(summary.pending_by_priority.get("P0"), Some(&2));
        assert_eq!(summary.pending_by_priority.get("P1"), Some(&1));
    }

    #[test]
    fn test_parse_backlog_no_priority() {
        let json = r#"[
            {"status": "pending"},
            {"status": "done"},
            {"status": "deferred"}
        ]"#;

        let summary = parse_backlog(json);
        assert_eq!(summary.pending, 1);
        assert_eq!(summary.done, 1);
        assert_eq!(summary.deferred, 1);
        assert!(summary.pending_by_priority.is_empty());
    }

    #[test]
    fn test_parse_backlog_invalid_json() {
        let json = "not valid json";
        let summary = parse_backlog(json);
        assert_eq!(summary.pending, 0);
        assert_eq!(summary.done, 0);
        assert_eq!(summary.deferred, 0);
    }

    #[test]
    fn test_bucket_hypotheses_empty() {
        let json = "[]";
        let buckets = bucket_hypotheses(json);
        assert_eq!(buckets.open, 0);
        assert_eq!(buckets.awaiting_measurement, 0);
        assert_eq!(buckets.validated, 0);
        assert_eq!(buckets.rejected, 0);
    }

    #[test]
    fn test_bucket_hypotheses_all_statuses() {
        let json = r#"[
            {"status": "open"},
            {"status": "open"},
            {"status": "awaiting-measurement"},
            {"status": "validated"},
            {"status": "validated"},
            {"status": "validated"},
            {"status": "rejected"}
        ]"#;

        let buckets = bucket_hypotheses(json);
        assert_eq!(buckets.open, 2);
        assert_eq!(buckets.awaiting_measurement, 1);
        assert_eq!(buckets.validated, 3);
        assert_eq!(buckets.rejected, 1);
    }

    #[test]
    fn test_bucket_hypotheses_invalid_json() {
        let json = "not json";
        let buckets = bucket_hypotheses(json);
        assert_eq!(buckets.open, 0);
        assert_eq!(buckets.awaiting_measurement, 0);
        assert_eq!(buckets.validated, 0);
        assert_eq!(buckets.rejected, 0);
    }

    #[test]
    fn test_parse_condukt_runs_empty() {
        let tsv = "";
        let runs = parse_condukt_runs(tsv);
        assert!(runs.is_empty());
    }

    #[test]
    fn test_parse_condukt_runs_single() {
        let tsv = "run-001\t5/10\tBuild feature X";
        let runs = parse_condukt_runs(tsv);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].run_id, "run-001");
        assert_eq!(runs[0].done, 5);
        assert_eq!(runs[0].total, 10);
        assert_eq!(runs[0].goal, Some("Build feature X".to_string()));
    }

    #[test]
    fn test_parse_condukt_runs_multiple() {
        let tsv = "run-001\t5/10\tPhase 1\nrun-002\t2/5\t\nrun-003\t0/3\tInitial";
        let runs = parse_condukt_runs(tsv);
        assert_eq!(runs.len(), 3);

        assert_eq!(runs[0].run_id, "run-001");
        assert_eq!(runs[0].done, 5);
        assert_eq!(runs[0].total, 10);
        assert_eq!(runs[0].goal, Some("Phase 1".to_string()));

        assert_eq!(runs[1].run_id, "run-002");
        assert_eq!(runs[1].done, 2);
        assert_eq!(runs[1].total, 5);
        assert_eq!(runs[1].goal, None); // Empty goal

        assert_eq!(runs[2].run_id, "run-003");
        assert_eq!(runs[2].done, 0);
        assert_eq!(runs[2].total, 3);
        assert_eq!(runs[2].goal, Some("Initial".to_string()));
    }

    #[test]
    fn test_parse_condukt_runs_no_goal() {
        let tsv = "run-001\t3/7";
        let runs = parse_condukt_runs(tsv);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].run_id, "run-001");
        assert_eq!(runs[0].done, 3);
        assert_eq!(runs[0].total, 7);
        assert_eq!(runs[0].goal, None);
    }

    #[test]
    fn test_parse_condukt_runs_malformed() {
        // Line with only run_id (missing progress) should be skipped
        let tsv = "run-001\nrun-002\t5/10\tGoal";
        let runs = parse_condukt_runs(tsv);
        assert_eq!(runs.len(), 1); // Only run-002 is valid
        assert_eq!(runs[0].run_id, "run-002");
    }

    #[test]
    fn test_progress_view_default_serializes() {
        let view = ProgressView::default();
        let json = serde_json::to_string(&view).unwrap();
        // Empty sessions and runs should be skipped due to skip_serializing_if
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        // Should be an empty object or only have null fields
        assert!(parsed.is_object());
    }

    #[test]
    fn test_progress_view_with_sessions() {
        let roster = SessionRoster {
            session_id: "s1".to_string(),
            leases: vec![LeaseInfo {
                key: "k1".to_string(),
                title: "t1".to_string(),
                heartbeat_age_secs: 100,
                is_stale: false,
            }],
            live_count: 1,
        };

        let view = ProgressView {
            sessions: vec![roster],
            ..Default::default()
        };

        let json = serde_json::to_string(&view).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(parsed["sessions"].is_array());
        assert_eq!(parsed["sessions"][0]["session_id"], "s1");
    }

    #[test]
    fn test_backlog_summary_serializes() {
        let mut summary = BacklogSummary {
            pending: 5,
            done: 10,
            deferred: 2,
            pending_by_priority: BTreeMap::new(),
        };
        summary.pending_by_priority.insert("P0".to_string(), 3);
        summary.pending_by_priority.insert("P1".to_string(), 2);

        let json = serde_json::to_string(&summary).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["pending"], 5);
        assert_eq!(parsed["done"], 10);
        assert_eq!(parsed["deferred"], 2);
        assert_eq!(parsed["pending_by_priority"]["P0"], 3);
    }

    #[test]
    fn test_hypo_buckets_serializes() {
        let buckets = HypoBuckets {
            open: 2,
            awaiting_measurement: 1,
            validated: 5,
            rejected: 1,
        };

        let json = serde_json::to_string(&buckets).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["open"], 2);
        assert_eq!(parsed["awaiting_measurement"], 1);
        assert_eq!(parsed["validated"], 5);
        assert_eq!(parsed["rejected"], 1);
    }

    #[test]
    fn test_run_row_serializes_with_goal() {
        let run = RunRow {
            run_id: "run-001".to_string(),
            done: 5,
            total: 10,
            goal: Some("Phase 1".to_string()),
        };

        let json = serde_json::to_string(&run).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["run_id"], "run-001");
        assert_eq!(parsed["done"], 5);
        assert_eq!(parsed["total"], 10);
        assert_eq!(parsed["goal"], "Phase 1");
    }

    #[test]
    fn test_run_row_serializes_without_goal() {
        let run = RunRow {
            run_id: "run-002".to_string(),
            done: 2,
            total: 5,
            goal: None,
        };

        let json = serde_json::to_string(&run).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["run_id"], "run-002");
        assert_eq!(parsed["done"], 2);
        assert_eq!(parsed["total"], 5);
        assert!(
            !parsed.get("goal").map(|v| !v.is_null()).unwrap_or(false) || parsed["goal"].is_null()
        );
    }

    #[test]
    fn test_parse_backlog_default_status() {
        // Items without a status field should be treated as pending
        let json = r#"[
            {"priority": "P0"},
            {"status": "done", "priority": "P1"}
        ]"#;

        let summary = parse_backlog(json);
        assert_eq!(summary.pending, 1);
        assert_eq!(summary.done, 1);
    }
}
