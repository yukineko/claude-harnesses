//! The closed consistency rule table of `sitecraft decide check` (rulings D7,
//! C1, U4).
//!
//! [`RULES`] holds exactly six rows — W1 (warning) and V1 to V5 (violations).
//! Adding or removing a row requires revising
//! `docs/specs/sitecraft-capabilities-rulings.md` (D7) first.
//!
//! Only catalog answers are looked at; domain questions are never evaluated
//! here (C1: their consistency is the SKILL's Phase 4 re-read with the user,
//! and this module does not pretend to judge it).
//!
//! Evaluation (D7): a condition is a conjunction of clauses, each clause a
//! disjunction of terms. A term only sees *recorded* values: a term that refers
//! to an answer that is not recorded (its area is irrelevant or undecided, the
//! follow-up is inactive, or the question is unanswered) is false, and a
//! relevance term on an undecided area is false. A multi answer satisfies an
//! answer term when any of its elements is in the term's set.

use super::record::{AnswerValue, Decisions, Relevance};

/// What a fired rule means for the exit code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// Printed as `warning: <id>: <text>` on stderr; never changes the exit
    /// code (U4).
    Warning,
    /// Reported with the pending items; makes `decide check` exit 1.
    Violation,
}

/// One term of a rule condition.
#[derive(Debug, Clone, Copy)]
pub enum Term {
    /// The area's recorded relevance equals the given one.
    Relevance(&'static str, Relevance),
    /// The recorded answer of `area`/`question` (any element, for a multi
    /// answer) is one of the listed values.
    AnswerIn(&'static str, &'static str, &'static [&'static str]),
}

/// One row of the table.
#[derive(Debug, Clone, Copy)]
pub struct Rule {
    /// Stable id (`W1`, `V1` … `V5`).
    pub id: &'static str,
    /// Warning or violation.
    pub severity: Severity,
    /// Display text (the gist from D7).
    pub text: &'static str,
    /// Conjunction of clauses; each clause is a disjunction of terms.
    pub when: &'static [&'static [Term]],
}

const AUTH_OFF: &[Term] = &[
    Term::Relevance("auth", Relevance::Irrelevant),
    Term::AnswerIn("auth", "needs-login", &["no"]),
];

/// The closed table (rulings D7). Exactly these six rows.
pub const RULES: [Rule; 6] = [
    Rule {
        id: "W1",
        severity: Severity::Warning,
        text: "公開範囲が未定のままログインを作ろうとしている（auth が relevant なのに exposure が irrelevant）",
        when: &[
            &[Term::Relevance("auth", Relevance::Relevant)],
            &[Term::Relevance("exposure", Relevance::Irrelevant)],
        ],
    },
    Rule {
        id: "V1",
        severity: Severity::Violation,
        text: "静的ホスティングではサーバー側処理を実行できない",
        when: &[
            &[Term::AnswerIn("exposure", "hosting", &["static"])],
            &[Term::AnswerIn(
                "processing",
                "execution",
                &["in-request", "background"],
            )],
        ],
    },
    Rule {
        id: "V2",
        severity: Severity::Violation,
        text: "静的ホスティングには書き込めるローカルディスクが無い",
        when: &[
            &[Term::AnswerIn("exposure", "hosting", &["static"])],
            &[Term::AnswerIn("storage", "file-storage", &["local-disk"])],
        ],
    },
    Rule {
        id: "V3",
        severity: Severity::Violation,
        text: "利用者を区別するのにログインが無い",
        when: &[
            &[Term::AnswerIn(
                "users-roles",
                "user-model",
                &["same-rights", "multi-role"],
            )],
            AUTH_OFF,
        ],
    },
    Rule {
        id: "V4",
        severity: Severity::Violation,
        text: "管理画面があるのにログインが無い",
        when: &[&[Term::AnswerIn("admin", "admin-ui", &["yes"])], AUTH_OFF],
    },
    Rule {
        id: "V5",
        severity: Severity::Violation,
        text: "失敗を知らせるのに通知が無い",
        when: &[
            &[Term::AnswerIn("workflow", "on-failure", &["notify", "both"])],
            &[Term::Relevance("notification", Relevance::Irrelevant)],
        ],
    },
];

