//! The verdict side of Ask learning: read the learned table and answer an
//! `Ask` with `Allow` / `Deny` (spec `docs/specs/blastguard-ask-learning.md`,
//! user ruling 2026-11-28).
//!
//! # What this may and may not do
//!
//! It is handed an `Ask` (the detector's own, before any hardening) and may
//! turn it into `Allow` or `Deny`. A `Deny` is never handed to it and so can
//! never be overridden: [`apply`] returns every non-`Ask` decision unchanged.
//! A learned Allow additionally requires, enforced HERE and not only in the
//! learner:
//!
//! * the call is not system harm;
//! * the shape's target class is neither `System` nor `ProtectedConfig`, and
//!   no command word is unresolved ([`Shape::allow_permitted`]);
//! * the entry records zero declines.
//!
//! # Fail-closed
//!
//! A table that exists but cannot be read, parsed or is of another version is
//! `TableRead::Undetermined`: the `Ask` stands (never Allow) and its reason
//! gains a note that the learned table could not be read. An absent table is
//! simply "nothing learned yet".

use crate::model::Decision;
use harness_core::asklearn::{self, LearnedDecision, Shape, TableRead};

/// What the table did to an `Ask`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Applied {
    /// Not an `Ask`, or nothing learned: the decision is unchanged.
    Unchanged(Decision),
    /// Learned Allow; the string is the reason to record.
    Allow(String),
    /// Learned Deny.
    Deny(String),
}

fn allow_reason(shape: &str, approvals: u64, sessions: u64, declines: u64) -> String {
    format!(
        "learned: approved {approvals} times across {sessions} sessions, {declines} declines \
         — shape {shape}"
    )
}

/// Apply the table to `decision` for `shape`. `system_harm` is the detector's
/// system-harm flag for the call.
pub fn apply(decision: Decision, shape: &Shape, system_harm: bool, table: &TableRead) -> Applied {
    let Decision::Ask(reason) = decision else {
        return Applied::Unchanged(decision);
    };
    let t = match table {
        TableRead::Absent => return Applied::Unchanged(Decision::Ask(reason)),
        TableRead::Undetermined(why) => {
            return Applied::Unchanged(Decision::Ask(format!(
                "{reason}\n\n(blastguard: the learned table could not be read — {why} — so no \
                 learned decision was applied and this stays a question.)"
            )))
        }
        TableRead::Known(t) => t,
    };
    match asklearn::lookup(t, shape) {
        Some(LearnedDecision::Allow {
            shape: key,
            approvals,
            sessions,
            declines,
        }) if declines == 0 && !system_harm && shape.allow_permitted() => {
            Applied::Allow(allow_reason(&key, approvals, sessions, declines))
        }
        Some(LearnedDecision::Deny {
            shape: key,
            approvals,
            sessions,
            declines,
        }) => Applied::Deny(format!(
            "blastguard: learned deny — a human declined this shape {declines} time(s) (approved \
             {approvals} times across {sessions} sessions) — shape {key}. The original question \
             follows.\n\n{reason}"
        )),
        // No entry, or an Allow entry the guards refuse: the Ask stands.
        _ => Applied::Unchanged(Decision::Ask(reason)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness_core::asklearn::{Entry, LearnedTable, TargetClass, Verdict};

    fn shape(class: TargetClass, verb: &str) -> Shape {
        Shape {
            rule_id: "r".into(),
            target_class: class,
            verbs: vec![verb.into()],
        }
    }

    fn table(s: &Shape, decision: Verdict, declines: u64) -> TableRead {
        TableRead::Known(LearnedTable {
            version: 1,
            entries: vec![Entry {
                shape: s.key(),
                decision,
                approvals: 5,
                declines,
                sessions: 3,
                samples: vec![],
            }],
        })
    }

    #[test]
    fn deny_is_never_changed() {
        let s = shape(TargetClass::Home, "rm");
        let t = table(&s, Verdict::Allow, 0);
        let d = Decision::deny("no");
        assert_eq!(apply(d.clone(), &s, false, &t), Applied::Unchanged(d));
    }

    #[test]
    fn allow_entry_is_refused_for_guarded_shapes() {
        for class in [TargetClass::System, TargetClass::ProtectedConfig] {
            let s = shape(class, "rm");
            let t = table(&s, Verdict::Allow, 0);
            assert!(matches!(
                apply(Decision::ask("q"), &s, false, &t),
                Applied::Unchanged(Decision::Ask(_))
            ));
        }
        let s = shape(TargetClass::Home, "rm");
        // An allow entry that records a decline is inconsistent: not honoured.
        assert!(matches!(
            apply(Decision::ask("q"), &s, false, &table(&s, Verdict::Allow, 1)),
            Applied::Unchanged(Decision::Ask(_))
        ));
        // System harm is refused even with a clean entry.
        assert!(matches!(
            apply(Decision::ask("q"), &s, true, &table(&s, Verdict::Allow, 0)),
            Applied::Unchanged(Decision::Ask(_))
        ));
        assert!(matches!(
            apply(Decision::ask("q"), &s, false, &table(&s, Verdict::Allow, 0)),
            Applied::Allow(_)
        ));
    }
}
