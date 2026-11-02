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

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::catalog::{self, Area, Question, IRRELEVANT, RELEVANCE_QUESTION, RELEVANT};
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

/// Is `answer` acceptable for a question with `choices` (empty = free text,
/// which still must be non-blank)?
fn answer_ok(choices: &[String], answer: &str) -> bool {
    if choices.is_empty() {
        !answer.trim().is_empty()
    } else {
        choices.iter().any(|c| c == answer)
    }
}

/// Structural consistency of a record against the catalog: every area id is a
/// catalog area, every recorded catalog answer belongs to a question of its
/// area, is one of its choices and is active (answers exist only in relevant
/// areas, never for inactive follow-ups), domain questions have unique non-empty
/// ids/texts and valid answers, features have non-empty names.
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
            if !answer_ok(&q.choices, ans) {
                return Err(format!(
                    "areas.{area_id}.answers.{qid} = {ans:?} is not one of its choices {:?}",
                    q.choices
                ));
            }
            if !catalog::is_active(q, &ad.answers) {
                return Err(format!(
                    "areas.{area_id}.answers.{qid} answers a question that is not active for the other recorded answers (stale answer)"
                ));
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
            if !answer_ok(&dq.choices, ans) {
                return Err(format!(
                    "question `{}`: answer {ans:?} is not one of its choices {:?}",
                    dq.id, dq.choices
                ));
            }
        }
    }
    if d.features.keys().any(|f| f.trim().is_empty()) {
        return Err("a [features] entry has an empty name".to_string());
    }
    Ok(())
}

/// Validate `d` against the catalog; any failure is `Undetermined`.
fn validated(d: &Decisions, label: &str) -> Determination<()> {
    match catalog::areas() {
        Determination::Known(all) => match validate(d, &all) {
            Ok(()) => Determination::known(()),
            Err(e) => Determination::undetermined(format!("{label}: {e}")),
        },
        Determination::Undetermined(why) => Determination::Undetermined(why),
    }
}

/// Load the record.
///
/// Contract: file absent (`NotFound`) → `Known(None)`; read error, non-UTF-8,
/// TOML error, unknown key, unknown area id, an answer not among its
/// question's choices, an answer to an inactive question or a non-relevant
/// area, or an entry that exists but cannot be read (e.g. a dangling
/// symlink) → `Undetermined`; else `Known(Some(_))`.
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
    match validated(&d, &path.display().to_string()) {
        Determination::Known(()) => Determination::known(Some(d)),
        Determination::Undetermined(why) => Determination::Undetermined(why),
    }
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
    }
}

fn ask(area: &str, q: &Question) -> Next {
    Next::Ask {
        area: area.to_string(),
        question_id: q.id.clone(),
        text: q.text.clone(),
        choices: q.choices.clone(),
    }
}

/// Every pending item, in asking order, as `Next::Ask` values. Shared by
/// [`next`] (first item) and [`check`] (all items), so the two can never
/// disagree about what is pending.
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
/// first unanswered domain question; nothing left → `Done`. Catalog
/// unavailable or record inconsistent (unknown area) → `Undetermined`.
pub fn next(decisions: &Decisions) -> Determination<Next> {
    // An empty pending list is an observed "nothing left" (the catalog is
    // non-empty, so every area contributed either a pending item or a
    // recorded decision), hence `Done`.
    pending(decisions).map(|p| match p.into_iter().next() {
        Some(item) => item,
        None => Next::Done,
    })
}

