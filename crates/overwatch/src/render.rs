/// Status rendering and output formatting.
///
/// # `(none)` must mean "checked, found nothing"
///
/// Every section of this report renders an empty collection as `(none)`. That is
/// only honest while emptiness is an observation. When a source could not be
/// read at all, `(none)` states a fact nobody established — and it states the
/// *reassuring* one, so nothing downstream ever questions it. The sections
/// therefore consult [`aggregate::ProgressView::undetermined`] first and print
/// `(unknown: …)` instead; the reason is carried into the line so a reader can
/// act on it without re-running anything.
///
/// This is the same correction the statusline took in `3b1eb24` (CLAUDE.md §1):
/// a blank render was being read as "plenty of headroom".
use crate::aggregate::{
    self, BacklogSummary, HypoBuckets, ProgressView, RunRow, SessionRoster, SOURCE_BACKLOG,
    SOURCE_COMPASS_GAP, SOURCE_HYPOTHESES, SOURCE_RUNS, SOURCE_SESSIONS,
};
use anyhow::Result;
use std::fmt::Write as _;

/// Render the placeholder for a section: `(unknown: …)` when that source could
/// not be observed, otherwise the caller's own "genuinely empty" text.
fn placeholder(view: &ProgressView, source: &str, empty_text: &str) -> String {
    match view.undetermined_reason(source) {
        Some(reason) => format!("(unknown: {reason})\n"),
        None => empty_text.to_string(),
    }
}

/// Emit the loud stderr summary when any source was undetermined.
///
/// stdout carries the report a human skims; this goes to stderr so an
/// undetermined source is also visible to anything capturing hook diagnostics,
/// and so it cannot be mistaken for part of the report body.
fn warn_undetermined(view: &ProgressView) {
    if !view.has_undetermined() {
        return;
    }
    eprintln!(
        "overwatch status: WARNING — {} status source(s) could NOT be observed. \
         The sections below marked `(unknown)` are NOT reports of zero.",
        view.undetermined.len()
    );
    for u in &view.undetermined {
        eprintln!("  [{}] {}", u.source, u.reason);
    }
}

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
    out.push_str(&format_roster(&view, &view.sessions));
    out.push('\n');

    out.push_str("== PDO hypotheses ==\n");
    out.push_str(&format_hypotheses(&view, view.hypotheses.as_ref()));
    out.push('\n');

    out.push_str("== Backlog ==\n");
    out.push_str(&format_backlog(&view, view.backlog.as_ref()));
    out.push('\n');

    out.push_str("== Condukt runs ==\n");
    out.push_str(&format_runs(&view, &view.runs));
    out.push('\n');

    out.push_str("== Compass gap ==\n");
    out.push_str(&format_compass_gap(&view, view.compass_gap.as_deref()));

    print!("{out}");
    warn_undetermined(&view);
    Ok(())
}

/// Render only the per-session roster (section 1 of `status`), or JSON if `json` is true.
pub fn sessions(json: bool) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let view = aggregate::build(&cwd);

    if json {
        // The roster alone cannot express "unreadable ledger", so an
        // undetermined roster is surfaced on stderr rather than being implied by
        // an empty array. Callers scripting against this should prefer
        // `status --json`, whose `undetermined` key is machine-readable.
        println!("{}", serde_json::to_string_pretty(&view.sessions)?);
        warn_undetermined(&view);
        return Ok(());
    }

    print!("{}", format_roster(&view, &view.sessions));
    warn_undetermined(&view);
    Ok(())
}

