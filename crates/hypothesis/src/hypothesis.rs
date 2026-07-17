use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

/// FNV-1a 64-bit hash (the shared `harness_core::hash` implementation),
/// returning the low 32 bits as an 8-digit lowercase hex string.
pub fn new_id(text: &str) -> String {
    let fnv = harness_core::hash::fnv1a64(text.as_bytes());
    // Take lower 32 bits → 8 hex digits
    format!("{:08x}", fnv as u32)
}

/// Returns current UTC time as an ISO 8601 string (e.g. "2026-06-26T13:00:00Z").
fn now_iso8601() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    // Manual conversion from Unix timestamp to date/time components.
    let s = secs % 60;
    let m = (secs / 60) % 60;
    let h = (secs / 3600) % 24;
    let days = secs / 86400; // days since 1970-01-01

    // Compute year, month, day from days count.
    let mut year = 1970u32;
    let mut remaining_days = days;
    loop {
        let days_in_year = if is_leap_year(year) { 366 } else { 365 };
        if remaining_days < days_in_year {
            break;
        }
        remaining_days -= days_in_year;
        year += 1;
    }
    let leap = is_leap_year(year);
    let days_in_month = [
        31u64,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut month = 1u32;
    for dim in &days_in_month {
        if remaining_days < *dim {
            break;
        }
        remaining_days -= *dim;
        month += 1;
    }
    let day = remaining_days + 1;

    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year, month, day, h, m, s
    )
}

fn is_leap_year(year: u32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

/// Parses the leading `YYYY-MM-DD` of an ISO8601 string into days-since-epoch,
/// mirroring `now_iso8601`'s forward (days -> date) conversion in reverse.
/// The time/zone portion is ignored (day granularity is intended for the
/// measurement-delay metric). Returns `None` on malformed input so callers can
/// fail soft (skip the item rather than panic or misreport).
pub(crate) fn iso_date_to_epoch_days(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 10 || b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    let year: u32 = s.get(0..4)?.parse().ok()?;
    let month: u32 = s.get(5..7)?.parse().ok()?;
    let day: u32 = s.get(8..10)?.parse().ok()?;
    if year < 1970 || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }

    let mut days: i64 = 0;
    let mut y = 1970u32;
    while y < year {
        days += if is_leap_year(y) { 366 } else { 365 };
        y += 1;
    }
    let leap = is_leap_year(year);
    let days_in_month = [
        31i64,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    for dim in days_in_month.iter().take((month - 1) as usize) {
        days += *dim;
    }
    days += (day - 1) as i64;
    Some(days)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Open,
    /// A linked deliverable has shipped but the hypothesis has not yet been
    /// measured. Distinct from `Open` (not started) and `Validated`/`Rejected`
    /// (measured): "shipped but not yet measured" — build is not validation.
    AwaitingMeasurement,
    Validated,
    Rejected,
}

impl Status {
    pub fn is_open(&self) -> bool {
        matches!(self, Status::Open)
    }

    pub fn is_awaiting_measurement(&self) -> bool {
        matches!(self, Status::AwaitingMeasurement)
    }

    #[allow(dead_code)]
    pub fn is_validated(&self) -> bool {
        matches!(self, Status::Validated)
    }

    #[allow(dead_code)]
    pub fn is_rejected(&self) -> bool {
        matches!(self, Status::Rejected)
    }
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Status::Open => write!(f, "open"),
            Status::AwaitingMeasurement => write!(f, "awaiting-measurement"),
            Status::Validated => write!(f, "validated"),
            Status::Rejected => write!(f, "rejected"),
        }
    }
}

/// A comparison operator for a pre-registered [`Criterion`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Comparator {
    /// `>=`
    Ge,
    /// `<=`
    Le,
    /// `>`
    Gt,
    /// `<`
    Lt,
    /// `==`
    Eq,
}

