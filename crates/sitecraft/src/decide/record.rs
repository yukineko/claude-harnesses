//! The decision record `<root>/sitecraft/decisions.toml`: its schema, load /
//! save, the next unanswered question, the change history, and the
//! completeness check.
//!
//! Schema (TOML):
//!
//! ```toml
//! [purpose]                        # `decide purpose --text` (rulings D1)
//! text = "A VRM viewer for my avatars."
//!
//! [areas.auth]
//! relevance = "relevant"          # or "irrelevant"; missing = undecided
//! [areas.auth.answers]
//! needs-login = "yes"
//! [areas.auth.notes]               # `decide record --note` (D8)
//! needs-login = "only staff log in"
//!
//! [areas.notification.answers]
//! channel = ["web-push", "email"]  # multi question: always an array (D8)
//!
//! [[questions]]                    # domain questions added by `decide add-question`
//! id = "vrm-other-formats"
//! text = "How should formats other than VRM be handled?"
//! choices = ["reject", "convert", "accept-as-is"]   # empty = free text
//! multi = true                     # optional; answer is then an array
//! answer = ["reject"]              # missing = unanswered
//! note = "..."                     # optional
//!
//! [features.login]                 # `decide classify`
//! kind = "dedicated-harness"       # or "simple"
//! harness = "authkit"              # plugin name looked up in marketplace.json
//! fit = "yes"                      # `--confirm-fit` (D9), only for an existing plugin
//!
//! [capabilities.blender-headless]  # `decide capability` (D2; schema in `super::capability`)
//! relevance = "relevant"
//! how = "..."
//! done_when = "..."
//! [capabilities.blender-headless.verify]
//! kind = "builtin:blender-headless"
//! script = "scripts/make.py"
//! glb = "out/model.glb"
//! render = "out/render.png"
//!
//! [[history]]                      # appended when a recorded value changes (D8)
//! area = "auth"                    # "capability" for a capability (question = its name)
//! question = "needs-login"         # "<q>#note" for a note, "relevance", "text" for purpose
//! old = "yes"
//! new = "no"
//! at = "2026-11-03T01:02:03Z"      # UTC RFC 3339
//! pruned = true                    # only for an answer dropped by follow-up pruning
//! ```
//!
//! Reading (D8): a legacy single string under a multi question is read as a
//! one-element array (and written back as an array); an array under a
//! non-multi question makes the record unusable (`Undetermined`, exit 2).

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::capability::{self, Capability};
use super::catalog::{
    self, AnswerChoice, Area, Question, IRRELEVANT, RELEVANCE_QUESTION, RELEVANT,
};
use super::rules::{self, Severity};
use crate::{Determination, Verdict};

/// Directory (relative to the project root) holding the record.
pub const DIR: &str = "sitecraft";
/// Record file name inside [`DIR`].
pub const FILE_NAME: &str = "decisions.toml";
/// Area id under which domain questions (added by [`add_question`]) are
/// addressed by `decide next` / `decide record`. Not a catalog area.
pub const DOMAIN_AREA: &str = "domain";
/// History `area` of a purpose change (rulings D1).
pub const PURPOSE_AREA: &str = "purpose";
/// History `question` of a purpose change (rulings D1).
pub const PURPOSE_QUESTION: &str = "text";
/// Suffix of a history `question` recording a note change (`<q>#note`, D8).
pub const NOTE_SUFFIX: &str = "#note";

/// Whether an area matters for the site.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Relevance {
    /// The area's questions must be answered.
    Relevant,
    /// Recorded as out of scope; settles the area without further questions.
    Irrelevant,
}

impl Relevance {
    fn word(self) -> &'static str {
        match self {
            Relevance::Relevant => RELEVANT,
            Relevance::Irrelevant => IRRELEVANT,
        }
    }
}

/// A recorded answer: one string, or (multi question) an array.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AnswerValue {
    /// The answer to a single-choice / free-text question.
    One(String),
    /// The answer to a multi question (non-empty, distinct elements).
    Many(Vec<String>),
}

impl AnswerValue {
    /// Human-readable form used in history rows and `changed:` lines: the
    /// string itself, or the elements joined with `, `.
    #[must_use]
    pub fn display(&self) -> String {
        match self {
            AnswerValue::One(s) => s.clone(),
            AnswerValue::Many(xs) => xs.join(", "),
        }
    }
}

impl AnswerChoice for AnswerValue {
    fn any_in(&self, set: &[String]) -> bool {
        match self {
            AnswerValue::One(s) => s.any_in(set),
            AnswerValue::Many(xs) => xs.any_in(set),
        }
    }
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
    pub answers: BTreeMap<String, AnswerValue>,
    /// Notes (`--note`) keyed by question id; only for answered questions.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub notes: BTreeMap<String, String>,
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
    /// Multi-select (`add-question --multi`): the answer is an array.
    #[serde(default, skip_serializing_if = "is_false")]
    pub multi: bool,
    /// `None` = unanswered (blocks [`check`]).
    #[serde(default)]
    pub answer: Option<AnswerValue>,
    /// Note (`--note`); only for an answered question.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_false(b: &bool) -> bool {
    !*b
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

/// The user's answer to "does the existing plugin fit the purpose?" (D9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Fit {
    /// Reuse the existing plugin (`reuse = "confirmed"`).
    Yes,
    /// Do not reuse it; build a new plugin under another name (`"rejected"`).
    No,
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
    /// Recorded purpose fit of the existing plugin (`--confirm-fit`); `None`
    /// = not confirmed (never read as permission to reuse).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fit: Option<Fit>,
}

/// `[purpose]` (rulings D1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Purpose {
    /// The purpose summary. Recorded iff non-blank after trimming.
    pub text: String,
}

