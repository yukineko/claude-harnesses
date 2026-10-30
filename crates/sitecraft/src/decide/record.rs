//! The decision record `<root>/sitecraft/decisions.toml`: its schema, load /
//! save, the next unanswered question, and the completeness check.
//!
//! Schema (TOML):
//!
//! ```toml
//! [areas.auth]
//! relevance = "relevant"          # or "irrelevant"; missing = undecided
//! [areas.auth.answers]
//! needs-auth = "yes"
//!
//! [[questions]]                    # domain questions added by `decide add-question`
//! id = "vrm-other-formats"
//! text = "How should formats other than VRM be handled?"
//! choices = ["reject", "convert", "accept-as-is"]   # empty = free text
//! answer = "reject"                # missing = unanswered
//!
//! [features.login]                 # `decide classify`
//! kind = "dedicated-harness"       # or "simple"
//! harness = "authkit"              # plugin name looked up in marketplace.json
//! ```

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{Determination, Verdict};

/// Directory (relative to the project root) holding the record.
pub const DIR: &str = "sitecraft";
/// Record file name inside [`DIR`].
pub const FILE_NAME: &str = "decisions.toml";
/// Area id under which domain questions (added by [`add_question`]) are
/// addressed by `decide next` / `decide record`. Not a catalog area.
pub const DOMAIN_AREA: &str = "domain";

/// Whether an area matters for the site.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Relevance {
    /// The area's questions must be answered.
    Relevant,
    /// Recorded as out of scope; settles the area without further questions.
    Irrelevant,
}

/// Decisions for one catalog area.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AreaDecision {
    /// `None` = not yet judged (undecided; counted as pending by [`check`]).
    #[serde(default)]
    pub relevance: Option<Relevance>,
    /// Answers keyed by question id.
    #[serde(default)]
    pub answers: BTreeMap<String, String>,
}

/// A domain-specific question raised from the purpose (`decide add-question`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DomainQuestion {
    /// Unique id among domain questions.
    pub id: String,
    /// Question text.
    pub text: String,
    /// Allowed answers; empty = free text.
    #[serde(default)]
    pub choices: Vec<String>,
    /// `None` = unanswered (blocks [`check`]).
    #[serde(default)]
    pub answer: Option<String>,
}

/// How a feature is to be built.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FeatureKind {
    /// Built directly in the site.
    Simple,
    /// Needs its own harness plugin in the harness repo.
    DedicatedHarness,
}

/// Classification of one feature.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FeatureClass {
    /// Simple or dedicated harness.
    pub kind: FeatureKind,
    /// Plugin name of the dedicated harness (defaults to the feature name).
    #[serde(default)]
    pub harness: Option<String>,
}

/// The whole record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Decisions {
    /// Per catalog area, keyed by area id.
    #[serde(default)]
    pub areas: BTreeMap<String, AreaDecision>,
    /// Domain questions.
    #[serde(default)]
    pub questions: Vec<DomainQuestion>,
    /// Feature classifications, keyed by feature name.
    #[serde(default)]
    pub features: BTreeMap<String, FeatureClass>,
}

impl Decisions {
    /// A record with nothing decided (what `decide record` starts from when the
    /// file is absent). Every catalog area is pending in it.
    #[must_use]
    pub fn empty() -> Self {
        Decisions {
            areas: BTreeMap::new(),
            questions: Vec::new(),
            features: BTreeMap::new(),
        }
    }
}

/// The next question to ask, as printed by `decide next` (JSON).
///
/// `{"status":"ask","area":…,"question_id":…,"text":…,"choices":[…]}` or
/// `{"status":"done"}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "kebab-case")]
pub enum Next {
    /// An unanswered question. `area` is a catalog area id or [`DOMAIN_AREA`];
    /// `question_id` may be [`super::catalog::RELEVANCE_QUESTION`].
    Ask {
        /// Area id.
        area: String,
        /// Question id.
        question_id: String,
        /// Question text.
        text: String,
        /// Allowed answers (empty = free text).
        choices: Vec<String>,
    },
    /// Every area judged, every active question and domain question answered.
    Done,
}

