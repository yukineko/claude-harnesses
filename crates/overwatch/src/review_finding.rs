/// AI / adversarial review findings — a persisted, overwatch-readable record of
/// a confirmed code-review finding.
///
/// Context: the `reviewgate` crate runs an independent (or self-) review over a
/// diff and *injects* findings back into the model turn as a block reason; it
/// records only a terse `{ts, verdict, mode, files, attempt}` line to its own
/// gate log — the finding TEXT is never persisted anywhere overwatch can read.
/// So there is genuinely no cross-tool findings store today.
///
/// Rather than fabricate findings, this module defines the schema and an
/// append-only stream (`review_findings.jsonl` under overwatch's storage root)
/// that a future Continuous-Audit loop (a separate backlog item) will populate.
/// The `review-queue` command reads this stream; when it is absent/empty the
/// AI-findings arm simply contributes nothing and the other two sources still
/// render (fail-soft). `overwatch record-finding` is the defined ingestion
/// point (used by that future loop, and by this crate's integration test to
/// seed a finding — that is the real write path, not a fabricated source).
use serde::{Deserialize, Deserializer, Serialize};

/// The adversarial verifier's verdict on a proposed finding.
///
/// TRI-state on purpose. The Continuous-Audit verifier used to answer a BINARY
/// question (CONFIRMED / REFUTED) with "default REFUTED", which collapses
/// *"I could not trace a permissive path"* into *"there is no permissive
/// path"* — structurally the same fail-open the audited gates are reviewed
/// for, and it demonstrably discarded a real finding (specguard
/// `forge/gather.rs`, 2026-07-21: the verifier followed only the
/// shortfall→sentinel path and missed the partial-bundle path a later fix,
/// `EXIT_INTAKE_INCOMPLETE=8`, had to close).
///
/// [`Unverified`](Self::Unverified) is the RESTRICTIVE resolution: the claim is
/// neither established nor dismissed, so the item stays pending. Only
/// [`Confirmed`](Self::Confirmed) findings take the actionable path
/// (`review-queue --to-backlog`); [`Refuted`](Self::Refuted) requires the
/// verifier to have enumerated EVERY consumption path with verbatim quotes
/// (a prompt-side burden of proof — see the `continuous-audit` SKILL.md).
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum AuditVerdict {
    /// The verifier reproduced/established the finding in code.
    Confirmed,
    /// The verifier discharged the finding by tracing ALL consumption paths
    /// and quoting each one. "I could not find a path" is NOT this verdict.
    Refuted,
    /// Undetermined: the verifier could neither establish nor discharge the
    /// claim. The default for anything unparseable — undetermined always
    /// resolves to the restrictive side.
    #[default]
    Unverified,
}

impl AuditVerdict {
    /// Parse a verdict token (case-insensitive, surrounding whitespace
    /// ignored). ANY unrecognized value — including the empty string and
    /// near-misses like `confirm` — yields [`Unverified`](Self::Unverified)
    /// rather than an error or a charitable guess: an input we cannot read is
    /// an undetermined verdict, and undetermined resolves restrictively.
    pub fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "confirmed" => Self::Confirmed,
            "refuted" => Self::Refuted,
            _ => Self::Unverified,
        }
    }

    /// The canonical snake_case label used in JSON and CLI output.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Confirmed => "confirmed",
            Self::Refuted => "refuted",
            Self::Unverified => "unverified",
        }
    }

    /// True only for [`Confirmed`](Self::Confirmed) — the single verdict that
    /// makes a finding actionable work (bridged to the backlog).
    pub fn is_actionable(&self) -> bool {
        matches!(self, Self::Confirmed)
    }

    /// The verdict assumed for a `review_findings.jsonl` row written BEFORE
    /// this field existed. Those rows could only be produced by the
    /// CONFIRMED-only ingestion contract of the day (`record-finding` was
    /// documented and used exclusively for the verifier's CONFIRMED subset —
    /// see `scripts/continuous-audit.sh`), so an ABSENT key means "confirmed",
    /// not "undetermined". This is deliberately different from an unparseable
    /// PRESENT value, which is undetermined and reads as `Unverified`.
    fn legacy_absent() -> Self {
        Self::Confirmed
    }
}

