//! Read gauge's session records for recent session info.
//!
//! Reads gauge's canonical [`SessionRecord`] (shared via harness_core) instead
//! of a local mirror struct, and prefers budgetguard's authoritative per-session
//! cost from the shared [`Ledger`] over recomputing it with a divergent override
//! set — so the number shown here matches what the gate actually counted.

use serde::Serialize;

use harness_core::ledger::Ledger;
use harness_core::verdict::Determination;
use harness_core::{pricing, session};

#[derive(Debug, Serialize, Clone)]
pub struct SessionSummary {
    pub session_id: String,
    pub project: String,
    pub turns: u64,
    pub total_tokens: u64,
    pub last_ts: Option<String>,
    pub cost_usd: f64,
}

/// The `n` most recent sessions gauge recorded.
///
/// Three-valued because the display layer must be able to say **unknown**:
/// `Known(vec![])` is "gauge recorded no sessions" (the honest empty state this
/// panel already renders as "No session records found — gauge not installed?"),
/// while `Undetermined` is "gauge's store could not be read". Collapsing the
/// latter into an empty `Vec` — which this function used to do — made an
/// unreadable store render as a confident "no sessions", and this panel's
/// numbers are read by a human as spend.
///
/// This function holds no allow/block verdict, but it is not exempt from
/// [`Determination`] on that ground: the exemption in `CLAUDE.md` is for output
/// no consumer can read as "fine", and its two consumers both render silence as
/// an affirmative statement about spend —
/// `crates/harness-status/src/main.rs:132` (`sessions` subcommand: an empty
/// list prints nothing at all) and `crates/harness-status/src/main.rs:330`
/// (default report → `crates/harness-status/src/display.rs:64`, which prints
/// "No session records found"). Both are updated to print `unknown` instead.
pub fn recent(n: usize) -> Determination<Vec<SessionSummary>> {
    let mut records = match session::load_all(&session::default_state_dir()) {
        Determination::Known(records) => records,
        Determination::Undetermined(why) => return Determination::Undetermined(why),
    };
    if records.is_empty() {
        return Determination::Known(vec![]);
    }

    // Sort newest first.
    records.sort_by(|a, b| b.last_ts.cmp(&a.last_ts));
    records.truncate(n);

    // budgetguard's ledger holds the authoritative per-session USD; reuse it so
    // this view can't diverge from the gate. Fall back to a default-rate recompute
    // only when a session isn't in the ledger (gauge ran but budgetguard didn't).
    let ledger = Ledger::load(&harness_core::ledger::default_state_dir());

    let summaries = records
        .into_iter()
        .map(|r| {
            let total_tokens = r.total_tokens();
            let cost_usd = ledger
                .session_cost(&r.session_id)
                .unwrap_or_else(|| pricing::session_cost(r.models.iter(), &[]));
            SessionSummary {
                session_id: r.session_id,
                project: r.project,
                turns: r.turns,
                total_tokens,
                last_ts: r.last_ts,
                cost_usd,
            }
        })
        .collect();
    Determination::Known(summaries)
}
