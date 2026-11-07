//! Run-policy decision stats (moved verbatim out of `state.rs`).

use super::RunPolicyLogRecord;
use serde::Serialize;

/// Aggregate counts of RUN-POLICY decisions by verdict category. Used by
/// `condukt run-policy stats`.
#[derive(Debug, Clone, Serialize, PartialEq, Default)]
pub struct RunPolicyStats {
    pub verify_only: usize,
    pub escalate_docker: usize,
    pub escalate_ship: usize,
    pub ask_human: usize,
}

/// Pure function: classify each record's `verdict` string into the four known
/// categories and count them. Unknown/garbage verdict values are ignored
/// (never panics, never bumps an "other" bucket that doesn't exist).
pub fn aggregate_run_policy_stats(records: &[RunPolicyLogRecord]) -> RunPolicyStats {
    let mut stats = RunPolicyStats::default();
    for r in records {
        match r.verdict.as_str() {
            "verify_only" => stats.verify_only += 1,
            "escalate_docker" => stats.escalate_docker += 1,
            "escalate_ship" => stats.escalate_ship += 1,
            "ask_human" => stats.ask_human += 1,
            _ => {}
        }
    }
    stats
}