impl Comparator {
    fn as_str(&self) -> &'static str {
        match self {
            Comparator::Ge => ">=",
            Comparator::Le => "<=",
            Comparator::Gt => ">",
            Comparator::Lt => "<",
            Comparator::Eq => "==",
        }
    }

    fn satisfied(&self, measured: f64, threshold: f64) -> bool {
        match self {
            Comparator::Ge => measured >= threshold,
            Comparator::Le => measured <= threshold,
            Comparator::Gt => measured > threshold,
            Comparator::Lt => measured < threshold,
            Comparator::Eq => (measured - threshold).abs() < f64::EPSILON,
        }
    }
}

/// A pre-registered, falsifiable bar on a named metric, fixed at `add` time.
///
/// Recording the threshold *before* the experiment ships is what stops post-hoc
/// goalpost-shifting: `validate` checks a measurement against this registered
/// bar instead of accepting any non-empty evidence after the fact.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Criterion {
    pub metric: String,
    pub comparator: Comparator,
    pub threshold: f64,
}

impl Criterion {
    /// Parse a criterion string like `"conversion >= 0.12"`.
    ///
    /// The operator may be one of `>=`, `<=`, `>`, `<`, `==` and may be
    /// surrounded by spaces. The metric is everything before the operator; the
    /// threshold is a number after it.
    pub fn parse(s: &str) -> Result<Self> {
        // Two-char operators first so the `>` inside `>=` isn't matched early.
        for (op, cmp) in [
            (">=", Comparator::Ge),
            ("<=", Comparator::Le),
            ("==", Comparator::Eq),
            (">", Comparator::Gt),
            ("<", Comparator::Lt),
        ] {
            if let Some(idx) = s.find(op) {
                let metric = s[..idx].trim();
                let value = s[idx + op.len()..].trim();
                if metric.is_empty() {
                    anyhow::bail!("criterion is missing a metric name: {s:?}");
                }
                let threshold: f64 = value.parse().map_err(|_| {
                    anyhow::anyhow!("criterion threshold is not a number: {value:?} (in {s:?})")
                })?;
                return Ok(Criterion {
                    metric: metric.to_string(),
                    comparator: cmp,
                    threshold,
                });
            }
        }
        anyhow::bail!("criterion must contain a comparator (>=, <=, >, <, ==): {s:?}")
    }

    /// Does `measured` clear this criterion?
    pub fn satisfied_by(&self, measured: f64) -> bool {
        self.comparator.satisfied(measured, self.threshold)
    }
}

impl fmt::Display for Criterion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} {} {}",
            self.metric,
            self.comparator.as_str(),
            self.threshold
        )
    }
}

/// How damaging it is if an assumption turns out false.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Risk {
    Low,
    Medium,
    High,
}

impl Risk {
    pub fn parse(s: &str) -> Result<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "low" => Ok(Risk::Low),
            "medium" | "med" => Ok(Risk::Medium),
            "high" => Ok(Risk::High),
            other => anyhow::bail!("unknown risk {other:?} (expected low|medium|high)"),
        }
    }
    fn weight(&self) -> u8 {
        match self {
            Risk::Low => 0,
            Risk::Medium => 1,
            Risk::High => 2,
        }
    }
    fn as_str(&self) -> &'static str {
        match self {
            Risk::Low => "low",
            Risk::Medium => "medium",
            Risk::High => "high",
        }
    }
}

/// How much we currently know about an assumption (evidence strength).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Evidence {
    Strong,
    Weak,
    None,
}

impl Evidence {
    pub fn parse(s: &str) -> Result<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "strong" => Ok(Evidence::Strong),
            "weak" => Ok(Evidence::Weak),
            "none" => Ok(Evidence::None),
            other => anyhow::bail!("unknown evidence {other:?} (expected strong|weak|none)"),
        }
    }
    fn weakness(&self) -> u8 {
        match self {
            Evidence::Strong => 0,
            Evidence::Weak => 1,
            Evidence::None => 2,
        }
    }
    fn as_str(&self) -> &'static str {
        match self {
            Evidence::Strong => "strong",
            Evidence::Weak => "weak",
            Evidence::None => "none",
        }
    }
}