/// One `[[history]]` row (rulings D8).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryEntry {
    /// Area id, [`DOMAIN_AREA`] or [`PURPOSE_AREA`].
    pub area: String,
    /// Question id, `relevance`, `<q>#note`, or [`PURPOSE_QUESTION`].
    pub question: String,
    /// Previous value.
    pub old: String,
    /// New value (`""` for a pruned answer).
    pub new: String,
    /// UTC RFC 3339 time of the change.
    pub at: String,
    /// The value was dropped by follow-up pruning (or a relevance change),
    /// not set by the user.
    #[serde(default, skip_serializing_if = "is_false")]
    pub pruned: bool,
}

/// The whole record. (`PartialEq` only: `toml::Value` has no `Eq`.)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Decisions {
    /// The purpose (D1); `None` = not recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purpose: Option<Purpose>,
    /// Per catalog area, keyed by area id.
    #[serde(default)]
    pub areas: BTreeMap<String, AreaDecision>,
    /// Domain questions.
    #[serde(default)]
    pub questions: Vec<DomainQuestion>,
    /// Feature classifications, keyed by feature name.
    #[serde(default)]
    pub features: BTreeMap<String, FeatureClass>,
    /// Capabilities beyond the web app (D2), keyed by name, kept raw so a
    /// broken table (exit 2) can be told apart from a missing key (exit 1);
    /// parse with [`super::capability::parse_all`]. [`load`] has already
    /// checked that every table parses.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub capabilities: BTreeMap<String, toml::Value>,
    /// Change history (append-only).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub history: Vec<HistoryEntry>,
}

impl Decisions {
    /// A record with nothing decided (what `decide record` starts from when the
    /// file is absent). Every catalog area is pending in it.
    #[must_use]
    pub fn empty() -> Self {
        Decisions {
            purpose: None,
            areas: BTreeMap::new(),
            questions: Vec::new(),
            features: BTreeMap::new(),
            capabilities: BTreeMap::new(),
            history: Vec::new(),
        }
    }

    /// The parsed capabilities. `Err` only for a record that did not come
    /// from [`load`] (which rejects unparseable tables).
    pub fn parsed_capabilities(&self) -> Result<BTreeMap<String, Capability>, String> {
        capability::parse_all(&self.capabilities)
    }

    /// The recorded purpose text (trimmed), if non-blank.
    #[must_use]
    pub fn purpose_text(&self) -> Option<&str> {
        self.purpose
            .as_ref()
            .map(|p| p.text.trim())
            .filter(|t| !t.is_empty())
    }
}

/// The next question to ask, as printed by `decide next` (JSON).
///
/// `{"status":"ask","area":…,"question_id":…,"text":…,"choices":[…],"multi":bool}`
/// or `{"status":"done"}`.
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
        /// Several `--answer`s may be given.
        multi: bool,
    },
    /// Every area judged, every active question and domain question answered.
    Done,
}

/// `<root>/sitecraft/decisions.toml`.
#[must_use]
pub fn path_for(root: &Path) -> PathBuf {
    root.join(DIR).join(FILE_NAME)
}

/// Is `answer` acceptable for a question with `choices` (empty = free text,
/// which still must be non-blank)?
fn answer_ok(choices: &[String], answer: &str) -> bool {
    if choices.is_empty() {
        !answer.trim().is_empty()
    } else {
        choices.iter().any(|c| c == answer)
    }
}

/// Shape and element check of a stored answer against its question.
fn value_ok(multi: bool, choices: &[String], v: &AnswerValue) -> Result<(), String> {
    match (multi, v) {
        (false, AnswerValue::One(s)) => {
            if answer_ok(choices, s) {
                Ok(())
            } else {
                Err(format!("{s:?} is not one of its choices {choices:?}"))
            }
        }
        (false, AnswerValue::Many(_)) => {
            Err("is an array but the question is not multi-select".to_string())
        }
        (true, AnswerValue::One(_)) => {
            Err("is a single value but the question is multi-select".to_string())
        }
        (true, AnswerValue::Many(xs)) => {
            if xs.is_empty() {
                return Err("is an empty array".to_string());
            }
            let mut seen = BTreeSet::new();
            for x in xs {
                if !answer_ok(choices, x) {
                    return Err(format!(
                        "element {x:?} is empty or not one of its choices {choices:?}"
                    ));
                }
                if !seen.insert(x.as_str()) {
                    return Err(format!("element {x:?} is duplicated"));
                }
            }
            Ok(())
        }
    }
}

/// Turn CLI `--answer` values into a stored answer for a question.
fn value_from_input(
    multi: bool,
    choices: &[String],
    given: &[String],
) -> Result<AnswerValue, String> {
    let v = if multi {
        AnswerValue::Many(given.to_vec())
    } else {
        match given {
            [one] => AnswerValue::One(one.clone()),
            [] => return Err("--answer is required".to_string()),
            _ => {
                return Err(format!(
                "{} --answer values given but the question is not multi-select (give exactly one)",
                given.len()
            ))
            }
        }
    };
    value_ok(multi, choices, &v).map(|()| v)
}

/// Read-time normalisation (D8): a legacy string under a multi question
/// becomes a one-element array. Arrays under non-multi questions are left for
/// [`validate`] to reject.
fn normalize(d: &mut Decisions, catalog: &[Area]) {
    for (area_id, ad) in &mut d.areas {
        let Some(area) = catalog.iter().find(|a| &a.id == area_id) else {
            continue;
        };
        for (qid, v) in &mut ad.answers {
            let multi = area.questions.iter().any(|q| &q.id == qid && q.multi);
            if multi {
                if let AnswerValue::One(s) = v {
                    *v = AnswerValue::Many(vec![s.clone()]);
                }
            }
        }
    }
    for dq in &mut d.questions {
        if dq.multi {
            if let Some(AnswerValue::One(s)) = &dq.answer {
                dq.answer = Some(AnswerValue::Many(vec![s.clone()]));
            }
        }
    }
}