/// `<root>/sitecraft/decisions.toml`.
#[must_use]
pub fn path_for(root: &Path) -> PathBuf {
    root.join(DIR).join(FILE_NAME)
}

/// Load the record.
///
/// Contract: file absent (`NotFound`) → `Known(None)`; read error, non-UTF-8,
/// TOML error, unknown key, unknown area id, or an answer not among its
/// question's choices → `Undetermined`; else `Known(Some(_))`.
pub fn load(root: &Path) -> Determination<Option<Decisions>> {
    Determination::undetermined(format!(
        "sitecraft: loading {} is not implemented yet",
        path_for(root).display()
    ))
}

/// Write the record to `<root>/sitecraft/decisions.toml` (creating the
/// directory; temp file then rename). `Err` on any IO/encode failure.
pub fn save(root: &Path, decisions: &Decisions) -> Result<(), String> {
    let _ = decisions;
    Err(format!(
        "sitecraft: saving {} is not implemented yet",
        path_for(root).display()
    ))
}

/// The next unanswered question, in catalog order then domain questions.
///
/// Contract: an area with no relevance → `Ask` its
/// [`super::catalog::RELEVANCE_QUESTION`]; a relevant area → its first active
/// unanswered question; irrelevant areas are skipped as settled; then the
/// first unanswered domain question; nothing left → `Done`. Catalog
/// unavailable or record inconsistent (unknown area) → `Undetermined`.
pub fn next(decisions: &Decisions) -> Determination<Next> {
    let _ = decisions;
    Determination::undetermined("sitecraft: `decide next` is not implemented yet")
}

/// Record `answer` for `question_id` in `area` (in memory; call [`save`]).
///
/// Contract: `question_id == RELEVANCE_QUESTION` sets the area's relevance
/// (`relevant`/`irrelevant`, anything else is `Err`); `area == DOMAIN_AREA`
/// answers a domain question; unknown area, unknown question, or an answer
/// not among a non-empty `choices` → `Err` (record unchanged).
pub fn record_answer(
    decisions: &mut Decisions,
    area: &str,
    question_id: &str,
    answer: &str,
) -> Result<(), String> {
    let _ = (decisions, answer);
    Err(format!(
        "sitecraft: recording {area}/{question_id} is not implemented yet"
    ))
}

/// Add an (unanswered) domain question. Duplicate `id` or empty `id`/`text`
/// → `Err` (record unchanged).
pub fn add_question(
    decisions: &mut Decisions,
    id: &str,
    text: &str,
    choices: Vec<String>,
) -> Result<(), String> {
    let _ = (decisions, text, choices);
    Err(format!(
        "sitecraft: adding question `{id}` is not implemented yet"
    ))
}

/// Record the classification of `feature`. Empty `feature` → `Err`.
pub fn classify(
    decisions: &mut Decisions,
    feature: &str,
    kind: FeatureKind,
    harness: Option<String>,
) -> Result<(), String> {
    let _ = (decisions, kind, harness);
    Err(format!(
        "sitecraft: classifying `{feature}` is not implemented yet"
    ))
}

/// Are all required decisions present?
///
/// Contract: `Clean` iff every catalog area has a relevance, every relevant
/// area's active questions are answered, and every domain question is
/// answered; otherwise `Violation` naming the pending area / question ids;
/// catalog unavailable → `Undetermined`.
pub fn check(decisions: &Decisions) -> Verdict {
    let _ = decisions;
    Verdict::undetermined("sitecraft: `decide check` is not implemented yet")
}

/// [`load`] then [`check`] for `root`: absent record → `Undetermined`
/// (nothing decided is not "all decided"); load `Undetermined` forwarded.
pub fn check_root(root: &Path) -> Verdict {
    Verdict::undetermined(format!(
        "sitecraft: `decide check` for {} is not implemented yet",
        path_for(root).display()
    ))
}