/// Format the per-session lease roster as a readable table.
fn format_roster(view: &ProgressView, sessions: &[SessionRoster]) -> String {
    if sessions.is_empty() {
        return placeholder(view, SOURCE_SESSIONS, "(none)\n");
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
fn format_hypotheses(view: &ProgressView, hypotheses: Option<&HypoBuckets>) -> String {
    match hypotheses {
        None => placeholder(view, SOURCE_HYPOTHESES, "(none)\n"),
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
fn format_backlog(view: &ProgressView, backlog: Option<&BacklogSummary>) -> String {
    match backlog {
        None => placeholder(view, SOURCE_BACKLOG, "(none)\n"),
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
fn format_runs(view: &ProgressView, runs: &[RunRow]) -> String {
    if runs.is_empty() {
        return placeholder(view, SOURCE_RUNS, "(none)\n");
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
fn format_compass_gap(view: &ProgressView, gap: Option<&str>) -> String {
    match gap {
        None => placeholder(view, SOURCE_COMPASS_GAP, "(none)\n"),
        Some(gap) => format!("  {gap}\n"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aggregate::LeaseInfo;
    use std::collections::BTreeMap;

    /// A view in which every source WAS observed and all of them were empty.
    fn observed_empty() -> ProgressView {
        ProgressView::default()
    }

    /// A view in which `source` could not be observed.
    fn unobserved(source: &str) -> ProgressView {
        let mut v = ProgressView::default();
        v.mark_undetermined(source, "boom");
        v
    }

    #[test]
    fn test_format_roster_empty() {
        assert_eq!(format_roster(&observed_empty(), &[]), "(none)\n");
    }

    // ── Regression: `(none)` must not be printed for an unobserved source ────
    //
    // Before this, `aggregate::build` bound its lease load with `if let Ok(..)`,
    // so a corrupt `leases.json` produced the same empty roster as an absent
    // one, and this renderer printed byte-identical `(none)` for both. Measured
    // end-to-end against the real binary: a truncated `leases.json` holding one
    // LIVE lease from another session rendered exactly the same five `(none)`
    // sections, exit 0, as a store that had never been written.
    //
    // `(none)` in the Sessions section is the claim "no other session is live" —
    // the fact CLAUDE.md §8 tells every session NOT to assume, and the liveness
    // input condukt's main-tree guard reads before permitting a commit in main's
    // shared working tree (`condukt/src/maintree.rs`, which documents this very
    // flattening as "a real residual hole, not a safe degradation").

    #[test]
    fn undetermined_sessions_render_unknown_not_none() {
        let out = format_roster(&unobserved(SOURCE_SESSIONS), &[]);
        assert!(
            out.starts_with("(unknown:"),
            "an unreadable lease ledger must not render as an observation, got {out:?}"
        );
        assert_ne!(out, "(none)\n");
    }

    #[test]
    fn undetermined_render_carries_the_reason() {
        let mut v = ProgressView::default();
        v.mark_undetermined(SOURCE_SESSIONS, "leases.json present but corrupt");
        assert!(format_roster(&v, &[]).contains("leases.json present but corrupt"));
    }

    #[test]
    fn undetermined_backlog_renders_unknown_not_none() {
        let out = format_backlog(&unobserved(SOURCE_BACKLOG), None);
        assert!(out.starts_with("(unknown:"), "got {out:?}");
    }

    #[test]
    fn undetermined_hypotheses_render_unknown_not_none() {
        let out = format_hypotheses(&unobserved(SOURCE_HYPOTHESES), None);
        assert!(out.starts_with("(unknown:"), "got {out:?}");
    }

    #[test]
    fn undetermined_runs_render_unknown_not_none() {
        let out = format_runs(&unobserved(SOURCE_RUNS), &[]);
        assert!(out.starts_with("(unknown:"), "got {out:?}");
    }

    #[test]
    fn undetermined_compass_gap_renders_unknown_not_none() {
        let out = format_compass_gap(&unobserved(SOURCE_COMPASS_GAP), None);
        assert!(out.starts_with("(unknown:"), "got {out:?}");
    }

    /// An undetermined mark on one source must not leak into the others: the
    /// point of the fix is a per-source distinction, so a blanket `(unknown)`
    /// everywhere would be a different (over-)report, not a correct one.
    #[test]
    fn undetermined_is_scoped_to_its_own_source() {
        let v = unobserved(SOURCE_SESSIONS);
        assert_eq!(format_backlog(&v, None), "(none)\n");
        assert_eq!(format_runs(&v, &[]), "(none)\n");
        assert_eq!(format_compass_gap(&v, None), "(none)\n");
    }

    #[test]
    fn test_format_roster_no_leases() {
        let sessions = vec![SessionRoster {
            session_id: "s1".to_string(),
            leases: vec![],
            live_count: 0,
        }];
        let out = format_roster(&observed_empty(), &sessions);
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
        let out = format_roster(&observed_empty(), &sessions);
        assert!(out.contains("[live] task-1"));
        assert!(out.contains("[stale] task-2"));
    }

    #[test]
    fn test_format_hypotheses_none() {
        assert_eq!(format_hypotheses(&observed_empty(), None), "(none)\n");
    }

    #[test]
    fn test_format_hypotheses_some() {
        let buckets = HypoBuckets {
            open: 2,
            awaiting_measurement: 1,
            validated: 5,
            rejected: 0,
        };
        let out = format_hypotheses(&observed_empty(), Some(&buckets));
        assert!(out.contains("open:"));
        assert!(out.contains("awaiting-measurement"));
        assert!(out.contains('1'));
    }

    #[test]
    fn test_format_backlog_none() {
        assert_eq!(format_backlog(&observed_empty(), None), "(none)\n");
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
        let out = format_backlog(&observed_empty(), Some(&summary));
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
        let out = format_backlog(&observed_empty(), Some(&summary));
        assert!(out.contains("by priority: (none)"));
    }

    #[test]
    fn test_format_runs_empty() {
        assert_eq!(format_runs(&observed_empty(), &[]), "(none)\n");
    }

    #[test]
    fn test_format_runs_with_goal() {
        let runs = vec![RunRow {
            run_id: "run-1".to_string(),
            done: 3,
            total: 5,
            goal: Some("ship it".to_string()),
        }];
        let out = format_runs(&observed_empty(), &runs);
        assert!(out.contains("run-1  3/5  ship it"));
    }

    #[test]
    fn test_format_compass_gap_none() {
        assert_eq!(format_compass_gap(&observed_empty(), None), "(none)\n");
    }

    #[test]
    fn test_format_compass_gap_some() {
        let out = format_compass_gap(&observed_empty(), Some("need more coverage"));
        assert!(out.contains("need more coverage"));
    }
}