/// Structural consistency of a record against the catalog: every area id is a
/// catalog area, every recorded catalog answer belongs to a question of its
/// area, has the question's shape (array iff multi), only valid elements, and
/// is active (answers exist only in relevant areas, never for inactive
/// follow-ups); notes only for answered questions and non-blank; domain
/// questions have unique non-empty ids/texts and valid answers; features have
/// non-empty names and a fit only on a dedicated harness.
fn validate(d: &Decisions, catalog: &[Area]) -> Result<(), String> {
    for (area_id, ad) in &d.areas {
        let Some(area) = catalog.iter().find(|a| &a.id == area_id) else {
            return Err(format!("`{area_id}` under [areas] is not a catalog area"));
        };
        if !ad.answers.is_empty() && ad.relevance != Some(Relevance::Relevant) {
            return Err(format!(
                "areas.{area_id}.answers is present but the area is not judged relevant (answers only count for a relevant area)"
            ));
        }
        for (qid, ans) in &ad.answers {
            let Some(q) = area.questions.iter().find(|q| &q.id == qid) else {
                return Err(format!(
                    "areas.{area_id}.answers.{qid}: `{qid}` is not a question of area `{area_id}`"
                ));
            };
            if let Err(e) = value_ok(q.multi, &q.choices, ans) {
                return Err(format!("areas.{area_id}.answers.{qid} {e}"));
            }
            if !catalog::is_active(q, &ad.answers) {
                return Err(format!(
                    "areas.{area_id}.answers.{qid} answers a question that is not active for the other recorded answers (stale answer)"
                ));
            }
        }
        for (qid, note) in &ad.notes {
            if !ad.answers.contains_key(qid) {
                return Err(format!(
                    "areas.{area_id}.notes.{qid} is a note for a question without a recorded answer"
                ));
            }
            if note.trim().is_empty() {
                return Err(format!("areas.{area_id}.notes.{qid} is blank"));
            }
        }
    }
    let mut seen = BTreeSet::new();
    for dq in &d.questions {
        if dq.id.trim().is_empty() || dq.text.trim().is_empty() {
            return Err("a [[questions]] entry has an empty id or text".to_string());
        }
        if !seen.insert(dq.id.as_str()) {
            return Err(format!("duplicate [[questions]] id `{}`", dq.id));
        }
        if dq.choices.iter().any(|c| c.trim().is_empty()) {
            return Err(format!("question `{}` has an empty choice", dq.id));
        }
        if let Some(ans) = &dq.answer {
            if let Err(e) = value_ok(dq.multi, &dq.choices, ans) {
                return Err(format!("question `{}`: answer {e}", dq.id));
            }
        }
        match (&dq.note, &dq.answer) {
            (Some(_), None) => {
                return Err(format!("question `{}` has a note but no answer", dq.id))
            }
            (Some(n), Some(_)) if n.trim().is_empty() => {
                return Err(format!("question `{}` has a blank note", dq.id))
            }
            _ => {}
        }
    }
    for (name, f) in &d.features {
        if name.trim().is_empty() {
            return Err("a [features] entry has an empty name".to_string());
        }
        if f.fit.is_some() && f.kind != FeatureKind::DedicatedHarness {
            return Err(format!(
                "features.{name}.fit is recorded but the feature is not a dedicated harness"
            ));
        }
    }
    capability::parse_all(&d.capabilities)?;
    Ok(())
}

/// Normalise then validate `d` against the catalog; any failure is
/// `Undetermined`.
fn validated(mut d: Decisions, label: &str) -> Determination<Decisions> {
    match catalog::areas() {
        Determination::Known(all) => {
            normalize(&mut d, &all);
            match validate(&d, &all) {
                Ok(()) => Determination::known(d),
                Err(e) => Determination::undetermined(format!("{label}: {e}")),
            }
        }
        Determination::Undetermined(why) => Determination::Undetermined(why),
    }
}

/// Load the record.
///
/// Contract: file absent (`NotFound`) → `Known(None)`; read error, non-UTF-8,
/// TOML error, unknown key, unknown area id, an answer not among its
/// question's choices, an array answer to a non-multi question, an answer to
/// an inactive question or a non-relevant area, a note without an answer, a
/// `[capabilities.<name>]` table that does not parse (bad name, unknown key or
/// `verify.kind`, missing / mistyped required `verify` key; see
/// [`super::capability`]), or an entry that exists but cannot be read (e.g. a
/// dangling symlink) →
/// `Undetermined`; else `Known(Some(_))` (legacy strings under multi
/// questions normalised to arrays).
pub fn load(root: &Path) -> Determination<Option<Decisions>> {
    let path = path_for(root);
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        // `NotFound` from `read` also covers a dangling symlink; only "no
        // directory entry at all" is an absent record.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return match std::fs::symlink_metadata(&path) {
                Err(m) if m.kind() == std::io::ErrorKind::NotFound => Determination::known(None),
                Ok(_) => Determination::undetermined(format!(
                    "{} exists but cannot be read (dangling symlink?): {e}",
                    path.display()
                )),
                Err(m) => {
                    Determination::undetermined(format!("cannot inspect {}: {m}", path.display()))
                }
            };
        }
        Err(e) => {
            return Determination::undetermined(format!("cannot read {}: {e}", path.display()))
        }
    };
    let text = match String::from_utf8(bytes) {
        Ok(t) => t,
        Err(e) => {
            return Determination::undetermined(format!(
                "{} is not valid UTF-8: {e}",
                path.display()
            ))
        }
    };
    let d: Decisions = match toml::from_str(&text) {
        Ok(d) => d,
        Err(e) => {
            return Determination::undetermined(format!(
                "{} is not a valid decision record: {e}",
                path.display()
            ))
        }
    };
    validated(d, &path.display().to_string()).map(Some)
}