/// Record `answer` for `question_id` in `area` (in memory; call [`save`]).
///
/// Contract: `question_id == RELEVANCE_QUESTION` sets the area's relevance
/// (`relevant`/`irrelevant`, anything else is `Err`); `area == DOMAIN_AREA`
/// answers a domain question; unknown area, unknown question, or an answer
/// not among a non-empty `choices` → `Err` (record unchanged). A catalog
/// question can only be answered once its area is judged relevant and the
/// question is active (its condition holds); free-text answers must be
/// non-blank. Changing an answer drops (transitively) every answer whose
/// question is no longer active; changing the relevance clears the area's
/// answers — a stale answer is never counted.
pub fn record_answer(
    decisions: &mut Decisions,
    area: &str,
    question_id: &str,
    answer: &str,
) -> Result<(), String> {
    if area == DOMAIN_AREA {
        let Some(dq) = decisions.questions.iter_mut().find(|q| q.id == question_id) else {
            return Err(format!(
                "no domain question `{question_id}` (add it with `sitecraft decide add-question --id {question_id} --text ...`)"
            ));
        };
        if !answer_ok(&dq.choices, answer) {
            return Err(format!(
                "{answer:?} is not a valid answer to domain question `{question_id}`; choices: {:?}",
                dq.choices
            ));
        }
        dq.answer = Some(answer.to_string());
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
        let rel = match answer {
            RELEVANT => Relevance::Relevant,
            IRRELEVANT => Relevance::Irrelevant,
            other => {
                return Err(format!(
                    "relevance must be `{RELEVANT}` or `{IRRELEVANT}`, got {other:?}"
                ))
            }
        };
        let ad = decisions
            .areas
            .entry(area.to_string())
            .or_insert_with(|| AreaDecision {
                relevance: None,
                answers: BTreeMap::new(),
            });
        if ad.relevance != Some(rel) {
            // Answers were given for the previous relevance state; they do not
            // count for the new one.
            ad.answers.clear();
        }
        ad.relevance = Some(rel);
        return Ok(());
    }
    let Some(q) = a.questions.iter().find(|q| q.id == question_id) else {
        return Err(format!(
            "`{question_id}` is not a question of area `{area}` (run `sitecraft decide next`)"
        ));
    };
    if !answer_ok(&q.choices, answer) {
        return Err(format!(
            "{answer:?} is not a valid answer to {area}/{question_id}; choices: {:?}",
            q.choices
        ));
    }
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
    ad.answers
        .insert(question_id.to_string(), answer.to_string());
    prune_inactive(&a, &mut ad.answers);
    Ok(())
}

/// Drop every answer whose question is no longer active, repeatedly, so a
/// follow-up of a dropped follow-up goes too. An answer only counts if it was
/// given for the current state of its gates.
fn prune_inactive(area: &Area, answers: &mut BTreeMap<String, String>) {
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

/// Add an (unanswered) domain question. Duplicate `id` or empty `id`/`text`
/// (or an empty / duplicate choice) → `Err` (record unchanged).
pub fn add_question(
    decisions: &mut Decisions,
    id: &str,
    text: &str,
    choices: Vec<String>,
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
        answer: None,
    });
    Ok(())
}

/// Record the classification of `feature`. Empty `feature` → `Err`.
pub fn classify(
    decisions: &mut Decisions,
    feature: &str,
    kind: FeatureKind,
    harness: Option<String>,
) -> Result<(), String> {
    if feature.trim().is_empty() {
        return Err("--feature must be non-empty".to_string());
    }
    if harness.as_deref().is_some_and(|h| h.trim().is_empty()) {
        return Err("--harness must be non-empty when given".to_string());
    }
    decisions
        .features
        .insert(feature.to_string(), FeatureClass { kind, harness });
    Ok(())
}

/// The `decide` command that settles one pending item.
fn fix_hint(item: &Next) -> String {
    match item {
        Next::Ask {
            area,
            question_id,
            choices,
            ..
        } => {
            let answer = if choices.is_empty() {
                "<free text>".to_string()
            } else {
                choices.join("|")
            };
            format!(
                "{area}/{question_id}: run `sitecraft decide record --area {area} --question {question_id} --answer {answer}`"
            )
        }
        Next::Done => String::new(),
    }
}

/// Are all required decisions present?
///
/// Contract: `Clean` iff every catalog area has a relevance, every relevant
/// area's active questions are answered, and every domain question is
/// answered; otherwise `Violation` naming the pending area / question ids;
/// catalog unavailable or record inconsistent → `Undetermined`.
pub fn check(decisions: &Decisions) -> Verdict {
    match pending(decisions) {
        Determination::Known(items) => Verdict::from_findings(
            items
                .iter()
                .map(|i| harness_core::verdict::Reason::new(fix_hint(i)))
                .collect(),
        ),
        Determination::Undetermined(why) => why.into_verdict(),
    }
}

/// [`load`] then [`check`] for `root`: absent record → `Undetermined`
/// (nothing decided is not "all decided"); load `Undetermined` forwarded.
pub fn check_root(root: &Path) -> Verdict {
    match load(root) {
        Determination::Known(Some(d)) => check(&d),
        Determination::Known(None) => Verdict::undetermined(format!(
            "{} does not exist: no design decision has been recorded yet",
            path_for(root).display()
        )),
        Determination::Undetermined(why) => why.into_verdict(),
    }
}