/// A belief a hypothesis rests on. The riskiest untested assumption with weak
/// evidence is the "leap of faith" — a RAT (riskiest-assumption test) de-risks
/// it with a minimal experiment *before* committing to a full build.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Assumption {
    pub text: String,
    pub risk: Risk,
    pub evidence: Evidence,
    #[serde(default)]
    pub tested: bool,
}

impl Assumption {
    /// Leap-of-faith score: high risk + weak evidence ranks highest (test first).
    pub fn leap_score(&self) -> u8 {
        self.risk.weight() + self.evidence.weakness()
    }

    /// A leap of faith worth a RAT: untested, high-stakes, and not yet
    /// well-evidenced. These are what flow should de-risk before a full build.
    pub fn is_leap_of_faith(&self) -> bool {
        !self.tested && self.risk == Risk::High && self.evidence != Evidence::Strong
    }
}

impl fmt::Display for Assumption {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "[risk:{} evidence:{}{}] {}",
            self.risk.as_str(),
            self.evidence.as_str(),
            if self.tested { " tested" } else { "" },
            self.text
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hypothesis {
    pub id: String,
    pub text: String,
    pub status: Status,
    #[serde(default)]
    pub evidence: Vec<String>,
    #[serde(default)]
    pub linked_goal: Option<String>,
    #[serde(default)]
    pub condukt_run: Option<String>,
    /// Pre-registered success bar: validation must clear this measured value.
    #[serde(default)]
    pub success_criterion: Option<Criterion>,
    /// Pre-registered kill bar: hitting it means the bet is disproven (reject).
    #[serde(default)]
    pub kill_criterion: Option<Criterion>,
    /// Beliefs the bet rests on; the riskiest untested one is the RAT target.
    #[serde(default)]
    pub assumptions: Vec<Assumption>,
    /// Discovery-ordering score (higher = validate sooner). Symmetric to an
    /// opportunity's execution weight: confidence drives the order in which open
    /// hypotheses surface for validation, turning the open list from insertion
    /// order into a deterministic score order. Absent in older stores;
    /// `#[serde(default)]` loads those at [`default_confidence`] (a neutral 0.5),
    /// which keeps legacy hypotheses ordering by created_at among themselves.
    #[serde(default = "default_confidence")]
    pub confidence: f64,
    pub created_at: String,
    pub updated_at: String,
    /// When this hypothesis's linked deliverable shipped (entered
    /// `awaiting-measurement`), as an ISO8601 timestamp. `None` for hypotheses
    /// that have never shipped, and for legacy stores written before this field
    /// existed (`#[serde(default)]` loads those as `None`, not a fabricated
    /// timestamp — build ≠ validate; we don't guess when the past shipped).
    #[serde(default)]
    pub shipped_at: Option<String>,
}

/// Neutral default confidence for hypotheses created or loaded without one.
/// 0.5 sits in the middle of the [0,1] band so an unscored bet neither jumps
/// the queue nor sinks below every scored one.
pub fn default_confidence() -> f64 {
    0.5
}

impl Hypothesis {
    /// The riskiest untested leap-of-faith assumption to de-risk with a RAT
    /// before a full build, if any. Ties break toward the higher leap score.
    pub fn riskiest_assumption(&self) -> Option<&Assumption> {
        self.assumptions
            .iter()
            .filter(|a| a.is_leap_of_faith())
            .max_by_key(|a| a.leap_score())
    }
}

impl Hypothesis {
    pub fn new(text: impl Into<String>, linked_goal: Option<String>) -> Self {
        let text = text.into();
        let id = new_id(&text);
        let now = now_iso8601();
        Self {
            id,
            text,
            status: Status::Open,
            evidence: vec![],
            linked_goal,
            condukt_run: None,
            success_criterion: None,
            kill_criterion: None,
            assumptions: vec![],
            confidence: default_confidence(),
            created_at: now.clone(),
            updated_at: now,
            shipped_at: None,
        }
    }
}

/// Shipped-vs-measured PDO health metrics (`hypothesis stats`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Stats {
    /// Hypotheses that have ever shipped a linked deliverable (`shipped_at`
    /// is set), regardless of current status.
    pub shipped: usize,
    pub validated: usize,
    pub rejected: usize,
    /// Currently outstanding: shipped but not yet measured.
    pub awaiting: usize,
    /// Mean days between `shipped_at` and `updated_at` for hypotheses that
    /// have been measured (validated or rejected) and have a `shipped_at`.
    /// `None` when no measured hypothesis has both timestamps available
    /// (nothing to average — not the same as a delay of zero).
    pub avg_measurement_delay_days: Option<f64>,
}