/// Write the record to `<root>/sitecraft/decisions.toml` (creating the
/// directory; temp file then rename). `Err` on any IO/encode failure.
pub fn save(root: &Path, decisions: &Decisions) -> Result<(), String> {
    let path = path_for(root);
    let dir = root.join(DIR);
    let body = toml::to_string(decisions)
        .map_err(|e| format!("cannot encode the decision record: {e}"))?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let tmp = dir.join(format!(".{FILE_NAME}.{}.tmp", std::process::id()));
    let write = || -> std::io::Result<()> {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(body.as_bytes())?;
        f.sync_all()?;
        std::fs::rename(&tmp, &path)
    };
    match write() {
        Ok(()) => Ok(()),
        Err(e) => {
            // Best-effort cleanup of the temp file; the error reported is the
            // write/rename failure, which already makes this call `Err`.
            let _cleanup = std::fs::remove_file(&tmp);
            Err(format!("cannot write {}: {e}", path.display()))
        }
    }
}

fn relevance_ask(area: &Area) -> Next {
    Next::Ask {
        area: area.id.clone(),
        question_id: RELEVANCE_QUESTION.to_string(),
        text: format!(
            "「{}」はこのサイトに関係しますか？（relevant: 関係する（詳しく決める）/ irrelevant: 関係しない（対象外として記録））",
            area.title
        ),
        choices: vec![RELEVANT.to_string(), IRRELEVANT.to_string()],
        multi: false,
    }
}

fn ask(area: &str, q: &Question) -> Next {
    Next::Ask {
        area: area.to_string(),
        question_id: q.id.clone(),
        text: q.text.clone(),
        choices: q.choices.clone(),
        multi: q.multi,
    }
}

/// Every pending item, in asking order, as `Next::Ask` values. Shared by
/// [`next`] (first item) and [`check`] (all items), so the two can never
/// disagree about what is pending. The purpose is not a pending question
/// (D1: it is recorded in SKILL Phase 1, see [`check_report`]).
fn pending(decisions: &Decisions) -> Determination<Vec<Next>> {
    let all = match catalog::areas() {
        Determination::Known(a) => a,
        Determination::Undetermined(why) => return Determination::Undetermined(why),
    };
    if let Err(e) = validate(decisions, &all) {
        return Determination::undetermined(format!("inconsistent decision record: {e}"));
    }
    let empty = BTreeMap::new();
    let mut out = Vec::new();
    for area in &all {
        let ad = decisions.areas.get(&area.id);
        match ad.and_then(|a| a.relevance) {
            None => out.push(relevance_ask(area)),
            Some(Relevance::Irrelevant) => {}
            Some(Relevance::Relevant) => {
                let answers = ad.map_or(&empty, |a| &a.answers);
                for q in area.questions.iter() {
                    if catalog::is_active(q, answers) && !answers.contains_key(&q.id) {
                        out.push(ask(&area.id, q));
                    }
                }
            }
        }
    }
    for dq in &decisions.questions {
        if dq.answer.is_none() {
            out.push(Next::Ask {
                area: DOMAIN_AREA.to_string(),
                question_id: dq.id.clone(),
                text: dq.text.clone(),
                choices: dq.choices.clone(),
                multi: dq.multi,
            });
        }
    }
    Determination::known(out)
}

/// The next unanswered question, in catalog order then domain questions.
///
/// Contract: an area with no relevance → `Ask` its
/// [`super::catalog::RELEVANCE_QUESTION`]; a relevant area → its first active
/// unanswered question; irrelevant areas are skipped as settled; then the
/// first unanswered domain question; nothing left → `Done`. The purpose is
/// never asked. Catalog unavailable or record inconsistent (unknown area) →
/// `Undetermined`.
pub fn next(decisions: &Decisions) -> Determination<Next> {
    // An empty pending list is an observed "nothing left" (the catalog is
    // non-empty, so every area contributed either a pending item or a
    // recorded decision), hence `Done`.
    pending(decisions).map(|p| match p.into_iter().next() {
        Some(item) => item,
        None => Next::Done,
    })
}

