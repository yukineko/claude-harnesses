//! `condukt review-brief`: a per-item, DETERMINISTIC reviewer digest composed
//! from STATIC persisted signals only — no LLM, no runtime API call, no live
//! git diff. It turns a run/task id into "here's what changed, why it's
//! risky, look here first" for a human reviewer.
//!
//! Signals used (all real, already-persisted; nothing invented):
//! - **Intent**: the run's `goal` ([`crate::state::RunState`]) plus the
//!   task's `title` / `done_criteria` / `kind` from the decomposition
//!   sidecar ([`crate::model::Task`]).
//! - **Scope**: the task's DECLARED `touched_files` / `target_symbols`
//!   (decomposition-level, not a recomputed live diff).
//! - **Sensitive-path driver**: [`blastguard::diffrisk::SensitiveConfig`]
//!   classifying each declared touched file.
//! - **Tripped invariants**: [`overwatch::violation::ViolationEvent`]s whose
//!   `task_key` matches this run/task (the same `"<run_id>/<task_id>"` key
//!   format [`crate::diffrisk_record::record_post_execution_diff_risk`]
//!   writes).
//!
//! The [`build_review_brief`] function is pure (no I/O): the CLI layer
//! (`main.rs`) does the store reads and passes already-loaded data in, which
//! keeps this module directly unit-testable.

use crate::precedent::{match_precedent, Precedent, PrecedentMatch};
use blastguard::diffrisk::SensitiveConfig;
use harness_core::verdict::Required;
use overwatch::violation::{ViolationEvent, ViolationSource};
use serde::Serialize;
use std::collections::BTreeSet;

/// Grounding "why this task exists" facts, sourced from the run-state goal
/// and the decomposition's per-task fields. Used both as the pure function's
/// input and (verbatim) as the brief's `intent` field.
#[derive(Debug, Clone, Serialize)]
pub struct Intent {
    /// The run's overall goal (`RunState::goal`).
    pub run_goal: String,
    /// The task's declared title.
    pub task_title: String,
    /// The task's declared `done_criteria`, if any.
    pub done_criteria: Option<String>,
    /// The task's declared `kind` (fix|feature|chore|...), if any.
    pub kind: Option<String>,
}

/// One tripped invariant surfaced in the brief: a matched, deduplicated
/// (source, signature) pair from the overwatch violation ledger, with its
/// free-text detail carried through for human context.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct TrippedInvariant {
    /// Which gate/tool raised it (`blastguard`|`propguard`|`specguard`|`mutategate`).
    pub source: String,
    /// The normalized signature (`overwatch::violation::normalize_signature`).
    pub signature: String,
    /// Free-text detail, if the recorded event carried one.
    pub detail: Option<String>,
}

/// A coarse, deterministically-derived risk tier. `High` whenever a
/// sensitive path was touched OR any invariant tripped for this task;
/// `Medium` when neither fired but the task's declared scope spans more than
/// one file; `Low` otherwise (a single, non-sensitive, invariant-clean file).
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RiskTier {
    Low,
    Medium,
    High,
}

impl std::fmt::Display for RiskTier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            RiskTier::Low => "low",
            RiskTier::Medium => "medium",
            RiskTier::High => "high",
        };
        write!(f, "{s}")
    }
}

/// The full reviewer digest for one run/task, composed entirely from static,
/// already-persisted signals.
#[derive(Debug, Clone, Serialize)]
pub struct ReviewBrief {
    pub intent: Intent,
    /// The task's DECLARED touched-files footprint (decomposition-level,
    /// not a recomputed live diff).
    pub touched_files: Vec<String>,
    /// The task's DECLARED target symbols (decomposition-level).
    pub target_symbols: Vec<String>,
    pub risk_tier: RiskTier,
    /// Human-readable risk drivers (at minimum "touches sensitive path" and
    /// one entry per distinct tripped-invariant signature).
    pub risk_drivers: Vec<String>,
    /// Invariants tripped for THIS task (task_key-matched, deduplicated by
    /// (source, signature)).
    pub tripped_invariants: Vec<TrippedInvariant>,
    /// Ordered file list a reviewer should look at first: sensitive-path
    /// files, then files implicated by a tripped invariant's detail text,
    /// then the remaining declared touched files — deduplicated, stable.
    pub look_here_first: Vec<String>,
    /// The ratified precedent this task's declared shape matched, set ONLY
    /// when that match ALSO triggered the tier downgrade to `Low` (see the
    /// SAFETY INVARIANT on [`build_review_brief`] — a sensitive-path or
    /// tripped-invariant change is never downgraded, so this stays `None`
    /// for it even if its shape happens to match a precedent). Absent from
    /// JSON output (`skip_serializing_if`) so an empty precedent store — or
    /// today's callers, before this field existed — sees byte-identical
    /// output.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub precedented: Option<PrecedentMatch>,
}

