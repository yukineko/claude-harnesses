//! The capability-area catalog: the areas a site may need decisions on, each
//! with representative questions and answer-conditional follow-ups.
//!
//! Selecting which areas matter for the user's purpose is the skill's (LLM's)
//! job; this module only holds the fixed catalog and the deterministic
//! question-activation rule. Area ids (fixed by the decide tests):
//! `auth`, `users-roles`, `admin`, `storage`, `data-analytics`,
//! `web-analytics`, `processing`, `workflow`, `notification`, `ai`,
//! `formats`, `exposure`.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::Determination;

/// Pseudo question id every area has: whether the area is relevant to the
/// purpose. Answers are [`RELEVANT`] / [`IRRELEVANT`]. It is asked (by
/// `decide next`) before any other question of the area; an `irrelevant`
/// answer settles the area with no further questions.
pub const RELEVANCE_QUESTION: &str = "relevance";
/// Answer to [`RELEVANCE_QUESTION`]: the area matters for this site.
pub const RELEVANT: &str = "relevant";
/// Answer to [`RELEVANCE_QUESTION`]: the area is out of scope (recorded, not skipped).
pub const IRRELEVANT: &str = "irrelevant";

/// Answers recorded for one area, keyed by question id.
pub type Answers = BTreeMap<String, String>;

/// One catalog area.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Area {
    /// Stable id (e.g. `auth`).
    pub id: String,
    /// Human-readable title.
    pub title: String,
    /// All questions of the area, representative and conditional.
    pub questions: Vec<Question>,
}

/// One question in an area.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Question {
    /// Id, unique within its area.
    pub id: String,
    /// The question text shown to the user.
    pub text: String,
    /// Allowed answers. Empty means free text.
    pub choices: Vec<String>,
    /// `None` for a representative question (always asked once the area is
    /// relevant); `Some` for a follow-up asked only when the condition holds.
    pub when: Option<Condition>,
}

/// Activation condition of a follow-up question.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Condition {
    /// The question (same area) whose answer gates this one.
    pub question_id: String,
    /// The follow-up is active iff that question's recorded answer is one of these.
    pub answer_in: Vec<String>,
}

/// The full catalog, in asking order.
///
/// Contract: `Known` with exactly the 12 areas listed in the module docs,
/// each with at least one representative question (`when == None`). Never
/// `Known(vec![])`.
pub fn areas() -> Determination<Vec<Area>> {
    Determination::undetermined("sitecraft: the capability-area catalog is not implemented yet")
}

/// The questions of `area_id` that are active given `answers` (the area's
/// recorded answers), in asking order: every representative question, plus
/// each follow-up whose [`Condition`] holds. Does not include
/// [`RELEVANCE_QUESTION`].
///
/// Contract: unknown `area_id` → `Undetermined`; otherwise `Known` (never
/// empty: every area has a representative question).
pub fn active_questions(area_id: &str, answers: &Answers) -> Determination<Vec<Question>> {
    let _ = answers;
    Determination::undetermined(format!(
        "sitecraft: question activation for area `{area_id}` is not implemented yet"
    ))
}