/// Record `answers` (one, or several for a multi question) for `question_id`
/// in `area`, plus an optional note (in memory; call [`save`]).
///
/// Contract: `question_id == RELEVANCE_QUESTION` sets the area's relevance
/// (exactly one `relevant`/`irrelevant`, anything else is `Err`; a note is
/// refused); `area == DOMAIN_AREA` answers a domain question; unknown area,
/// unknown question, an answer not among a non-empty `choices`, several
/// answers to a non-multi question, an empty / duplicate element of a multi
/// answer, or a blank note → `Err` (record unchanged). A catalog question can
/// only be answered once its area is judged relevant and the question is
/// active (its condition holds); free-text answers must be non-blank.
/// `note = None` keeps an existing note. Changing an answer drops
/// (transitively) every answer — and its note — whose question is no longer
/// active; changing the relevance clears the area's answers and notes — a
/// stale answer is never counted. History is not written here (see
/// [`apply_history`]).
pub fn record_answer(
    decisions: &mut Decisions,
    area: &str,
    question_id: &str,
    answers: &[String],
    note: Option<&str>,
) -> Result<(), String> {
    if note.is_some_and(|n| n.trim().is_empty()) {
        return Err("--note must be non-blank when given".to_string());
    }
    if area == DOMAIN_AREA {
        let Some(dq) = decisions.questions.iter_mut().find(|q| q.id == question_id) else {
            return Err(format!(
                "no domain question `{question_id}` (add it with `sitecraft decide add-question --id {question_id} --text ...`)"
            ));
        };
        let v = value_from_input(dq.multi, &dq.choices, answers).map_err(|e| {
            format!(
                "invalid answer to domain question `{question_id}`: {e}; choices: {:?}",
                dq.choices
            )
        })?;
        dq.answer = Some(v);
        if let Some(n) = note {
            dq.note = Some(n.to_string());
        }
        return Ok(());
    }
    let a = match catalog::area_by_id(area) {
        Determination::Known(a) => a,
        Determination::Undetermined(why) => {
            return Err(format!(
                "{} (areas: run `sitecraft decide next` to see what to answer)",
                why.as_str()
            ))
        }
    };
    if question_id == RELEVANCE_QUESTION {
        if note.is_some() {
            return Err("--note applies to a question answer, not to `relevance`".to_string());
        }
        let rel = match answers {
            [one] if one == RELEVANT => Relevance::Relevant,
            [one] if one == IRRELEVANT => Relevance::Irrelevant,
            other => {
                return Err(format!(
                    "relevance must be exactly one of `{RELEVANT}` or `{IRRELEVANT}`, got {other:?}"
                ))
            }
        };
        let ad = decisions
            .areas
            .entry(area.to_string())
            .or_insert_with(|| AreaDecision {
                relevance: None,
                answers: BTreeMap::new(),
                notes: BTreeMap::new(),
            });
        if ad.relevance != Some(rel) {
            // Answers were given for the previous relevance state; they do not
            // count for the new one.
            ad.answers.clear();
            ad.notes.clear();
        }
        ad.relevance = Some(rel);
        return Ok(());
    }
    let Some(q) = a.questions.iter().find(|q| q.id == question_id) else {
        return Err(format!(
            "`{question_id}` is not a question of area `{area}` (run `sitecraft decide next`)"
        ));
    };
    let v = value_from_input(q.multi, &q.choices, answers).map_err(|e| {
        format!(
            "invalid answer to {area}/{question_id}: {e}; choices: {:?}",
            q.choices
        )
    })?;
    let Some(ad) = decisions.areas.get_mut(area) else {
        return Err(format!(
            "area `{area}` has no relevance decision yet; run `sitecraft decide record --area {area} --question {RELEVANCE_QUESTION} --answer {RELEVANT}` first"
        ));
    };
    if ad.relevance != Some(Relevance::Relevant) {
        return Err(format!(
            "area `{area}` is not judged relevant; run `sitecraft decide record --area {area} --question {RELEVANCE_QUESTION} --answer {RELEVANT}` first"
        ));
    }
    if !catalog::is_active(q, &ad.answers) {
        return Err(format!(
            "{area}/{question_id} is not active for the answers recorded so far (run `sitecraft decide next`)"
        ));
    }
    ad.answers.insert(question_id.to_string(), v);
    if let Some(n) = note {
        ad.notes.insert(question_id.to_string(), n.to_string());
    }
    prune_inactive(&a, &mut ad.answers);
    ad.notes.retain(|qid, _| ad.answers.contains_key(qid));
    Ok(())
}

/// Drop every answer whose question is no longer active, repeatedly, so a
/// follow-up of a dropped follow-up goes too. An answer only counts if it was
/// given for the current state of its gates.
fn prune_inactive(area: &Area, answers: &mut BTreeMap<String, AnswerValue>) {
    loop {
        let stale: Vec<String> = area
            .questions
            .iter()
            .filter(|q| answers.contains_key(&q.id) && !catalog::is_active(q, answers))
            .map(|q| q.id.clone())
            .collect();
        if stale.is_empty() {
            return;
        }
        for id in stale {
            answers.remove(&id);
        }
    }
}

/// Record the purpose (D1). The text is trimmed; a blank text → `Err`
/// (record unchanged).
pub fn set_purpose(decisions: &mut Decisions, text: &str) -> Result<(), String> {
    let t = text.trim();
    if t.is_empty() {
        return Err("--text must be non-blank".to_string());
    }
    decisions.purpose = Some(Purpose {
        text: t.to_string(),
    });
    Ok(())
}