/// Deserialize a verdict permissively in SHAPE but restrictively in VALUE: any
/// JSON that is not one of the three known verdict strings deserializes as
/// [`AuditVerdict::Unverified`] instead of failing the line.
///
/// Failing would be worse than it sounds: `store::read_review_findings` SKIPS
/// corrupt lines, so a rejected verdict would silently DELETE the finding from
/// the review surface — a finding disappearing is exactly the outcome this
/// tri-state exists to prevent.
fn de_verdict<'de, D>(deserializer: D) -> Result<AuditVerdict, D::Error>
where
    D: Deserializer<'de>,
{
    let v = serde_json::Value::deserialize(deserializer)?;
    Ok(match v {
        serde_json::Value::String(s) => AuditVerdict::parse(&s),
        _ => AuditVerdict::Unverified,
    })
}

/// A single AI/adversarial review finding and the verifier's verdict on it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReviewFinding {
    /// Stable identifier for this finding (e.g. a rule id, a hash, or a
    /// caller-assigned id). Used as the key identifier in the review surface.
    pub finding_id: String,
    /// Which reviewer/tool produced it (e.g. "reviewgate", "auditmap").
    pub source: String,
    /// Severity as reported by the reviewer (free-text: high/med/low), kept as
    /// a string since different reviewers use different scales.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub severity: Option<String>,
    /// A short human summary of the finding.
    pub summary: String,
    /// The primary file the finding concerns, if known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// The verifier's rationale for confirming this finding (e.g. a file:line
    /// quoted argument for why it's real). Added after the initial schema, so
    /// `#[serde(default)]` lets pre-existing `review_findings.jsonl` rows
    /// (which have no `rationale` key) keep deserializing without error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rationale: Option<String>,
    /// The adversarial verifier's tri-state verdict. Always serialized. An
    /// ABSENT key (a row from before this field existed) reads as
    /// [`AuditVerdict::Confirmed`] — those rows came from the CONFIRMED-only
    /// ingestion contract; an unrecognized PRESENT value reads as
    /// [`AuditVerdict::Unverified`] (see [`de_verdict`]).
    #[serde(
        default = "AuditVerdict::legacy_absent",
        deserialize_with = "de_verdict"
    )]
    pub verdict: AuditVerdict,
    /// Unix timestamp when the finding was recorded.
    pub ts: i64,
}

