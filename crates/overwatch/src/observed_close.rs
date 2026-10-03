//! Observation-based closure of review findings (backlog 89544915).
//!
//! A finding produced by a deterministic detector (condukt's gate-exec
//! escalation, specguard's structural detection, …) is closed by RE-OBSERVING
//! the condition that produced it — never by a commit message naming its id
//! (ruling R2: that is self-report) and never because a probe went quiet.
//!
//! This module is the shared plumbing for those closers; each producer owns its
//! own judge (it alone knows how to re-observe its condition):
//!
//! * [`open_findings`] — the findings still open on the queue (no disposition),
//!   read tri-state: an unreadable store is `Undetermined`, never "no findings".
//! * [`close_observed`] — runs the producer's judge over each open finding and
//!   appends a `resolved` disposition ONLY for a `Known(Resolved)` observation.
//!   `StillPresent` stays open silently; `Undetermined` stays open and is
//!   reported, with its reason.
//! * [`ReconcileReport`] — the CLI contract: `{"resolved":[..],
//!   "undetermined":[{"finding_id","why"}]}`, a `NOT closed <id>: <why>` stderr
//!   line per undetermined finding, exit 0 / 3.
//!
//! `resolved` is not a human verdict (ruling R3); see
//! [`crate::disposition::DispositionVerdict::Resolved`].
use crate::disposition::{Disposition, DispositionVerdict};
use crate::review_finding::ReviewFinding;
use crate::store::{self, DispositionAppend, ReviewFindingScan};
use harness_core::verdict::Determination;
use std::collections::BTreeSet;
use std::path::Path;

/// What re-observing a finding's condition showed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Observation {
    /// The condition no longer holds. `evidence` records what was observed;
    /// it is persisted on the disposition row.
    Resolved { evidence: String },
    /// The condition still holds: the finding stays open (this is a
    /// determined answer, not an undetermined one).
    StillPresent,
}

/// One finding that could not be judged — it stays OPEN.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotClosed {
    pub finding_id: String,
    pub why: String,
}

/// The result of one reconcile pass.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReconcileReport {
    /// Findings this pass closed (a `resolved` row was WRITTEN by this pass).
    pub resolved: Vec<String>,
    /// Findings that could not be judged or could not be closed; they stay
    /// open. Never empty-by-error: a failure to read the store itself lands
    /// in `store_undetermined` instead of producing an empty, clean report.
    pub undetermined: Vec<NotClosed>,
    /// The open-findings store itself could not be read: NOTHING was judged.
    pub store_undetermined: Option<String>,
}

impl ReconcileReport {
    /// A report for a pass that could not even list the open findings.
    pub fn store_undetermined(why: impl Into<String>) -> Self {
        Self {
            store_undetermined: Some(why.into()),
            ..Self::default()
        }
    }

    /// Whether anything stayed open for lack of a determination.
    pub fn has_undetermined(&self) -> bool {
        !self.undetermined.is_empty() || self.store_undetermined.is_some()
    }

    /// Process exit code: 0 when everything was determined, else 3.
    pub fn exit_code(&self) -> i32 {
        if self.has_undetermined() {
            3
        } else {
            0
        }
    }

    /// The `--json` stdout shape (both keys always present).
    pub fn to_json(&self) -> serde_json::Value {
        let mut v = serde_json::json!({
            "resolved": self.resolved,
            "undetermined": self
                .undetermined
                .iter()
                .map(|u| serde_json::json!({ "finding_id": u.finding_id, "why": u.why }))
                .collect::<Vec<_>>(),
        });
        if let Some(why) = &self.store_undetermined {
            v["store_undetermined"] = serde_json::Value::String(why.clone());
        }
        v
    }

    /// One `NOT closed` line per undetermined finding (and one for an
    /// unreadable store), prefixed with `tool`.
    pub fn not_closed_lines(&self, tool: &str) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(why) = &self.store_undetermined {
            out.push(format!(
                "{tool}: NOT closed: the review-findings store could not be read, so no \
                 finding was judged ({why})"
            ));
        }
        for u in &self.undetermined {
            out.push(format!("{tool}: NOT closed {}: {}", u.finding_id, u.why));
        }
        out
    }

    /// Print the report (JSON or prose on stdout, `NOT closed` lines on
    /// stderr) and return the exit code.
    pub fn emit(&self, tool: &str, json: bool) -> i32 {
        for line in self.not_closed_lines(tool) {
            eprintln!("{line}");
        }
        if json {
            println!("{}", self.to_json());
        } else {
            println!(
                "{tool}: {} finding(s) resolved by observation, {} NOT closed (undetermined)",
                self.resolved.len(),
                self.undetermined.len() + usize::from(self.store_undetermined.is_some())
            );
            for id in &self.resolved {
                println!("  resolved {id}");
            }
        }
        self.exit_code()
    }
}