/// The history rows describing how `after` differs from `before`, all stamped
/// `at` (rulings D8).
///
/// Contract: a row is produced for every value that existed in `before` and
/// is different in `after` — purpose text, an area's relevance, a catalog
/// answer, a note (`<q>#note`), a domain answer or note, a capability table
/// (`area = "capability"`, `question = <name>`, old/new as canonical JSON).
/// A value present in
/// `before` and absent in `after` is a row with `new = ""` and `pruned =
/// true` (follow-up pruning, or answers cleared by a relevance change). A
/// value recorded for the first time (absent in `before`) is not a change
/// and produces no row; an identical re-record produces no row.
#[must_use]
pub fn diff_history(before: &Decisions, after: &Decisions, at: &str) -> Vec<HistoryEntry> {
    let mut out = Vec::new();
    let mut row = |area: &str, question: &str, old: String, new: Option<String>| {
        let pruned = new.is_none();
        out.push(HistoryEntry {
            area: area.to_string(),
            question: question.to_string(),
            old,
            new: new.unwrap_or_default(),
            at: at.to_string(),
            pruned,
        });
    };
    if let (Some(o), Some(n)) = (&before.purpose, &after.purpose) {
        if o.text != n.text {
            row(
                PURPOSE_AREA,
                PURPOSE_QUESTION,
                o.text.clone(),
                Some(n.text.clone()),
            );
        }
    }
    let empty = AreaDecision {
        relevance: None,
        answers: BTreeMap::new(),
        notes: BTreeMap::new(),
    };
    for (area_id, ob) in &before.areas {
        let na = after.areas.get(area_id).unwrap_or(&empty);
        if let (Some(o), Some(n)) = (ob.relevance, na.relevance) {
            if o != n {
                row(
                    area_id,
                    RELEVANCE_QUESTION,
                    o.word().to_string(),
                    Some(n.word().to_string()),
                );
            }
        }
        for (qid, ov) in &ob.answers {
            match na.answers.get(qid) {
                Some(nv) if nv == ov => {}
                Some(nv) => row(area_id, qid, ov.display(), Some(nv.display())),
                None => row(area_id, qid, ov.display(), None),
            }
        }
        for (qid, on) in &ob.notes {
            let key = format!("{qid}{NOTE_SUFFIX}");
            match na.notes.get(qid) {
                Some(nn) if nn == on => {}
                Some(nn) => row(area_id, &key, on.clone(), Some(nn.clone())),
                None => row(area_id, &key, on.clone(), None),
            }
        }
    }
    for oq in &before.questions {
        let nq = after.questions.iter().find(|q| q.id == oq.id);
        if let Some(ov) = &oq.answer {
            match nq.and_then(|q| q.answer.as_ref()) {
                Some(nv) if nv == ov => {}
                Some(nv) => row(DOMAIN_AREA, &oq.id, ov.display(), Some(nv.display())),
                None => row(DOMAIN_AREA, &oq.id, ov.display(), None),
            }
        }
        if let Some(on) = &oq.note {
            let key = format!("{}{NOTE_SUFFIX}", oq.id);
            match nq.and_then(|q| q.note.as_ref()) {
                Some(nn) if nn == on => {}
                Some(nn) => row(DOMAIN_AREA, &key, on.clone(), Some(nn.clone())),
                None => row(DOMAIN_AREA, &key, on.clone(), None),
            }
        }
    }
    for (name, ov) in &before.capabilities {
        match after.capabilities.get(name) {
            Some(nv) if nv == ov => {}
            Some(nv) => row(
                capability::HISTORY_AREA,
                name,
                capability::canonical_json(ov),
                Some(capability::canonical_json(nv)),
            ),
            None => row(
                capability::HISTORY_AREA,
                name,
                capability::canonical_json(ov),
                None,
            ),
        }
    }
    out
}

/// Record (or replace) the raw `[capabilities.<name>]` table (in memory;
/// call [`save`]). The table must parse ([`capability::parse`]), else `Err`
/// with the record unchanged.
pub fn record_capability(
    decisions: &mut Decisions,
    name: &str,
    table: toml::Value,
) -> Result<(), String> {
    capability::parse(name, &table)?;
    decisions.capabilities.insert(name.to_string(), table);
    Ok(())
}

/// Append [`diff_history`] of `before` → `after` to `after.history` and
/// return the appended rows.
pub fn apply_history(before: &Decisions, after: &mut Decisions, at: &str) -> Vec<HistoryEntry> {
    let rows = diff_history(before, after, at);
    after.history.extend(rows.iter().cloned());
    rows
}

/// Current time as UTC RFC 3339 (`YYYY-MM-DDTHH:MM:SSZ`). A clock before the
/// Unix epoch → `Err` (no fabricated timestamp).
pub fn now_utc_rfc3339() -> Result<String, String> {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| format!("the system clock is before the Unix epoch: {e}"))?
        .as_secs();
    Ok(format_utc(secs))
}