impl ReviewFinding {
    /// Construct a review finding whose verdict is
    /// [`AuditVerdict::Confirmed`]. This constructor is the "the verifier
    /// established this" path; use [`with_verdict`](Self::with_verdict) to
    /// state any other verdict explicitly.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        finding_id: String,
        source: String,
        severity: Option<String>,
        summary: String,
        file: Option<String>,
        rationale: Option<String>,
        ts: i64,
    ) -> Self {
        Self {
            finding_id,
            source,
            severity,
            summary,
            file,
            rationale,
            verdict: AuditVerdict::Confirmed,
            ts,
        }
    }

    /// Set the verifier's verdict on this finding.
    pub fn with_verdict(mut self, verdict: AuditVerdict) -> Self {
        self.verdict = verdict;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn review_finding_round_trips() {
        let f = ReviewFinding::new(
            "F-001".to_string(),
            "reviewgate".to_string(),
            Some("high".to_string()),
            "unchecked unwrap in foo.rs".to_string(),
            Some("src/foo.rs".to_string()),
            Some("foo.rs:42 unwraps a None returned by bar()".to_string()),
            1000,
        );
        let json = serde_json::to_string(&f).unwrap();
        let back: ReviewFinding = serde_json::from_str(&json).unwrap();
        assert_eq!(f, back);
    }

    #[test]
    fn review_finding_omits_optional_none_fields() {
        let f = ReviewFinding::new(
            "F-002".to_string(),
            "auditmap".to_string(),
            None,
            "missing test coverage".to_string(),
            None,
            None,
            5,
        );
        let json = serde_json::to_string(&f).unwrap();
        assert!(!json.contains("severity"));
        assert!(!json.contains("file"));
        assert!(!json.contains("rationale"));
    }

    /// The adversarial verdict is TRI-state. A binary CONFIRMED/REFUTED
    /// collapses "could not determine" into "no problem" — the exact
    /// fail-open shape the audited gates are reviewed for.
    #[test]
    fn audit_verdict_parses_tri_state() {
        assert_eq!(AuditVerdict::parse("confirmed"), AuditVerdict::Confirmed);
        assert_eq!(AuditVerdict::parse("refuted"), AuditVerdict::Refuted);
        assert_eq!(AuditVerdict::parse("unverified"), AuditVerdict::Unverified);
        assert_eq!(AuditVerdict::parse("  CONFIRMED "), AuditVerdict::Confirmed);
        assert_eq!(AuditVerdict::parse("REFUTED"), AuditVerdict::Refuted);
    }

    /// An UNPARSEABLE verdict is undetermined, and undetermined resolves to
    /// the RESTRICTIVE side: `Unverified` (never silently `Confirmed`, never
    /// silently `Refuted`/dropped).
    #[test]
    fn unparseable_verdict_falls_back_to_unverified() {
        assert_eq!(AuditVerdict::parse("bogus"), AuditVerdict::Unverified);
        assert_eq!(AuditVerdict::parse(""), AuditVerdict::Unverified);
        assert_eq!(AuditVerdict::parse("   "), AuditVerdict::Unverified);
        // Near-misses must NOT be charitably read as a decided verdict.
        assert_eq!(AuditVerdict::parse("confirm"), AuditVerdict::Unverified);
        assert_eq!(AuditVerdict::parse("refute"), AuditVerdict::Unverified);
    }

    #[test]
    fn verdict_labels_are_stable() {
        assert_eq!(AuditVerdict::Confirmed.label(), "confirmed");
        assert_eq!(AuditVerdict::Refuted.label(), "refuted");
        assert_eq!(AuditVerdict::Unverified.label(), "unverified");
    }

    #[test]
    fn finding_carries_verdict_and_round_trips() {
        let f = ReviewFinding::new(
            "F-010".to_string(),
            "continuous-audit".to_string(),
            Some("high".to_string()),
            "unverified claim".to_string(),
            None,
            None,
            7,
        )
        .with_verdict(AuditVerdict::Unverified);
        let json = serde_json::to_string(&f).unwrap();
        assert!(
            json.contains("\"verdict\":\"unverified\""),
            "verdict must serialize snake_case: {json}"
        );
        let back: ReviewFinding = serde_json::from_str(&json).unwrap();
        assert_eq!(f, back);
        assert_eq!(back.verdict, AuditVerdict::Unverified);
    }

    /// A row whose `verdict` key holds an unrecognized value (or a non-string)
    /// must NOT fail the whole line into the corrupt-line skip path (that would
    /// silently DROP a finding). It reads back as `Unverified`.
    #[test]
    fn row_with_unknown_verdict_value_reads_as_unverified() {
        let row = r#"{"finding_id":"F-011","source":"continuous-audit","summary":"x","verdict":"probably-fine","ts":9}"#;
        let f: ReviewFinding = serde_json::from_str(row).unwrap();
        assert_eq!(f.verdict, AuditVerdict::Unverified);

        let row = r#"{"finding_id":"F-012","source":"continuous-audit","summary":"x","verdict":3,"ts":9}"#;
        let f: ReviewFinding = serde_json::from_str(row).unwrap();
        assert_eq!(f.verdict, AuditVerdict::Unverified);
    }

    /// Rows written BEFORE the field existed came from the CONFIRMED-only
    /// ingestion path, so an ABSENT key reads as `Confirmed` (documented
    /// migration rule — distinct from an unparseable value, above).
    #[test]
    fn legacy_row_without_verdict_field_reads_as_confirmed() {
        let legacy =
            r#"{"finding_id":"F-013","source":"continuous-audit","summary":"legacy","ts":42}"#;
        let f: ReviewFinding = serde_json::from_str(legacy).unwrap();
        assert_eq!(f.verdict, AuditVerdict::Confirmed);
    }

    /// Pre-`rationale` rows (no `rationale` key at all) must still deserialize
    /// — the whole point of `#[serde(default)]` on the new field.
    #[test]
    fn review_finding_reads_legacy_row_without_rationale_field() {
        let legacy =
            r#"{"finding_id":"F-003","source":"reviewgate","summary":"legacy row","ts":42}"#;
        let f: ReviewFinding = serde_json::from_str(legacy).unwrap();
        assert_eq!(f.finding_id, "F-003");
        assert_eq!(f.rationale, None);
    }
}