/// Compute shipped-vs-measured health metrics over a set of hypotheses.
/// Pure and deterministic: no I/O, no clock reads — every input is a
/// [`Hypothesis`] already loaded from the store, so callers get identical
/// output for identical input (fixed-number regression tests hold).
pub fn compute_stats(hypotheses: &[Hypothesis]) -> Stats {
    let shipped = hypotheses.iter().filter(|h| h.shipped_at.is_some()).count();
    let validated = hypotheses
        .iter()
        .filter(|h| h.status == Status::Validated)
        .count();
    let rejected = hypotheses
        .iter()
        .filter(|h| h.status == Status::Rejected)
        .count();
    let awaiting = hypotheses
        .iter()
        .filter(|h| h.status == Status::AwaitingMeasurement)
        .count();

    let delays: Vec<f64> = hypotheses
        .iter()
        .filter(|h| matches!(h.status, Status::Validated | Status::Rejected))
        .filter_map(|h| {
            let shipped_at = h.shipped_at.as_deref()?;
            let shipped_days = iso_date_to_epoch_days(shipped_at)?;
            let measured_days = iso_date_to_epoch_days(&h.updated_at)?;
            Some((measured_days - shipped_days) as f64)
        })
        .collect();

    let avg_measurement_delay_days = if delays.is_empty() {
        None
    } else {
        Some(delays.iter().sum::<f64>() / delays.len() as f64)
    };

    Stats {
        shipped,
        validated,
        rejected,
        awaiting,
        avg_measurement_delay_days,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_id_is_deterministic() {
        let id1 = new_id("test hypothesis text");
        let id2 = new_id("test hypothesis text");
        assert_eq!(id1, id2);
    }

    #[test]
    fn new_id_differs_for_different_input() {
        let id1 = new_id("hypothesis A");
        let id2 = new_id("hypothesis B");
        assert_ne!(id1, id2);
    }

    #[test]
    fn new_id_is_8_hex_chars() {
        let id = new_id("some text");
        assert_eq!(id.len(), 8);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn status_predicates() {
        assert!(Status::Open.is_open());
        assert!(!Status::Open.is_validated());
        assert!(!Status::Open.is_rejected());

        assert!(!Status::Validated.is_open());
        assert!(Status::Validated.is_validated());
        assert!(!Status::Validated.is_rejected());

        assert!(!Status::Rejected.is_open());
        assert!(!Status::Rejected.is_validated());
        assert!(Status::Rejected.is_rejected());

        assert!(Status::AwaitingMeasurement.is_awaiting_measurement());
        assert!(!Status::AwaitingMeasurement.is_open());
        assert!(!Status::AwaitingMeasurement.is_validated());
        assert!(!Status::AwaitingMeasurement.is_rejected());
        assert!(!Status::Open.is_awaiting_measurement());
    }

    #[test]
    fn status_display() {
        assert_eq!(Status::Open.to_string(), "open");
        assert_eq!(
            Status::AwaitingMeasurement.to_string(),
            "awaiting-measurement"
        );
        assert_eq!(Status::Validated.to_string(), "validated");
        assert_eq!(Status::Rejected.to_string(), "rejected");
    }

    #[test]
    fn iso_date_to_epoch_days_known_dates() {
        assert_eq!(iso_date_to_epoch_days("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(iso_date_to_epoch_days("1970-01-02T00:00:00Z"), Some(1));
        // 2026 is not a leap year; sanity-check a date well past several
        // leap-year boundaries (2020, 2024) rather than asserting a magic
        // number blindly copied from elsewhere.
        assert_eq!(
            iso_date_to_epoch_days("2026-01-01T00:00:00Z"),
            iso_date_to_epoch_days("2025-01-01T00:00:00Z").map(|d| d + 365)
        );
    }

    #[test]
    fn iso_date_to_epoch_days_rejects_malformed() {
        assert_eq!(iso_date_to_epoch_days("garbage"), None);
        assert_eq!(iso_date_to_epoch_days("1969-01-01T00:00:00Z"), None);
        assert_eq!(iso_date_to_epoch_days("2026-13-01T00:00:00Z"), None);
    }

    fn hypothesis_with(status: Status, shipped_at: Option<&str>, updated_at: &str) -> Hypothesis {
        let mut h = Hypothesis::new("stats fixture", None);
        h.status = status;
        h.shipped_at = shipped_at.map(str::to_string);
        h.updated_at = updated_at.to_string();
        h
    }

    #[test]
    fn compute_stats_counts_and_avg_delay_are_fixed() {
        let hypotheses = vec![
            // Open, never shipped: contributes to none of the counts.
            hypothesis_with(Status::Open, None, "2026-01-01T00:00:00Z"),
            // Awaiting measurement: shipped, outstanding, not yet measured.
            hypothesis_with(
                Status::AwaitingMeasurement,
                Some("2026-01-01T00:00:00Z"),
                "2026-01-01T00:00:00Z",
            ),
            // Validated 10 days after shipping.
            hypothesis_with(
                Status::Validated,
                Some("2026-01-01T00:00:00Z"),
                "2026-01-11T00:00:00Z",
            ),
            // Rejected 4 days after shipping.
            hypothesis_with(
                Status::Rejected,
                Some("2026-02-01T00:00:00Z"),
                "2026-02-05T00:00:00Z",
            ),
        ];

        let stats = compute_stats(&hypotheses);
        assert_eq!(stats.shipped, 3, "3 of 4 have a shipped_at");
        assert_eq!(stats.validated, 1);
        assert_eq!(stats.rejected, 1);
        assert_eq!(stats.awaiting, 1);
        // (10 + 4) / 2 = 7.0 exactly — a fixed, deterministic number.
        assert_eq!(stats.avg_measurement_delay_days, Some(7.0));
    }

    #[test]
    fn compute_stats_avg_delay_is_none_when_nothing_measured() {
        let hypotheses = vec![
            hypothesis_with(Status::Open, None, "2026-01-01T00:00:00Z"),
            hypothesis_with(
                Status::AwaitingMeasurement,
                Some("2026-01-01T00:00:00Z"),
                "2026-01-01T00:00:00Z",
            ),
        ];
        let stats = compute_stats(&hypotheses);
        assert_eq!(stats.validated, 0);
        assert_eq!(stats.rejected, 0);
        assert_eq!(
            stats.avg_measurement_delay_days, None,
            "no validated/rejected hypothesis exists, so there is nothing to average"
        );
    }

    #[test]
    fn compute_stats_skips_measured_hypothesis_missing_shipped_at() {
        // A hypothesis validated directly from `open` (bypassing
        // awaiting-measurement, as `test_awaiting_measurement_can_still_validate`
        // in store.rs exercises) has no shipped_at, so it can't contribute a
        // delay — but it must still count toward `validated`.
        let hypotheses = vec![hypothesis_with(
            Status::Validated,
            None,
            "2026-01-11T00:00:00Z",
        )];
        let stats = compute_stats(&hypotheses);
        assert_eq!(stats.shipped, 0);
        assert_eq!(stats.validated, 1);
        assert_eq!(stats.avg_measurement_delay_days, None);
    }

    #[test]
    fn hypothesis_new_sets_open_status() {
        let h = Hypothesis::new("our first hypothesis", None);
        assert!(h.status.is_open());
        assert_eq!(h.text, "our first hypothesis");
        assert!(h.evidence.is_empty());
        assert!(h.linked_goal.is_none());
        assert!(h.shipped_at.is_none());
        assert_eq!(h.id.len(), 8);
    }

    #[test]
    fn hypothesis_serialize_deserialize_roundtrip() {
        let h = Hypothesis::new("roundtrip test", Some("goal-abc".to_string()));
        let json = serde_json::to_string(&h).expect("serialize");
        let h2: Hypothesis = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(h.id, h2.id);
        assert_eq!(h.text, h2.text);
        assert_eq!(h.status, h2.status);
        assert_eq!(h.linked_goal, h2.linked_goal);
    }

    #[test]
    fn hypothesis_deserialize_legacy_without_evidence_and_goal() {
        // Old records without evidence/linked_goal fields should deserialize fine
        let json = r#"{
            "id": "abcd1234",
            "text": "legacy hypothesis",
            "status": "open",
            "created_at": "2026-06-26T13:00:00Z",
            "updated_at": "2026-06-26T13:00:00Z"
        }"#;
        let h: Hypothesis = serde_json::from_str(json).expect("deserialize legacy");
        assert!(h.evidence.is_empty());
        assert!(h.linked_goal.is_none());
        assert!(h.status.is_open());
        // confidence is also absent in this legacy record → defaults to 0.5,
        // so legacy hypotheses keep ordering by created_at among themselves.
        assert_eq!(h.confidence, default_confidence());
    }

    #[test]
    fn new_sets_default_confidence() {
        let h = Hypothesis::new("a fresh bet", None);
        assert_eq!(h.confidence, 0.5);
    }

    #[test]
    fn status_serde_snake_case() {
        let s = serde_json::to_string(&Status::Validated).expect("serialize");
        assert_eq!(s, r#""validated""#);
        let s2: Status = serde_json::from_str(&s).expect("deserialize");
        assert_eq!(s2, Status::Validated);
    }

    #[test]
    fn status_serde_awaiting_measurement_snake_case() {
        let s = serde_json::to_string(&Status::AwaitingMeasurement).expect("serialize");
        assert_eq!(s, r#""awaiting_measurement""#);
        let s2: Status = serde_json::from_str(&s).expect("deserialize");
        assert_eq!(s2, Status::AwaitingMeasurement);
    }

    #[test]
    fn criterion_parse_all_operators() {
        let cases = [
            ("conversion >= 0.12", Comparator::Ge, "conversion", 0.12),
            ("bounce <= 0.3", Comparator::Le, "bounce", 0.3),
            ("signups > 100", Comparator::Gt, "signups", 100.0),
            ("latency_ms < 250", Comparator::Lt, "latency_ms", 250.0),
            ("exact == 1", Comparator::Eq, "exact", 1.0),
        ];
        for (s, cmp, metric, threshold) in cases {
            let c = Criterion::parse(s).unwrap_or_else(|e| panic!("parse {s:?}: {e}"));
            assert_eq!(c.comparator, cmp, "operator for {s:?}");
            assert_eq!(c.metric, metric, "metric for {s:?}");
            assert_eq!(c.threshold, threshold, "threshold for {s:?}");
        }
    }

    #[test]
    fn criterion_parse_tight_and_loose_spacing() {
        let tight = Criterion::parse("conversion>=0.12").unwrap();
        let loose = Criterion::parse("  conversion  >=  0.12 ").unwrap();
        assert_eq!(tight, loose);
        assert_eq!(tight.metric, "conversion");
    }

    #[test]
    fn criterion_parse_rejects_garbage() {
        assert!(Criterion::parse("no operator here").is_err());
        assert!(Criterion::parse(">= 5").is_err()); // missing metric
        assert!(Criterion::parse("metric >= notanumber").is_err());
    }

    #[test]
    fn criterion_satisfied_by() {
        let ge = Criterion::parse("m >= 10").unwrap();
        assert!(ge.satisfied_by(10.0));
        assert!(ge.satisfied_by(11.0));
        assert!(!ge.satisfied_by(9.99));

        let le = Criterion::parse("m <= 0.05").unwrap();
        assert!(le.satisfied_by(0.05));
        assert!(le.satisfied_by(0.0));
        assert!(!le.satisfied_by(0.06));
    }

    #[test]
    fn criterion_display_roundtrips_via_parse() {
        let c = Criterion::parse("conversion >= 0.12").unwrap();
        let shown = c.to_string();
        assert_eq!(shown, "conversion >= 0.12");
        assert_eq!(Criterion::parse(&shown).unwrap(), c);
    }

    fn assumption(text: &str, risk: Risk, evidence: Evidence) -> Assumption {
        Assumption {
            text: text.to_string(),
            risk,
            evidence,
            tested: false,
        }
    }

    #[test]
    fn risk_and_evidence_parse() {
        assert_eq!(Risk::parse("High").unwrap(), Risk::High);
        assert_eq!(Risk::parse(" med ").unwrap(), Risk::Medium);
        assert_eq!(Risk::parse("low").unwrap(), Risk::Low);
        assert!(Risk::parse("huge").is_err());
        assert_eq!(Evidence::parse("STRONG").unwrap(), Evidence::Strong);
        assert_eq!(Evidence::parse("none").unwrap(), Evidence::None);
        assert!(Evidence::parse("medium").is_err());
    }

    #[test]
    fn leap_of_faith_requires_high_risk_and_not_strong() {
        assert!(assumption("a", Risk::High, Evidence::None).is_leap_of_faith());
        assert!(assumption("a", Risk::High, Evidence::Weak).is_leap_of_faith());
        // High risk but strong evidence → already de-risked, not a leap.
        assert!(!assumption("a", Risk::High, Evidence::Strong).is_leap_of_faith());
        // Low/medium risk → not a leap of faith regardless of evidence.
        assert!(!assumption("a", Risk::Medium, Evidence::None).is_leap_of_faith());
        // Tested → no longer a leap.
        let mut t = assumption("a", Risk::High, Evidence::None);
        t.tested = true;
        assert!(!t.is_leap_of_faith());
    }

    #[test]
    fn riskiest_assumption_picks_highest_leap_score() {
        let mut h = Hypothesis::new("a bet", None);
        h.assumptions = vec![
            assumption("weak-evidence high risk", Risk::High, Evidence::Weak), // score 3
            assumption("no-evidence high risk", Risk::High, Evidence::None),   // score 4 ← RAT
            assumption("low risk", Risk::Low, Evidence::None),                 // not a leap
        ];
        let rat = h.riskiest_assumption().expect("a leap of faith exists");
        assert_eq!(rat.text, "no-evidence high risk");
    }

    #[test]
    fn riskiest_assumption_none_when_all_derisked() {
        let mut h = Hypothesis::new("a bet", None);
        h.assumptions = vec![
            assumption("well evidenced", Risk::High, Evidence::Strong),
            assumption("low stakes", Risk::Low, Evidence::None),
        ];
        assert!(h.riskiest_assumption().is_none());
    }

    #[test]
    fn hypothesis_deserialize_legacy_without_criteria() {
        // Records written before success/kill criteria existed must still load.
        let json = r#"{
            "id": "abcd1234",
            "text": "legacy hypothesis",
            "status": "open",
            "created_at": "2026-06-26T13:00:00Z",
            "updated_at": "2026-06-26T13:00:00Z"
        }"#;
        let h: Hypothesis = serde_json::from_str(json).expect("deserialize legacy");
        assert!(h.success_criterion.is_none());
        assert!(h.kill_criterion.is_none());
        assert!(h.assumptions.is_empty());
    }
}