/// A fired rule with the recorded (area, question, answer) triples of the
/// terms that made it fire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fired {
    /// Rule id.
    pub id: &'static str,
    /// Warning or violation.
    pub severity: Severity,
    /// Display text.
    pub text: &'static str,
    /// `(area, question, answer)` of every true term, in table order
    /// (`question` is `relevance` for a relevance term).
    pub involved: Vec<(String, String, String)>,
}

impl Fired {
    /// `<id>: <text> (<area>/<question> = <answer>, …)` — one line, no `; `
    /// (the check output splits findings on `; `).
    #[must_use]
    pub fn line(&self) -> String {
        let terms: Vec<String> = self
            .involved
            .iter()
            .map(|(a, q, v)| format!("{a}/{q} = {v}"))
            .collect();
        format!("{}: {} ({})", self.id, self.text, terms.join(", "))
    }
}

fn relevance_word(r: Relevance) -> &'static str {
    match r {
        Relevance::Relevant => super::catalog::RELEVANT,
        Relevance::Irrelevant => super::catalog::IRRELEVANT,
    }
}

/// The recorded value a term refers to, if the term is true.
fn eval_term(d: &Decisions, t: &Term) -> Option<(String, String, String)> {
    match *t {
        Term::Relevance(area, want) => {
            let got = d.areas.get(area).and_then(|a| a.relevance)?;
            (got == want).then(|| {
                (
                    area.to_string(),
                    super::catalog::RELEVANCE_QUESTION.to_string(),
                    relevance_word(got).to_string(),
                )
            })
        }
        Term::AnswerIn(area, question, set) => {
            let ad = d.areas.get(area)?;
            // Answers only count for a relevant area (load rejects others,
            // but the term must not depend on that).
            if ad.relevance != Some(Relevance::Relevant) {
                return None;
            }
            let v = ad.answers.get(question)?;
            let hit = match v {
                AnswerValue::One(s) => set.contains(&s.as_str()),
                AnswerValue::Many(xs) => xs.iter().any(|x| set.contains(&x.as_str())),
            };
            hit.then(|| (area.to_string(), question.to_string(), v.display()))
        }
    }
}

/// Every rule of the closed table [`RULES`] that fires for `d`, in table
/// order. This is what `decide check` and the Stop gate use; it is
/// [`evaluate_with`]`(&RULES, d)`.
#[must_use]
pub fn evaluate(d: &Decisions) -> Vec<Fired> {
    evaluate_with(&RULES, d)
}

/// Every rule of `table` that fires for `d`, in table order. Production code
/// only passes [`RULES`]; the parameter exists so the evaluation semantics
/// can be tested with synthetic rows (e.g. a term over a multi answer).
///
/// Contract: a rule fires iff every clause has at least one true term; a term
/// on an unrecorded value (area undecided / irrelevant, follow-up inactive,
/// question unanswered) is false; an answer term over a multi (array) answer
/// is true iff any element is in the term's set; domain questions are never
/// read.
#[must_use]
pub fn evaluate_with(table: &[Rule], d: &Decisions) -> Vec<Fired> {
    let mut out = Vec::new();
    'rules: for r in table {
        let mut involved = Vec::new();
        for clause in r.when {
            let hits: Vec<_> = clause.iter().filter_map(|t| eval_term(d, t)).collect();
            if hits.is_empty() {
                continue 'rules;
            }
            involved.extend(hits);
        }
        out.push(Fired {
            id: r.id,
            severity: r.severity,
            text: r.text,
            involved,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_has_exactly_the_six_rows_of_d7() {
        let ids: Vec<&str> = RULES.iter().map(|r| r.id).collect();
        assert_eq!(ids, ["W1", "V1", "V2", "V3", "V4", "V5"]);
        assert_eq!(RULES[0].severity, Severity::Warning);
        assert!(RULES[1..].iter().all(|r| r.severity == Severity::Violation));
    }

    #[test]
    fn empty_record_fires_nothing() {
        assert!(evaluate(&Decisions::empty()).is_empty());
    }
}
