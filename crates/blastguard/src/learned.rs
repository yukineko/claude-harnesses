//! The verdict side of Ask learning: read the learned table and the human rule
//! files and answer an `Ask` with `Allow` / `Deny` (spec
//! `docs/specs/blastguard-ask-learning.md`, user ruling 2026-11-28).
//!
//! # What this may and may not do
//!
//! It is handed an `Ask` (the detector's own, before any hardening) and may
//! turn it into `Allow` or `Deny`. A `Deny` is never handed to it and so can
//! never be overridden: [`apply`] returns every non-`Ask` decision unchanged.
//!
//! The JUDGMENT is not here: this module only converts the learned table to a
//! [`RuleSet`], builds the [`Facts`] and calls the single entry
//! [`askrules::judge`]. Every Allow guard (never-allowed classes, `?`,
//! system harm, the positive allow-list, deny-beats-allow) lives in
//! `askrules` and holds for learned and human rules alike.
//!
//! # Fail-closed
//!
//! A table that exists but cannot be read, parsed or is of another version is
//! `TableRead::Undetermined`: it contributes no rule (never an Allow), and an
//! `Ask` that stays an `Ask` gains a note that the learned table could not be
//! read. An absent table is simply "nothing learned yet".

use crate::askrules::{self, Facts, Judgement, RuleSet};
use crate::model::Decision;
use harness_core::asklearn::{Shape, TableRead};

/// What the rules did to an `Ask`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Applied {
    /// Not an `Ask`, or nothing applies: the decision is unchanged.
    Unchanged(Decision),
    /// Allow; the string is the reason to record.
    Allow(String),
    /// Deny.
    Deny(String),
}

/// Apply the learned table to `decision` for `shape`, with no human rules and
/// no command text. `system_harm` is the detector's system-harm flag.
pub fn apply(decision: Decision, shape: &Shape, system_harm: bool, table: &TableRead) -> Applied {
    apply_with(decision, shape, system_harm, None, table, &[])
}

/// Apply the human rule sets `human` and the learned `table` to `decision`.
/// `command` is the raw command text `command_regex` conditions search (the
/// Bash command; `None` where the call has none, which counts as matched).
pub fn apply_with(
    decision: Decision,
    shape: &Shape,
    system_harm: bool,
    command: Option<&str>,
    table: &TableRead,
    human: &[&RuleSet],
) -> Applied {
    let Decision::Ask(reason) = decision else {
        return Applied::Unchanged(decision);
    };
    let (learned, note) = match table {
        TableRead::Absent => (RuleSet::from_learned(&empty_table()), String::new()),
        TableRead::Undetermined(why) => (
            RuleSet::from_learned(&empty_table()),
            format!(
                "\n\n(blastguard: the learned table could not be read — {why} — so no learned \
                 decision was applied and this stays a question.)"
            ),
        ),
        TableRead::Known(t) => (RuleSet::from_learned(t), String::new()),
    };
    let mut facts = Facts::from_shape(shape, system_harm);
    facts = match command {
        Some(c) => facts.with_command(c),
        // No readable text: a shell call is then never Allow-eligible.
        None => facts.without_command_text(),
    };
    let mut sets: Vec<&RuleSet> = human.to_vec();
    sets.push(&learned);
    match askrules::judge(&facts, &sets) {
        Judgement::Allow(r) => Applied::Allow(r),
        Judgement::Deny(r) => Applied::Deny(format!("{r}\n\n{reason}")),
        Judgement::NoMatch => Applied::Unchanged(Decision::Ask(format!("{reason}{note}"))),
    }
}

fn empty_table() -> harness_core::asklearn::LearnedTable {
    harness_core::asklearn::LearnedTable::empty()
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
        for class in [
            TargetClass::System,
            TargetClass::ProtectedConfig,
            TargetClass::MainTree,
            TargetClass::Home,
            TargetClass::Undetermined,
        ] {
            let s = shape(class, "rm");
            let t = table(&s, Verdict::Allow, 0);
            assert!(matches!(
                apply(Decision::ask("q"), &s, false, &t),
                Applied::Unchanged(Decision::Ask(_))
            ));
        }
        let s = shape(TargetClass::Worktree, "rm");
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
        // Changed with the structural gate: the clean-entry control now
        // carries the command text (one simple static command), because a
        // shell call with no readable text is never Allowed.
        let t = table(&s, Verdict::Allow, 0);
        assert!(matches!(
            apply_with(Decision::ask("q"), &s, false, Some("rm build"), &t, &[]),
            Applied::Allow(_)
        ));
        assert!(matches!(
            apply(Decision::ask("q"), &s, false, &t),
            Applied::Unchanged(Decision::Ask(_))
        ));
    }

    #[test]
    fn an_unreadable_table_adds_a_note_and_never_allows() {
        let s = shape(TargetClass::Worktree, "rm");
        let t = TableRead::Undetermined("boom".into());
        let got = apply(Decision::ask("q"), &s, false, &t);
        assert!(
            matches!(&got, Applied::Unchanged(Decision::Ask(r)) if r.contains("boom")),
            "{got:?}"
        );
    }
}