/// `secs` since the Unix epoch as `YYYY-MM-DDTHH:MM:SSZ` (proleptic
/// Gregorian, Howard Hinnant's civil-from-days).
fn format_utc(secs: u64) -> String {
    let days = secs / 86_400;
    let rem = secs % 86_400;
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    // Post-epoch only, so every intermediate is non-negative and u64 suffices
    // (`days` < 2^48, far from overflow).
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z % 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + u64::from(month <= 2);
    format!("{year:04}-{month:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

/// Add an (unanswered) domain question. Duplicate `id` or empty `id`/`text`
/// (or an empty / duplicate choice) → `Err` (record unchanged).
pub fn add_question(
    decisions: &mut Decisions,
    id: &str,
    text: &str,
    choices: Vec<String>,
    multi: bool,
) -> Result<(), String> {
    if id.trim().is_empty() || text.trim().is_empty() {
        return Err("--id and --text must be non-empty".to_string());
    }
    if decisions.questions.iter().any(|q| q.id == id) {
        return Err(format!("a domain question with id `{id}` already exists"));
    }
    if choices.iter().any(|c| c.trim().is_empty()) {
        return Err("--choice values must be non-empty".to_string());
    }
    let distinct: BTreeSet<&String> = choices.iter().collect();
    if distinct.len() != choices.len() {
        return Err("--choice values must be distinct".to_string());
    }
    decisions.questions.push(DomainQuestion {
        id: id.to_string(),
        text: text.to_string(),
        choices,
        multi,
        answer: None,
        note: None,
    });
    Ok(())
}

/// Record the classification of `feature`. Empty `feature` or blank
/// `harness` → `Err`. `fit = Some(_)` records the user's fit answer;
/// `fit = None` keeps a previously recorded fit only when the feature stays a
/// dedicated harness with the same `harness` name (otherwise the fit is
/// dropped: it was given for another plugin). Whether a fit may be recorded
/// at all (the plugin exists) is decided by the caller.
pub fn classify(
    decisions: &mut Decisions,
    feature: &str,
    kind: FeatureKind,
    harness: Option<String>,
    fit: Option<Fit>,
) -> Result<(), String> {
    if feature.trim().is_empty() {
        return Err("--feature must be non-empty".to_string());
    }
    if harness.as_deref().is_some_and(|h| h.trim().is_empty()) {
        return Err("--harness must be non-empty when given".to_string());
    }
    if fit.is_some() && kind != FeatureKind::DedicatedHarness {
        return Err("--confirm-fit only applies to --kind dedicated-harness".to_string());
    }
    let fit = match fit {
        Some(f) => Some(f),
        None => decisions
            .features
            .get(feature)
            .filter(|old| {
                kind == FeatureKind::DedicatedHarness
                    && old.kind == FeatureKind::DedicatedHarness
                    && old.harness == harness
            })
            .and_then(|old| old.fit),
    };
    decisions
        .features
        .insert(feature.to_string(), FeatureClass { kind, harness, fit });
    Ok(())
}

/// The `decide` command that settles one pending item.
fn fix_hint(item: &Next) -> String {
    match item {
        Next::Ask {
            area,
            question_id,
            choices,
            multi,
            ..
        } => {
            let answer = if choices.is_empty() {
                "<free text>".to_string()
            } else {
                choices.join("|")
            };
            let more = if *multi { " [--answer ...]" } else { "" };
            format!(
                "{area}/{question_id}: run `sitecraft decide record --area {area} --question {question_id} --answer {answer}{more}`"
            )
        }
        Next::Done => String::new(),
    }
}

/// The pending purpose item of [`check_report`] (D1).
pub const PURPOSE_HINT: &str =
    "purpose: not recorded, run `sitecraft decide purpose --text \"<要約>\"` (the purpose summary agreed with the user in SKILL Phase 1)";

/// The pending items of the capabilities (D2): an undecided relevance, and
/// for a relevant capability a missing `how` / `done_when` / `verify`.
fn capability_findings(decisions: &Decisions) -> Result<Vec<String>, String> {
    let caps = decisions.parsed_capabilities()?;
    let mut out = Vec::new();
    for (name, c) in &caps {
        let missing = c.pending();
        if missing.is_empty() {
            continue;
        }
        out.push(format!(
            "capabilities.{name}: {} not recorded, ask the user and run `sitecraft decide capability --name {name} --relevance relevant|irrelevant --how <text> --done-when <text> --verify-kind {}` (with the kind's arguments)",
            missing.join(" / "),
            capability::KINDS.join("|")
        ));
    }
    Ok(out)
}

/// Pending catalog / domain items, pending capability keys, plus fired
/// violation rules, as findings.
fn findings(decisions: &Decisions) -> Determination<Vec<harness_core::verdict::Reason>> {
    let items = match pending(decisions) {
        Determination::Known(i) => i,
        Determination::Undetermined(why) => return Determination::Undetermined(why),
    };
    let caps = match capability_findings(decisions) {
        Ok(c) => c,
        Err(e) => return Determination::undetermined(format!("inconsistent decision record: {e}")),
    };
    let mut out: Vec<_> = items
        .iter()
        .map(|i| harness_core::verdict::Reason::new(fix_hint(i)))
        .collect();
    out.extend(caps.into_iter().map(harness_core::verdict::Reason::new));
    for f in rules::evaluate(decisions) {
        if f.severity == Severity::Violation {
            out.push(harness_core::verdict::Reason::new(f.line()));
        }
    }
    Determination::known(out)
}

/// Are all required catalog / domain decisions present and consistent?
///
/// Contract: `Clean` iff every catalog area has a relevance, every relevant
/// area's active questions are answered, every domain question is answered,
/// every capability has a relevance and every relevant capability has `how`,
/// `done_when` and `verify` (D2), and no violation rule of
/// [`super::rules::RULES`] fires; otherwise `Violation` naming the pending
/// area / question ids, the pending capability keys and the fired rules
/// (`<id>: <text> (<area>/<question> = <answer>, …)`); catalog unavailable or
/// record inconsistent (a capability table that does not parse included) →
/// `Undetermined`. Whether a capability has been *verified* is not part of
/// this verdict (that is the Stop gate's capability-record check). Warning rules never affect the
/// verdict. The purpose is **not** part of this verdict (it is what the Stop
/// gate composes); `decide check` adds it via [`check_report`].
pub fn check(decisions: &Decisions) -> Verdict {
    match findings(decisions) {
        Determination::Known(items) => Verdict::from_findings(items),
        Determination::Undetermined(why) => why.into_verdict(),
    }
}

/// What `sitecraft decide check` reports.
#[derive(Debug)]
pub struct CheckReport {
    /// [`check`] plus a pending purpose ([`PURPOSE_HINT`]) when the purpose
    /// is missing or blank.
    pub verdict: Verdict,
    /// Fired warning rules as `<id>: <text> (…)` lines (printed as
    /// `warning: …`; never change the exit code).
    pub warnings: Vec<String>,
}

/// [`check`] plus the purpose requirement (D1) and the warning rules (U4).
#[must_use]
pub fn check_report(decisions: &Decisions) -> CheckReport {
    let warnings = rules::evaluate(decisions)
        .into_iter()
        .filter(|f| f.severity == Severity::Warning)
        .map(|f| f.line())
        .collect();
    let verdict = match findings(decisions) {
        Determination::Known(mut items) => {
            if decisions.purpose_text().is_none() {
                items.insert(0, harness_core::verdict::Reason::new(PURPOSE_HINT));
            }
            Verdict::from_findings(items)
        }
        Determination::Undetermined(why) => why.into_verdict(),
    };
    CheckReport { verdict, warnings }
}

fn absent_record(root: &Path) -> Verdict {
    Verdict::undetermined(format!(
        "{} does not exist: no design decision has been recorded yet",
        path_for(root).display()
    ))
}

/// [`load`] then [`check`] for `root` (the Stop gate's view): absent record →
/// `Undetermined` (nothing decided is not "all decided"); load `Undetermined`
/// forwarded.
pub fn check_root(root: &Path) -> Verdict {
    match load(root) {
        Determination::Known(Some(d)) => check(&d),
        Determination::Known(None) => absent_record(root),
        Determination::Undetermined(why) => why.into_verdict(),
    }
}

/// [`load`] then [`check_report`] for `root` (`decide check`): absent record →
/// `Undetermined` with no warnings; load `Undetermined` forwarded.
#[must_use]
pub fn check_root_report(root: &Path) -> CheckReport {
    match load(root) {
        Determination::Known(Some(d)) => check_report(&d),
        Determination::Known(None) => CheckReport {
            verdict: absent_record(root),
            warnings: Vec::new(),
        },
        Determination::Undetermined(why) => CheckReport {
            verdict: why.into_verdict(),
            warnings: Vec::new(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_utc_known_points() {
        assert_eq!(format_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_utc(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(format_utc(1_700_000_000), "2023-11-14T22:13:20Z");
    }

    fn known_catalog() -> Vec<Area> {
        match catalog::areas() {
            Determination::Known(a) => a,
            Determination::Undetermined(u) => panic!("catalog: {}", u.as_str()),
        }
    }

    /// A record with only `area` relevant and the gate chain of `q` answered
    /// so that `q` is active.
    fn activate(area: &Area, q: &Question) -> Decisions {
        let mut d = Decisions::empty();
        record_answer(
            &mut d,
            &area.id,
            RELEVANCE_QUESTION,
            &[RELEVANT.to_string()],
            None,
        )
        .unwrap();
        let mut chain = Vec::new();
        let mut cur = q;
        while let Some(c) = &cur.when {
            let gate = area
                .questions
                .iter()
                .find(|g| g.id == c.question_id)
                .unwrap_or_else(|| {
                    panic!("{}/{}: gate `{}` missing", area.id, cur.id, c.question_id)
                });
            chain.push((gate, c.answer_in[0].clone()));
            cur = gate;
        }
        for (gate, ans) in chain.into_iter().rev() {
            record_answer(&mut d, &area.id, &gate.id, &[ans], None)
                .unwrap_or_else(|e| panic!("{}/{} gate: {e}", area.id, gate.id));
        }
        d
    }

    /// Every option of every catalog question can be recorded through
    /// [`record_answer`] (alone, and for a multi question also together with
    /// every other option), survives [`validate`], and every follow-up
    /// condition only references real options of an existing gate question.
    #[test]
    fn every_catalog_option_is_recordable_and_every_condition_is_reachable() {
        let all = known_catalog();
        let mut recorded = 0;
        for area in &all {
            for q in &area.questions {
                if let Some(c) = &q.when {
                    let gate = area
                        .questions
                        .iter()
                        .find(|g| g.id == c.question_id)
                        .unwrap_or_else(|| panic!("{}/{}: no gate", area.id, q.id));
                    assert!(
                        !c.answer_in.is_empty(),
                        "{}/{}: empty condition",
                        area.id,
                        q.id
                    );
                    for v in &c.answer_in {
                        assert!(
                            gate.choices.contains(v),
                            "{}/{}: condition value {v:?} is not an option of {}",
                            area.id,
                            q.id,
                            gate.id
                        );
                        // Recording that gate option activates the follow-up.
                        let mut d = activate(area, gate);
                        record_answer(&mut d, &area.id, &gate.id, std::slice::from_ref(v), None)
                            .unwrap();
                        let ad = &d.areas[&area.id];
                        assert!(
                            catalog::is_active(q, &ad.answers),
                            "{}/{} via {v}",
                            area.id,
                            q.id
                        );
                    }
                }
                let opts: Vec<String> = if q.choices.is_empty() {
                    vec!["free text".to_string()]
                } else {
                    q.choices.clone()
                };
                for o in &opts {
                    let mut d = activate(area, q);
                    record_answer(&mut d, &area.id, &q.id, std::slice::from_ref(o), None)
                        .unwrap_or_else(|e| panic!("{}/{} = {o:?}: {e}", area.id, q.id));
                    validate(&d, &all)
                        .unwrap_or_else(|e| panic!("{}/{} = {o:?}: {e}", area.id, q.id));
                    assert!(d.areas[&area.id].answers.contains_key(&q.id));
                    recorded += 1;
                }
                if q.multi {
                    let mut d = activate(area, q);
                    record_answer(&mut d, &area.id, &q.id, &q.choices, None).unwrap();
                    validate(&d, &all).unwrap();
                }
            }
        }
        assert!(recorded > 100, "too few options exercised: {recorded}");
    }

    /// Option values are never shadowed: within a question, options are
    /// distinct and non-blank.
    #[test]
    fn catalog_options_are_distinct_and_non_blank() {
        for area in known_catalog() {
            for q in &area.questions {
                let set: BTreeSet<&String> = q.choices.iter().collect();
                assert_eq!(set.len(), q.choices.len(), "{}/{}", area.id, q.id);
                assert!(q.choices.iter().all(|c| !c.trim().is_empty()));
            }
        }
    }

    #[test]
    fn first_record_is_not_a_history_row_but_a_change_is() {
        let before = Decisions::empty();
        let mut after = before.clone();
        set_purpose(&mut after, "a").unwrap();
        assert!(diff_history(&before, &after, "t").is_empty());
        let mut again = after.clone();
        set_purpose(&mut again, "b").unwrap();
        let rows = diff_history(&after, &again, "t");
        assert_eq!(rows.len(), 1);
        assert_eq!((rows[0].old.as_str(), rows[0].new.as_str()), ("a", "b"));
    }
}
