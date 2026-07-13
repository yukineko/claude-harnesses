/// Status rendering and output formatting.
use crate::aggregate::{self, BacklogSummary, HypoBuckets, RunRow, SessionRoster};
use anyhow::Result;
use std::fmt::Write as _;

/// Render the full human-readable progress report, or JSON if `json` is true.
///
/// Uses the short-lived status cache (`aggregate::build_cached`): this is the
/// command wired to BOTH the SessionStart and Stop hooks (see
/// `hooks/hooks.json`), so a fresh render's cache hit collapses the second
/// hook invocation's ~5 subprocess spawns when the two fire close together.
pub fn status(json: bool) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let view = aggregate::build_cached(&cwd);

    if json {
        println!("{}", serde_json::to_string_pretty(&view)?);
        return Ok(());
    }

    let mut out = String::new();

    out.push_str("== Sessions ==\n");
    out.push_str(&format_roster(&view.sessions));
    out.push('\n');

    out.push_str("== PDO hypotheses ==\n");
    out.push_str(&format_hypotheses(view.hypotheses.as_ref()));
    out.push('\n');

    out.push_str("== Backlog ==\n");
    out.push_str(&format_backlog(view.backlog.as_ref()));
    out.push('\n');

    out.push_str("== Condukt runs ==\n");
    out.push_str(&format_runs(&view.runs));
    out.push('\n');

    out.push_str("== Compass gap ==\n");
    out.push_str(&format_compass_gap(view.compass_gap.as_deref()));

    print!("{out}");
    Ok(())
}

/// Render only the per-session roster (section 1 of `status`), or JSON if `json` is true.
pub fn sessions(json: bool) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let view = aggregate::build(&cwd);

    if json {
        println!("{}", serde_json::to_string_pretty(&view.sessions)?);
        return Ok(());
    }

    print!("{}", format_roster(&view.sessions));
    Ok(())
}

/// Format the per-session lease roster as a readable table.
fn format_roster(sessions: &[SessionRoster]) -> String {
    if sessions.is_empty() {
        return "(none)\n".to_string();
    }

    let mut out = String::new();
    for roster in sessions {
        let _ = writeln!(
            out,
            "session {}  ({} live)",
            roster.session_id, roster.live_count
        );
        if roster.leases.is_empty() {
            out.push_str("  (no leases)\n");
            continue;
        }
        for lease in &roster.leases {
            let marker = if lease.is_stale { "stale" } else { "live" };
            let _ = writeln!(
                out,
                "  [{marker}] {}  \"{}\"  heartbeat {}s ago",
                lease.key, lease.title, lease.heartbeat_age_secs
            );
        }
    }
    out
}

/// Format the PDO hypothesis bucket counts, emphasizing awaiting-measurement.
fn format_hypotheses(hypotheses: Option<&HypoBuckets>) -> String {
    match hypotheses {
        None => "(none)\n".to_string(),
        Some(buckets) => {
            let mut out = String::new();
            let _ = writeln!(out, "  open:                 {}", buckets.open);
            let _ = writeln!(
                out,
                "  awaiting-measurement: {}  <-- build\u{2260}validate signal",
                buckets.awaiting_measurement
            );
            let _ = writeln!(out, "  validated:            {}", buckets.validated);
            let _ = writeln!(out, "  rejected:             {}", buckets.rejected);
            out
        }
    }
}

/// Format the backlog summary (pending/done/deferred + priority breakdown).
fn format_backlog(backlog: Option<&BacklogSummary>) -> String {
    match backlog {
        None => "(none)\n".to_string(),
        Some(summary) => {
            let mut out = String::new();
            let _ = writeln!(out, "  pending:  {}", summary.pending);
            let _ = writeln!(out, "  done:     {}", summary.done);
            let _ = writeln!(out, "  deferred: {}", summary.deferred);
            if summary.pending_by_priority.is_empty() {
                out.push_str("  by priority: (none)\n");
            } else {
                out.push_str("  by priority:\n");
                for (priority, count) in &summary.pending_by_priority {
                    let _ = writeln!(out, "    {priority}: {count}");
                }
            }
            out
        }
    }
}

