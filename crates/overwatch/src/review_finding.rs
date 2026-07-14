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
use serde::{Deserialize, Serialize};

/// A single confirmed AI/adversarial review finding.
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
    /// Unix timestamp when the finding was recorded.
    pub ts: i64,
}

impl ReviewFinding {
    /// Construct a review finding.
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
            ts,
        }
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