/// Stable lowercase label for a [`ViolationSource`] (mirrors the private
/// `ViolationSource::token()` in overwatch, which is not `pub`).
fn source_label(source: ViolationSource) -> &'static str {
    match source {
        ViolationSource::Blastguard => "blastguard",
        ViolationSource::Propguard => "propguard",
        ViolationSource::Specguard => "specguard",
        ViolationSource::Mutategate => "mutategate",
        ViolationSource::Donegate => "donegate",
        ViolationSource::Reviewgate => "reviewgate",
        ViolationSource::Tdd => "tdd",
        ViolationSource::Budgetguard => "budgetguard",
        ViolationSource::Autoflow => "autoflow",
        ViolationSource::Ctxrot => "ctxrot",
    }
}

/// Build a [`ReviewBrief`] from already-loaded, static inputs. Pure: no I/O,
/// no wall-clock reads, no LLM call — the same inputs always produce the
/// same brief.
///
/// `task_key` is the exact `"<run_id>/<task_id>"` string
/// [`crate::diffrisk_record::record_post_execution_diff_risk`] writes to
/// overwatch; `violations` is the FULL set the caller read from the ledger
/// (this function does the task_key filtering itself, so a caller does not
/// need to pre-filter — and callers under test can hand in a mixed list to
/// prove the filter excludes non-matching entries).
#[allow(clippy::too_many_arguments)]
pub fn build_review_brief(
    intent: Intent,
    task_key: &str,
    touched_files: &[String],
    target_symbols: &[String],
    violations: &[ViolationEvent],
    sensitive_cfg: &SensitiveConfig,
    precedents: &[Precedent],
    tolerance: f64,
) -> ReviewBrief {
    // Tripped invariants: only events whose task_key matches THIS task,
    // deduplicated by (source, signature) so a repeated identical violation
    // doesn't pad the list.
    let mut tripped_invariants: Vec<TrippedInvariant> = Vec::new();
    let mut seen_sigs: BTreeSet<(&'static str, String)> = BTreeSet::new();
    for ev in violations.iter().filter(|ev| ev.task_key == task_key) {
        let source = source_label(ev.source);
        let key = (source, ev.signature.clone());
        if seen_sigs.insert(key) {
            tripped_invariants.push(TrippedInvariant {
                source: source.to_string(),
                signature: ev.signature.clone(),
                detail: ev.detail.clone(),
            });
        }
    }

    // Sensitive-path classification: per-file, so ordering can put the
    // sensitive file(s) first while preserving declared order among them.
    //
    // FAIL-CLOSED: `any_sensitive` is a `Determination` — it is `Undetermined`
    // when the configured sensitive-path globs did not compile, i.e. the file
    // was never actually tested. That resolves to the RESTRICTED side (the file
    // is treated as sensitive, which forces High tier and blocks the precedent
    // downgrade below), but it is NOT silently reported as a real sensitive-path
    // hit: `undetermined_why` below drives a distinct risk-driver line so the
    // human reading this brief is told the list was a fallback caused by a
    // misconfiguration, not a measurement that this change touches auth code.
    let mut sensitive_files: Vec<String> = Vec::new();
    let mut undetermined_why: Option<String> = None;
    let mut measured_sensitive = false;
    for f in touched_files {
        match sensitive_cfg
            .any_sensitive(std::slice::from_ref(f))
            .require()
        {
            Required::Determined(true) => {
                measured_sensitive = true;
                sensitive_files.push(f.clone());
            }
            Required::Determined(false) => {}
            Required::Blocked(verdict) => {
                if undetermined_why.is_none() {
                    undetermined_why = Some(
                        verdict
                            .reason()
                            .map(|r| r.as_str().to_string())
                            .unwrap_or_else(|| "sensitive-path check undetermined".to_string()),
                    );
                }
                sensitive_files.push(f.clone());
            }
        }
    }
    let any_sensitive = !sensitive_files.is_empty();

    let mut risk_drivers: Vec<String> = Vec::new();
    if measured_sensitive {
        risk_drivers.push("touches sensitive path".to_string());
    }
    if let Some(why) = &undetermined_why {
        risk_drivers.push(format!(
            "sensitive-path check could not run ({why}) — every touched file is \
             conservatively treated as sensitive; this is NOT a measured hit"
        ));
    }
    for ti in &tripped_invariants {
        risk_drivers.push(format!(
            "invariant tripped: {} (source: {})",
            ti.signature, ti.source
        ));
    }

    let mut risk_tier = if any_sensitive || !tripped_invariants.is_empty() {
        RiskTier::High
    } else if touched_files.len() > 1 {
        RiskTier::Medium
    } else {
        RiskTier::Low
    };

    // Precedent downgrade (Google LSC "reviewed-once-applied-broadly").
    //
    // SAFETY INVARIANT: only a ROUTINE change — no sensitive path touched AND
    // no invariant tripped, so `risk_tier` here is Medium or Low, NEVER a
    // driver-forced High — may be downgraded by a precedent match. A
    // sensitive-path or tripped-invariant change is NEVER downgraded, even
    // when its declared shape also matches a ratified precedent: a precedent
    // must not hide a genuinely risky change from review.
    let mut precedented: Option<PrecedentMatch> = None;
    if !any_sensitive && tripped_invariants.is_empty() {
        if let Some(m) = match_precedent(touched_files, target_symbols, precedents, tolerance) {
            risk_tier = RiskTier::Low;
            risk_drivers.push(format!(
                "precedented: matches ratified precedent {:016x} (sim {:.2}) — spot-check only",
                m.fingerprint, m.similarity
            ));
            precedented = Some(m);
        }
    }

    // look_here_first: sensitive files first, then files implicated by a
    // tripped invariant's detail/signature text (a real textual signal — no
    // invented file<->violation linkage table), then the rest in declared
    // order. Deduplicated, stable.
    let mut look_here_first: Vec<String> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();

    for f in &sensitive_files {
        if seen.insert(f.clone()) {
            look_here_first.push(f.clone());
        }
    }
    for f in touched_files {
        if seen.contains(f) {
            continue;
        }
        let implicated = tripped_invariants.iter().any(|ti| {
            ti.signature.contains(f.as_str())
                || ti
                    .detail
                    .as_deref()
                    .map(|d| d.contains(f.as_str()))
                    .unwrap_or(false)
        });
        if implicated && seen.insert(f.clone()) {
            look_here_first.push(f.clone());
        }
    }
    for f in touched_files {
        if seen.insert(f.clone()) {
            look_here_first.push(f.clone());
        }
    }

    ReviewBrief {
        intent,
        touched_files: touched_files.to_vec(),
        target_symbols: target_symbols.to_vec(),
        risk_tier,
        risk_drivers,
        tripped_invariants,
        look_here_first,
        precedented,
    }
}

/// Render a [`ReviewBrief`] as a readable markdown digest.
pub fn to_markdown(brief: &ReviewBrief) -> String {
    let mut out = String::new();
    out.push_str(&format!("# Review Brief: {}\n\n", brief.intent.task_title));
    out.push_str(
        "_Honest-scope note: this digest is derived from DECLARED `touched_files` + \
        persisted signals only (decomposition intent, blastguard sensitive-path \
        classification, the overwatch violation ledger). Hunk-level \
        enclosing-function resolution and live-diff recomputation are out of scope._\n\n",
    );

    out.push_str("## Intent\n");
    out.push_str(&format!("- Run goal: {}\n", brief.intent.run_goal));
    out.push_str(&format!("- Task: {}\n", brief.intent.task_title));
    if let Some(dc) = &brief.intent.done_criteria {
        out.push_str(&format!("- Done criteria: {dc}\n"));
    }
    if let Some(k) = &brief.intent.kind {
        out.push_str(&format!("- Kind: {k}\n"));
    }
    out.push('\n');

    out.push_str(&format!("## Risk: {}\n", brief.risk_tier));
    if brief.risk_drivers.is_empty() {
        out.push_str("- (no risk drivers)\n");
    } else {
        for d in &brief.risk_drivers {
            out.push_str(&format!("- {d}\n"));
        }
    }
    out.push('\n');

    out.push_str("## Tripped invariants\n");
    if brief.tripped_invariants.is_empty() {
        out.push_str("- (none)\n");
    } else {
        for ti in &brief.tripped_invariants {
            match &ti.detail {
                Some(d) => out.push_str(&format!("- [{}] {}: {}\n", ti.source, ti.signature, d)),
                None => out.push_str(&format!("- [{}] {}\n", ti.source, ti.signature)),
            }
        }
    }
    out.push('\n');

    out.push_str("## Look here first\n");
    if brief.look_here_first.is_empty() {
        out.push_str("- (no touched files declared)\n");
    } else {
        for (i, f) in brief.look_here_first.iter().enumerate() {
            out.push_str(&format!("{}. {}\n", i + 1, f));
        }
    }
    out.push('\n');

    out.push_str("## Touched files (declared)\n");
    if brief.touched_files.is_empty() {
        out.push_str("- (none declared)\n");
    } else {
        for f in &brief.touched_files {
            out.push_str(&format!("- {f}\n"));
        }
    }

    if !brief.target_symbols.is_empty() {
        out.push('\n');
        out.push_str("## Target symbols (declared)\n");
        for s in &brief.target_symbols {
            out.push_str(&format!("- {s}\n"));
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::precedent::structural_fingerprint;

    /// Mirrors `main.rs::DEFAULT_PRECEDENT_TOLERANCE` (0.8) for tests that
    /// don't specifically exercise the tolerance boundary.
    const DEFAULT_TEST_TOLERANCE: f64 = 0.8;

    fn intent() -> Intent {
        Intent {
            run_goal: "ship the payment flow".to_string(),
            task_title: "wire the login endpoint".to_string(),
            done_criteria: Some("auth/login.rs compiles and tests pass".to_string()),
            kind: Some("feature".to_string()),
        }
    }

    fn matching_violation(task_key: &str) -> ViolationEvent {
        ViolationEvent {
            source: ViolationSource::Blastguard,
            signature: "blastguard:diffrisk-public-api".to_string(),
            task_key: task_key.to_string(),
            session_id: "s1".to_string(),
            ts: 100,
            detail: Some(
                "post-execution diff-risk: public-API change on a sensitive path".to_string(),
            ),
        }
    }

    fn non_matching_violation() -> ViolationEvent {
        ViolationEvent {
            source: ViolationSource::Propguard,
            signature: "propguard:prop-1".to_string(),
            task_key: "run-other/t9".to_string(),
            session_id: "s2".to_string(),
            ts: 50,
            detail: Some("unrelated failure".to_string()),
        }
    }

    #[test]
    fn build_review_brief_carries_intent_orders_sensitive_first_and_filters_by_task_key() {
        let touched = vec![
            "crates/foo/src/util.rs".to_string(),
            "crates/foo/src/auth/login.rs".to_string(),
            "crates/foo/src/helpers.rs".to_string(),
        ];
        let symbols = vec!["login".to_string()];
        let violations = vec![matching_violation("run-1/t1"), non_matching_violation()];
        let cfg = SensitiveConfig::default();

        let brief = build_review_brief(
            intent(),
            "run-1/t1",
            &touched,
            &symbols,
            &violations,
            &cfg,
            &[],
            DEFAULT_TEST_TOLERANCE,
        );

        // (a) intent fields present.
        assert_eq!(brief.intent.task_title, "wire the login endpoint");
        assert_eq!(
            brief.intent.done_criteria.as_deref(),
            Some("auth/login.rs compiles and tests pass")
        );
        assert_eq!(brief.intent.kind.as_deref(), Some("feature"));

        // (b) sensitive-path file ordered first.
        assert_eq!(brief.look_here_first[0], "crates/foo/src/auth/login.rs");

        // (c) only the task_key-matching violation is a tripped invariant.
        assert_eq!(brief.tripped_invariants.len(), 1);
        assert_eq!(
            brief.tripped_invariants[0].signature,
            "blastguard:diffrisk-public-api"
        );
        assert!(!brief
            .tripped_invariants
            .iter()
            .any(|ti| ti.signature == "propguard:prop-1"));

        assert_eq!(brief.risk_tier, RiskTier::High);
        assert!(brief
            .risk_drivers
            .iter()
            .any(|d| d == "touches sensitive path"));

        // (d) markdown and JSON both carry the same logical content.
        let md = to_markdown(&brief);
        assert!(md.contains("wire the login endpoint"));
        assert!(md.contains("crates/foo/src/auth/login.rs"));
        assert!(md.contains("honest-scope") || md.to_lowercase().contains("honest-scope"));

        let json = serde_json::to_string(&brief).expect("serializable");
        assert!(json.contains("wire the login endpoint"));
        assert!(json.contains("crates/foo/src/auth/login.rs"));
    }

    #[test]
    fn build_review_brief_low_risk_when_no_sensitive_path_and_no_invariants() {
        let touched = vec!["crates/foo/src/plain.rs".to_string()];
        let cfg = SensitiveConfig::default();
        let brief = build_review_brief(
            Intent {
                run_goal: "g".to_string(),
                task_title: "t".to_string(),
                done_criteria: None,
                kind: None,
            },
            "run-1/t1",
            &touched,
            &[],
            &[],
            &cfg,
            &[],
            DEFAULT_TEST_TOLERANCE,
        );
        assert_eq!(brief.risk_tier, RiskTier::Low);
        assert!(brief.risk_drivers.is_empty());
        assert!(brief.tripped_invariants.is_empty());
        assert_eq!(brief.look_here_first, vec!["crates/foo/src/plain.rs"]);
        // Backward-compat: with an empty precedent store, `precedented` is
        // absent from JSON (not merely `null`) — a caller reading today's
        // shape sees byte-identical output.
        assert!(brief.precedented.is_none());
        let json = serde_json::to_string(&brief).expect("serializable");
        assert!(!json.contains("precedented"));
    }

    #[test]
    fn repo_sensitive_config_raises_brief_driver_for_repo_specific_path() {
        // Regression (backlog 68b658e1): the CLI now builds the brief with
        // diffrisk_record::repo_sensitive_config() — the SAME config the
        // diff-risk recorder uses — so a repo-specific gate/plugin surface
        // like `hooks/` raises the brief's OWN "touches sensitive path"
        // driver, matching the recorded diffrisk signal (previously the brief
        // used only blastguard's bare defaults and missed these paths).
        let cfg = crate::diffrisk_record::repo_sensitive_config();
        let touched = vec![
            "crates/foo/src/plain.rs".to_string(),
            "crates/bar/hooks/stop.sh".to_string(),
        ];
        let brief = build_review_brief(
            Intent {
                run_goal: "g".to_string(),
                task_title: "t".to_string(),
                done_criteria: None,
                kind: None,
            },
            "run-1/t1",
            &touched,
            &[],
            &[],
            &cfg,
            &[],
            DEFAULT_TEST_TOLERANCE,
        );
        assert!(brief
            .risk_drivers
            .iter()
            .any(|d| d == "touches sensitive path"));
        assert_eq!(brief.look_here_first[0], "crates/bar/hooks/stop.sh");
        // Consistency proof: blastguard's bare default would NOT flag hooks/.
        let hooks = "crates/bar/hooks/stop.sh".to_string();
        assert_eq!(
            SensitiveConfig::default().any_sensitive(std::slice::from_ref(&hooks)),
            harness_core::verdict::Determination::Known(false)
        );
    }

    #[test]
    fn undetermined_sensitive_check_is_conservative_and_says_so() {
        // FAIL-CLOSED end-to-end: a misconfigured (unparseable) sensitive-glob
        // list means NO file was tested. Every touched file is treated as
        // sensitive (High tier, precedent downgrade blocked) — but the brief
        // must NOT claim the measured "touches sensitive path" driver, because
        // that would tell a human this change hits auth code when in fact the
        // check never ran.
        let cfg = SensitiveConfig::from_globs(vec!["[".to_string()]);
        let touched = vec!["src/parser.rs".to_string(), "README.md".to_string()];
        let brief = build_review_brief(
            plain_intent(),
            "run-1/t1",
            &touched,
            &[],
            &[],
            &cfg,
            &[],
            DEFAULT_TEST_TOLERANCE,
        );

        assert_eq!(
            brief.risk_tier,
            RiskTier::High,
            "an unmeasurable sensitive-path check must resolve to the restricted side"
        );
        assert!(
            !brief
                .risk_drivers
                .iter()
                .any(|d| d == "touches sensitive path"),
            "must not claim a MEASURED sensitive-path hit: {:?}",
            brief.risk_drivers
        );
        let undet = brief
            .risk_drivers
            .iter()
            .find(|d| d.contains("could not run"))
            .expect("the undetermined driver must be stated in prose");
        assert!(
            undet.contains("invalid sensitive glob") && undet.contains("NOT a measured hit"),
            "the driver must name the cause and disclaim measurement: {undet:?}"
        );
        // Every file is conservatively surfaced for review.
        for f in &touched {
            assert!(brief.look_here_first.contains(f), "{f} must be surfaced");
        }
    }

    #[test]
    fn undetermined_sensitive_check_blocks_the_precedent_downgrade() {
        // The precedent downgrade is only safe for a change PROVEN routine. An
        // undetermined sensitive-path check proves nothing, so it must not open
        // the downgrade door that a measured `false` would.
        let cfg = SensitiveConfig::from_globs(vec!["[".to_string()]);
        let touched = vec!["src/parser.rs".to_string()];
        let precedents = vec![crate::precedent::Precedent {
            fingerprint: structural_fingerprint(&touched, &[]),
            files: touched.clone(),
            symbols: vec![],
            ratified_ts: 1,
            note: "routine parser tweak".to_string(),
        }];
        let brief = build_review_brief(
            plain_intent(),
            "run-1/t1",
            &touched,
            &[],
            &[],
            &cfg,
            &precedents,
            DEFAULT_TEST_TOLERANCE,
        );
        assert!(
            brief.precedented.is_none(),
            "an undetermined risk must not be downgraded by a precedent"
        );
        assert_eq!(brief.risk_tier, RiskTier::High);
    }

    fn plain_intent() -> Intent {
        Intent {
            run_goal: "g".to_string(),
            task_title: "t".to_string(),
            done_criteria: None,
            kind: None,
        }
    }

    #[test]
    fn routine_precedent_match_downgrades_medium_to_low() {
        // A routine MULTI-FILE change (no sensitive path, no tripped
        // invariant) would otherwise be Medium (touched_files.len() > 1).
        let touched = vec![
            "crates/foo/src/a.rs".to_string(),
            "crates/foo/src/b.rs".to_string(),
        ];
        let symbols = vec!["helper".to_string()];
        let cfg = SensitiveConfig::default();

        // FAILS before the downgrade wiring: an empty precedent store cannot
        // downgrade anything.
        let brief_no_precedent = build_review_brief(
            plain_intent(),
            "run-1/t1",
            &touched,
            &symbols,
            &[],
            &cfg,
            &[],
            DEFAULT_TEST_TOLERANCE,
        );
        assert_eq!(brief_no_precedent.risk_tier, RiskTier::Medium);
        assert!(brief_no_precedent.precedented.is_none());

        // PASSES after: a ratified precedent with the IDENTICAL declared
        // shape matches exactly, downgrading the tier to Low.
        let precedent = crate::precedent::Precedent {
            fingerprint: structural_fingerprint(&touched, &symbols),
            files: touched.clone(),
            symbols: symbols.clone(),
            ratified_ts: 1,
            note: "routine helper refactor".to_string(),
        };
        let brief = build_review_brief(
            plain_intent(),
            "run-1/t1",
            &touched,
            &symbols,
            &[],
            &cfg,
            &[precedent],
            DEFAULT_TEST_TOLERANCE,
        );
        assert_eq!(brief.risk_tier, RiskTier::Low);
        let m = brief.precedented.expect("expected a precedent match");
        assert_eq!(m.similarity, 1.0);
        assert!(brief
            .risk_drivers
            .iter()
            .any(|d| d.starts_with("precedented:")));
    }

    #[test]
    fn sensitive_path_high_is_never_downgraded_even_with_a_precedent_match() {
        // SAFETY INVARIANT: a sensitive-path change stays High even when its
        // declared shape ALSO matches a ratified precedent exactly.
        let touched = vec!["crates/bar/hooks/stop.sh".to_string()];
        let symbols = vec!["run_hook".to_string()];
        let cfg = crate::diffrisk_record::repo_sensitive_config();

        let precedent = crate::precedent::Precedent {
            fingerprint: structural_fingerprint(&touched, &symbols),
            files: touched.clone(),
            symbols: symbols.clone(),
            ratified_ts: 1,
            note: "should NOT apply".to_string(),
        };
        let brief = build_review_brief(
            plain_intent(),
            "run-1/t1",
            &touched,
            &symbols,
            &[],
            &cfg,
            &[precedent],
            DEFAULT_TEST_TOLERANCE,
        );
        assert_eq!(
            brief.risk_tier,
            RiskTier::High,
            "a sensitive-path change must NEVER be downgraded by a precedent match"
        );
        assert!(
            brief.precedented.is_none(),
            "precedented is only set when the match actually drove a downgrade"
        );
        assert!(!brief
            .risk_drivers
            .iter()
            .any(|d| d.starts_with("precedented:")));
    }

    #[test]
    fn tripped_invariant_high_is_never_downgraded_even_with_a_precedent_match() {
        // SAFETY INVARIANT, invariant-tripped variant: a tripped-invariant
        // change stays High even when its declared shape ALSO matches a
        // ratified precedent exactly.
        let touched = vec!["crates/foo/src/plain.rs".to_string()];
        let symbols = vec!["helper".to_string()];
        let cfg = SensitiveConfig::default();
        let violations = vec![matching_violation("run-1/t1")];

        let precedent = crate::precedent::Precedent {
            fingerprint: structural_fingerprint(&touched, &symbols),
            files: touched.clone(),
            symbols: symbols.clone(),
            ratified_ts: 1,
            note: "should NOT apply".to_string(),
        };
        let brief = build_review_brief(
            plain_intent(),
            "run-1/t1",
            &touched,
            &symbols,
            &violations,
            &cfg,
            &[precedent],
            DEFAULT_TEST_TOLERANCE,
        );
        assert_eq!(
            brief.risk_tier,
            RiskTier::High,
            "a tripped-invariant change must NEVER be downgraded by a precedent match"
        );
        assert!(brief.precedented.is_none());
    }

    #[test]
    fn empty_precedent_store_leaves_brief_identical_to_today() {
        // Backward-compat: with NO precedents ratified, the brief's tier and
        // risk_drivers are unaffected, and the JSON omits `precedented`
        // entirely (not `"precedented":null`).
        let touched = vec!["crates/foo/src/plain.rs".to_string()];
        let cfg = SensitiveConfig::default();
        let brief = build_review_brief(
            plain_intent(),
            "run-1/t1",
            &touched,
            &[],
            &[],
            &cfg,
            &[],
            DEFAULT_TEST_TOLERANCE,
        );
        assert_eq!(brief.risk_tier, RiskTier::Low);
        assert!(brief.precedented.is_none());
        let json = serde_json::to_string(&brief).unwrap();
        assert!(!json.contains("precedented"));
    }
}