/// Format open condukt runs as `run_id  done/total  goal`.
fn format_runs(runs: &[RunRow]) -> String {
    if runs.is_empty() {
        return "(none)\n".to_string();
    }

    let mut out = String::new();
    for run in runs {
        let goal = run.goal.as_deref().unwrap_or("");
        let _ = writeln!(
            out,
            "  {}  {}/{}  {}",
            run.run_id, run.done, run.total, goal
        );
    }
    out
}

/// Format the compass gap string, if present.
fn format_compass_gap(gap: Option<&str>) -> String {
    match gap {
        None => "(none)\n".to_string(),
        Some(gap) => format!("  {gap}\n"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aggregate::LeaseInfo;
    use std::collections::BTreeMap;

    #[test]
    fn test_format_roster_empty() {
        assert_eq!(format_roster(&[]), "(none)\n");
    }

    #[test]
    fn test_format_roster_no_leases() {
        let sessions = vec![SessionRoster {
            session_id: "s1".to_string(),
            leases: vec![],
            live_count: 0,
        }];
        let out = format_roster(&sessions);
        assert!(out.contains("session s1"));
        assert!(out.contains("(no leases)"));
    }

    #[test]
    fn test_format_roster_with_leases() {
        let sessions = vec![SessionRoster {
            session_id: "s1".to_string(),
            leases: vec![
                LeaseInfo {
                    key: "task-1".to_string(),
                    title: "Do thing".to_string(),
                    heartbeat_age_secs: 5,
                    is_stale: false,
                },
                LeaseInfo {
                    key: "task-2".to_string(),
                    title: "Old thing".to_string(),
                    heartbeat_age_secs: 900,
                    is_stale: true,
                },
            ],
            live_count: 1,
        }];
        let out = format_roster(&sessions);
        assert!(out.contains("[live] task-1"));
        assert!(out.contains("[stale] task-2"));
    }

    #[test]
    fn test_format_hypotheses_none() {
        assert_eq!(format_hypotheses(None), "(none)\n");
    }

    #[test]
    fn test_format_hypotheses_some() {
        let buckets = HypoBuckets {
            open: 2,
            awaiting_measurement: 1,
            validated: 5,
            rejected: 0,
        };
        let out = format_hypotheses(Some(&buckets));
        assert!(out.contains("open:"));
        assert!(out.contains("awaiting-measurement"));
        assert!(out.contains('1'));
    }

    #[test]
    fn test_format_backlog_none() {
        assert_eq!(format_backlog(None), "(none)\n");
    }

    #[test]
    fn test_format_backlog_some() {
        let mut pending_by_priority = BTreeMap::new();
        pending_by_priority.insert("P0".to_string(), 3usize);
        let summary = BacklogSummary {
            pending: 5,
            done: 10,
            deferred: 1,
            pending_by_priority,
        };
        let out = format_backlog(Some(&summary));
        assert!(out.contains("pending:  5"));
        assert!(out.contains("P0: 3"));
    }

    #[test]
    fn test_format_backlog_no_priority_breakdown() {
        let summary = BacklogSummary {
            pending: 1,
            done: 0,
            deferred: 0,
            pending_by_priority: BTreeMap::new(),
        };
        let out = format_backlog(Some(&summary));
        assert!(out.contains("by priority: (none)"));
    }

    #[test]
    fn test_format_runs_empty() {
        assert_eq!(format_runs(&[]), "(none)\n");
    }

    #[test]
    fn test_format_runs_with_goal() {
        let runs = vec![RunRow {
            run_id: "run-1".to_string(),
            done: 3,
            total: 5,
            goal: Some("ship it".to_string()),
        }];
        let out = format_runs(&runs);
        assert!(out.contains("run-1  3/5  ship it"));
    }

    #[test]
    fn test_format_compass_gap_none() {
        assert_eq!(format_compass_gap(None), "(none)\n");
    }

    #[test]
    fn test_format_compass_gap_some() {
        let out = format_compass_gap(Some("need more coverage"));
        assert!(out.contains("need more coverage"));
    }
}