/// The findings still OPEN on the review queue (no disposition for their exact
/// `finding_id`) whose id satisfies `select`, de-duplicated by id, in first-seen
/// order. Reads the hot store (the same stream `review-queue` lists).
///
/// Tri-state: a store or disposition ledger that could not be read in full is
/// `Undetermined` — never an empty list, which a closer would read as "nothing
/// to do". An ABSENT findings store is a genuine `Known(vec![])`.
pub fn open_findings(
    cwd: &Path,
    select: impl Fn(&str) -> bool,
) -> Determination<Vec<ReviewFinding>> {
    let findings = match store::scan_review_findings(cwd) {
        ReviewFindingScan::Absent => return Determination::known(Vec::new()),
        ReviewFindingScan::Findings(f) => f,
        ReviewFindingScan::Undetermined(why) => {
            return Determination::undetermined(format!("review_findings.jsonl: {why}"))
        }
    };
    let dispositions = match store::scan_dispositions(cwd) {
        Ok(Determination::Known(d)) => d,
        Ok(Determination::Undetermined(why)) => return Determination::Undetermined(why),
        Err(e) => {
            return Determination::undetermined(format!(
                "cannot resolve the disposition ledger path: {e}"
            ))
        }
    };
    let closed: BTreeSet<&str> = dispositions.iter().map(|d| d.finding_id.as_str()).collect();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut out = Vec::new();
    for f in findings {
        if !select(&f.finding_id) || closed.contains(f.finding_id.as_str()) {
            continue;
        }
        if seen.insert(f.finding_id.clone()) {
            out.push(f);
        }
    }
    Determination::known(out)
}

/// Judge each of `open` with `judge` and append a `resolved` disposition for
/// every `Known(Resolved)` observation (first writer wins — a finding someone
/// else dispositioned in the meantime is NOT reported as resolved by this
/// pass). A failed or refused write leaves the finding open and reports it as
/// undetermined: a closure that was not persisted is not a closure.
pub fn close_observed(
    cwd: &Path,
    open: Vec<ReviewFinding>,
    mut judge: impl FnMut(&ReviewFinding) -> Determination<Observation>,
    reviewer: &str,
    observed_source: &str,
    now: i64,
) -> ReconcileReport {
    let mut report = ReconcileReport::default();
    for f in open {
        let evidence = match judge(&f) {
            Determination::Known(Observation::StillPresent) => continue,
            Determination::Known(Observation::Resolved { evidence }) => evidence,
            Determination::Undetermined(why) => {
                report.undetermined.push(NotClosed {
                    finding_id: f.finding_id,
                    why: why.as_str().to_string(),
                });
                continue;
            }
        };
        let d = Disposition::new(
            f.finding_id.clone(),
            DispositionVerdict::Resolved,
            reviewer.to_string(),
            now,
        )
        .with_evidence(Some(evidence), Some(observed_source.to_string()));
        match store::append_disposition_detailed(cwd, &d) {
            Ok(DispositionAppend::Written) => report.resolved.push(f.finding_id),
            // Closed already by an earlier writer: not ours to report.
            Ok(DispositionAppend::AlreadyDispositioned) => {}
            Ok(DispositionAppend::NotPersisted(outcome)) => report.undetermined.push(NotClosed {
                finding_id: f.finding_id,
                why: format!(
                    "observed resolved, but the disposition was NOT persisted ({outcome:?})"
                ),
            }),
            Err(e) => report.undetermined.push(NotClosed {
                finding_id: f.finding_id,
                why: format!("observed resolved, but the disposition write failed: {e}"),
            }),
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_code_is_three_when_anything_is_undetermined() {
        let mut r = ReconcileReport::default();
        assert_eq!(r.exit_code(), 0);
        r.undetermined.push(NotClosed {
            finding_id: "x".into(),
            why: "y".into(),
        });
        assert_eq!(r.exit_code(), 3);
        assert_eq!(ReconcileReport::store_undetermined("z").exit_code(), 3);
    }

    #[test]
    fn json_always_has_both_keys() {
        let v = ReconcileReport::default().to_json();
        assert_eq!(v["resolved"], serde_json::json!([]));
        assert_eq!(v["undetermined"], serde_json::json!([]));
    }
}
