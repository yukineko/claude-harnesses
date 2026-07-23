/// Fleet-level correlated-error detection.
///
/// Gate violations (blastguard denial, propguard PROP failure, specguard drift
/// finding, mutategate kill failure) are recorded here with a normalized
/// **signature**: a stable string identifying *which* invariant/property/
/// pattern was violated, independent of the task/session that hit it. When
/// the same signature recurs `threshold` or more times within a `window_secs`
/// window across *multiple distinct* tasks/sessions, it is escalated from a
/// one-off failure to a **systemic issue**.
///
/// Everything here is a pure, deterministic function over recorded events:
/// no wall-clock reads, no randomness. Callers supply `now`/`ts` explicitly
/// (mirroring `store::now()` being threaded in rather than read internally),
/// which keeps unit tests reproducible and keeps liveness/recurrence judged
/// the same way overwatch already judges lease liveness: by an injected
/// timestamp compared against a TTL/window, never by a stored pid or
/// implicit "now".
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The gate/tool that produced a violation. Each source has its own shape of
/// "what was violated", which `normalize_signature` folds into one stable
/// string space.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum ViolationSource {
    /// blastguard denied a destructive/blast-radius command.
    Blastguard,
    /// propguard PROP-* property failed.
    Propguard,
    /// specguard detected a spec/impl/test drift finding.
    Specguard,
    /// mutategate: a mutant survived (kill failure).
    Mutategate,
    /// donegate: a build/test/lint blocking check failed.
    Donegate,
    /// reviewgate: a diff review (inject or subprocess) returned a blocking verdict.
    Reviewgate,
    /// tdd: the RED→GREEN test-first sequence was not observed before a Stop.
    Tdd,
    /// budgetguard: session/task cost exceeded the configured cap.
    Budgetguard,
    /// autoflow: the deterministic circuit breaker tripped (failure-streak/stall/budget).
    Autoflow,
    /// ctxrot: context-budget usage crossed a blocking threshold.
    Ctxrot,
}

impl ViolationSource {
    /// Stable lowercase token used inside signatures.
    fn token(self) -> &'static str {
        match self {
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

    /// Parse a source token back from its string form (used when reading
    /// persisted events). Unknown tokens are rejected rather than silently
    /// mapped, so bad data surfaces instead of corrupting aggregation.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "blastguard" => Some(ViolationSource::Blastguard),
            "propguard" => Some(ViolationSource::Propguard),
            "specguard" => Some(ViolationSource::Specguard),
            "mutategate" => Some(ViolationSource::Mutategate),
            "donegate" => Some(ViolationSource::Donegate),
            "reviewgate" => Some(ViolationSource::Reviewgate),
            "tdd" => Some(ViolationSource::Tdd),
            "budgetguard" => Some(ViolationSource::Budgetguard),
            "autoflow" => Some(ViolationSource::Autoflow),
            "ctxrot" => Some(ViolationSource::Ctxrot),
            _ => None,
        }
    }
}

/// A single recorded gate-violation event, as reported by a task/session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ViolationEvent {
    /// Which gate/tool raised this violation.
    pub source: ViolationSource,
    /// The normalized signature (see [`normalize_signature`]).
    pub signature: String,
    /// The task/content key this violation occurred against.
    pub task_key: String,
    /// Session that observed/recorded the violation.
    pub session_id: String,
    /// Unix timestamp when the violation was recorded.
    pub ts: i64,
    /// Optional free-text detail (e.g. the raw denial reason). Never used
    /// for signature matching — only for human-readable audit trails.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Raw identifying fields used to derive a signature, before normalization.
/// Each gate source fills in whichever of these fields it has; unused
/// fields are `None`. This keeps `normalize_signature` a single pure
/// function all callers funnel through, instead of one ad hoc formatter per
/// source living at each call site.
#[derive(Debug, Clone, Default)]
pub struct RawViolation<'a> {
    /// blastguard: the command classification / denial rule id (e.g. "rm-rf").
    pub rule_id: Option<&'a str>,
    /// propguard: the PROP-* identifier (e.g. "PROP-003").
    pub property_id: Option<&'a str>,
    /// specguard: the drift kind (e.g. "spec-without-impl") plus the spec/impl
    /// symbol path that drifted.
    pub drift_kind: Option<&'a str>,
    pub symbol: Option<&'a str>,
    /// mutategate: the mutation operator that survived (e.g. "arithmetic-op-swap").
    pub mutation_operator: Option<&'a str>,
    /// donegate / reviewgate / tdd / budgetguard / autoflow / ctxrot: the
    /// stable check/rule identifier that blocked (e.g. donegate's failing
    /// command name, tdd's missing-RED reason, ctxrot's threshold name).
    /// Shared across these six sources rather than one field per source,
    /// since — unlike blastguard's structured rule_id or specguard's
    /// drift_kind+symbol pair — each of these gates already reduces its
    /// blocking reason to a single short discriminator before reporting it.
    pub check_kind: Option<&'a str>,
}

/// Normalize a raw violation into a stable signature string: same kind of
/// failure across different tasks/sessions must produce the SAME signature,
/// regardless of task-specific details (file paths, line numbers, task ids).
///
/// Signature shape: `<source>:<discriminator>[:<discriminator2>]`, all
/// lowercased and whitespace-trimmed so cosmetic differences (case, stray
/// spaces) don't fragment what is really the same recurring failure.
///
/// **Empty / whitespace-only / missing discriminator is rejected** (`None`),
/// rather than folded into a bare `<source>:unknown` bucket. Correlated-error
/// detection escalates a signature to "systemic" once it recurs across
/// distinct tasks; a catch-all `unknown` bucket would merge *unrelated*
/// failures that merely happen to lack a discriminator into one signature and
/// falsely flag them as a systemic pattern. Since a missing discriminator by
/// definition carries no information to distinguish one failure from another,
/// the only safe options are (a) drop it or (b) invent a colliding sentinel;
/// we drop it. Callers that receive `None` must not persist the event (see
/// [`build_event`] / `store::append_violation`, which fail-soft skip it).
pub fn normalize_signature(source: ViolationSource, raw: &RawViolation) -> Option<String> {
    // Normalize a field, treating empty/whitespace-only as absent (`None`)
    // so a present-but-blank discriminator is handled the same as a missing
    // one instead of producing a bare `<source>:` bucket.
    let norm = |s: &str| {
        let n = s.trim().to_lowercase().replace(' ', "-");
        if n.is_empty() {
            None
        } else {
            Some(n)
        }
    };

    let discriminator = match source {
        ViolationSource::Blastguard => raw.rule_id.and_then(norm)?,
        ViolationSource::Propguard => raw.property_id.and_then(norm)?,
        ViolationSource::Specguard => {
            // The drift kind is the required discriminator; the symbol is an
            // optional refinement. A missing/blank kind is not bucketable.
            let kind = raw.drift_kind.and_then(norm)?;
            match raw.symbol.and_then(norm) {
                Some(sym) => format!("{kind}:{sym}"),
                None => kind,
            }
        }
        ViolationSource::Mutategate => raw.mutation_operator.and_then(norm)?,
        ViolationSource::Donegate
        | ViolationSource::Reviewgate
        | ViolationSource::Tdd
        | ViolationSource::Budgetguard
        | ViolationSource::Autoflow
        | ViolationSource::Ctxrot => raw.check_kind.and_then(norm)?,
    };

    Some(format!("{}:{}", source.token(), discriminator))
}

/// Build a [`ViolationEvent`] from raw fields, normalizing the signature.
///
/// Returns `None` when the raw violation has no usable discriminator (empty /
/// whitespace-only / missing): such an event cannot be correlated with
/// anything and must not be recorded, so callers fail-soft skip it rather than
/// pollute the ledger with an `unknown` bucket that merges unrelated failures.
#[allow(clippy::too_many_arguments)]
pub fn build_event(
    source: ViolationSource,
    raw: &RawViolation,
    task_key: String,
    session_id: String,
    ts: i64,
    detail: Option<String>,
) -> Option<ViolationEvent> {
    Some(ViolationEvent {
        source,
        signature: normalize_signature(source, raw)?,
        task_key,
        session_id,
        ts,
        detail,
    })
}

/// Recurrence detection configuration. Both fields are intentionally
/// configurable per the design: what counts as "systemic" varies by project.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct RecurrencePolicy {
    /// Minimum number of occurrences (across distinct tasks) required to
    /// escalate a signature as systemic.
    pub threshold: usize,
    /// Sliding window size in seconds: only events with
    /// `now - ts <= window_secs` are considered "recent" for recurrence.
    pub window_secs: i64,
}

impl Default for RecurrencePolicy {
    fn default() -> Self {
        // Defaults: 3+ occurrences within a 24h window escalate to systemic.
        Self {
            threshold: 3,
            window_secs: 86_400,
        }
    }
}

/// Aggregated recurrence info for one signature.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SignatureRecurrence {
    /// The normalized signature.
    pub signature: String,
    /// Source gate that produced this signature.
    pub source: ViolationSource,
    /// Total occurrences within the window.
    pub occurrences: usize,
    /// Distinct task keys that hit this signature within the window.
    pub distinct_tasks: usize,
    /// Distinct session ids that hit this signature within the window.
    pub distinct_sessions: usize,
    /// Timestamp of the earliest occurrence within the window.
    pub first_seen: i64,
    /// Timestamp of the most recent occurrence within the window.
    pub last_seen: i64,
    /// Whether this signature meets the recurrence policy AND spans more
    /// than one distinct task/session (a single task retried many times is
    /// NOT systemic — it must be a cross-task/session pattern).
    pub is_systemic: bool,
}

/// Deterministically aggregate violation events into per-signature
/// recurrence stats, restricted to the window `[now - window_secs, now]`,
/// and flag which signatures cross the recurrence threshold as systemic.
///
/// A signature is escalated to systemic only when BOTH:
/// 1. occurrences >= `policy.threshold`, AND
/// 2. it spans more than one distinct task OR more than one distinct
///    session (a single task/session hammering the same gate repeatedly is
///    a local retry loop, not a fleet-wide correlated error).
pub fn detect_recurrence(
    events: &[ViolationEvent],
    now: i64,
    policy: RecurrencePolicy,
) -> Vec<SignatureRecurrence> {
    #[derive(Default)]
    struct Acc {
        source: Option<ViolationSource>,
        occurrences: usize,
        tasks: std::collections::BTreeSet<String>,
        sessions: std::collections::BTreeSet<String>,
        first_seen: i64,
        last_seen: i64,
    }

    let mut by_sig: BTreeMap<String, Acc> = BTreeMap::new();

    for ev in events {
        if now - ev.ts > policy.window_secs || ev.ts > now {
            continue;
        }
        let acc = by_sig.entry(ev.signature.clone()).or_default();
        if acc.occurrences == 0 {
            acc.first_seen = ev.ts;
            acc.last_seen = ev.ts;
        } else {
            acc.first_seen = acc.first_seen.min(ev.ts);
            acc.last_seen = acc.last_seen.max(ev.ts);
        }
        acc.source = Some(ev.source);
        acc.occurrences += 1;
        acc.tasks.insert(ev.task_key.clone());
        acc.sessions.insert(ev.session_id.clone());
    }

    by_sig
        .into_iter()
        .map(|(signature, acc)| {
            let distinct_tasks = acc.tasks.len();
            let distinct_sessions = acc.sessions.len();
            let is_systemic = acc.occurrences >= policy.threshold
                && (distinct_tasks > 1 || distinct_sessions > 1);
            SignatureRecurrence {
                signature,
                source: acc.source.unwrap_or(ViolationSource::Blastguard),
                occurrences: acc.occurrences,
                distinct_tasks,
                distinct_sessions,
                first_seen: acc.first_seen,
                last_seen: acc.last_seen,
                is_systemic,
            }
        })
        .collect()
}

/// Convenience: only the signatures that are escalated as systemic.
pub fn systemic_issues(
    events: &[ViolationEvent],
    now: i64,
    policy: RecurrencePolicy,
) -> Vec<SignatureRecurrence> {
    detect_recurrence(events, now, policy)
        .into_iter()
        .filter(|r| r.is_systemic)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw_blastguard(rule: &str) -> RawViolation<'_> {
        RawViolation {
            rule_id: Some(rule),
            ..Default::default()
        }
    }

    fn raw_propguard(prop: &str) -> RawViolation<'_> {
        RawViolation {
            property_id: Some(prop),
            ..Default::default()
        }
    }

    fn raw_specguard<'a>(kind: &'a str, symbol: &'a str) -> RawViolation<'a> {
        RawViolation {
            drift_kind: Some(kind),
            symbol: Some(symbol),
            ..Default::default()
        }
    }

    fn raw_mutategate(op: &str) -> RawViolation<'_> {
        RawViolation {
            mutation_operator: Some(op),
            ..Default::default()
        }
    }

    fn raw_check_kind(kind: &str) -> RawViolation<'_> {
        RawViolation {
            check_kind: Some(kind),
            ..Default::default()
        }
    }

    #[test]
    fn normalize_signature_same_input_same_signature() {
        let a = normalize_signature(ViolationSource::Blastguard, &raw_blastguard("rm-rf"));
        let b = normalize_signature(ViolationSource::Blastguard, &raw_blastguard("rm-rf"));
        assert_eq!(a, b);
        assert_eq!(a.as_deref(), Some("blastguard:rm-rf"));
    }

    #[test]
    fn normalize_signature_is_case_and_whitespace_insensitive() {
        let a = normalize_signature(ViolationSource::Propguard, &raw_propguard("PROP-003"));
        let b = normalize_signature(ViolationSource::Propguard, &raw_propguard(" prop-003 "));
        assert_eq!(a, b);
        assert_eq!(a.as_deref(), Some("propguard:prop-003"));
    }

    #[test]
    fn normalize_signature_different_sources_differ() {
        let a = normalize_signature(ViolationSource::Blastguard, &raw_blastguard("x"));
        let b = normalize_signature(ViolationSource::Propguard, &raw_propguard("x"));
        assert_ne!(a, b);
    }

    #[test]
    fn normalize_signature_specguard_combines_kind_and_symbol() {
        let sig = normalize_signature(
            ViolationSource::Specguard,
            &raw_specguard("spec-without-impl", "crate::foo::Bar"),
        );
        assert_eq!(
            sig.as_deref(),
            Some("specguard:spec-without-impl:crate::foo::bar")
        );
    }

    #[test]
    fn normalize_signature_mutategate_uses_operator() {
        let sig = normalize_signature(
            ViolationSource::Mutategate,
            &raw_mutategate("arithmetic-op-swap"),
        );
        assert_eq!(sig.as_deref(), Some("mutategate:arithmetic-op-swap"));
    }

    #[test]
    fn normalize_signature_new_sources_use_check_kind() {
        let cases = [
            (ViolationSource::Donegate, "donegate"),
            (ViolationSource::Reviewgate, "reviewgate"),
            (ViolationSource::Tdd, "tdd"),
            (ViolationSource::Budgetguard, "budgetguard"),
            (ViolationSource::Autoflow, "autoflow"),
            (ViolationSource::Ctxrot, "ctxrot"),
        ];
        for (source, token) in cases {
            let sig = normalize_signature(source, &raw_check_kind("some-check"));
            assert_eq!(
                sig.as_deref(),
                Some(format!("{token}:some-check").as_str()),
                "source {token:?} did not use check_kind"
            );
        }
    }

    #[test]
    fn normalize_signature_new_sources_reject_empty_check_kind() {
        for source in [
            ViolationSource::Donegate,
            ViolationSource::Reviewgate,
            ViolationSource::Tdd,
            ViolationSource::Budgetguard,
            ViolationSource::Autoflow,
            ViolationSource::Ctxrot,
        ] {
            assert_eq!(normalize_signature(source, &raw_check_kind("   ")), None);
            assert_eq!(normalize_signature(source, &RawViolation::default()), None);
        }
    }

    #[test]
    fn violation_source_parse_round_trips_all_variants() {
        for source in [
            ViolationSource::Blastguard,
            ViolationSource::Propguard,
            ViolationSource::Specguard,
            ViolationSource::Mutategate,
            ViolationSource::Donegate,
            ViolationSource::Reviewgate,
            ViolationSource::Tdd,
            ViolationSource::Budgetguard,
            ViolationSource::Autoflow,
            ViolationSource::Ctxrot,
        ] {
            assert_eq!(ViolationSource::parse(source.token()), Some(source));
        }
    }

    #[test]
    fn violation_source_parse_rejects_unknown_token() {
        assert_eq!(ViolationSource::parse("not-a-real-gate"), None);
    }

    #[test]
    fn normalize_signature_missing_discriminator_is_rejected() {
        // A wholly-missing discriminator is not bucketable: it must be
        // dropped (`None`), not folded into a catch-all `<source>:unknown`
        // bucket that would merge unrelated failures into a false systemic
        // signature.
        assert_eq!(
            normalize_signature(ViolationSource::Blastguard, &RawViolation::default()),
            None
        );
    }

    #[test]
    fn normalize_signature_empty_and_whitespace_discriminator_are_rejected() {
        // Present-but-blank is treated the same as absent.
        assert_eq!(
            normalize_signature(ViolationSource::Propguard, &raw_propguard("")),
            None
        );
        assert_eq!(
            normalize_signature(ViolationSource::Mutategate, &raw_mutategate("   ")),
            None
        );
        // Specguard: a symbol is present but the required drift kind is blank
        // -> still rejected (kind is the mandatory discriminator).
        assert_eq!(
            normalize_signature(
                ViolationSource::Specguard,
                &raw_specguard("  ", "crate::foo")
            ),
            None
        );
    }

    #[test]
    fn empty_discriminator_inputs_do_not_collide_into_one_signature() {
        // Two genuinely-different empty-discriminator inputs (different
        // sources, different blank raw fields) must NOT collapse into a single
        // shared `<source>:unknown` signature that merges unrelated failures.
        // Under the reject-and-drop contract they both normalize to `None`,
        // so neither is recorded and there is no shared bucket to correlate
        // them — proving the false-systemic collision is gone.
        let a = normalize_signature(ViolationSource::Blastguard, &raw_blastguard(""));
        let b = normalize_signature(ViolationSource::Propguard, &raw_propguard("   "));
        assert_eq!(a, None);
        assert_eq!(b, None);

        // And via the build path: an un-bucketable raw yields no event to
        // persist, so unrelated blanks never land in the same ledger bucket.
        let ea = build_event(
            ViolationSource::Blastguard,
            &raw_blastguard(""),
            "task-a".to_string(),
            "session-a".to_string(),
            1,
            Some("disk full".to_string()),
        );
        let eb = build_event(
            ViolationSource::Propguard,
            &raw_propguard("   "),
            "task-b".to_string(),
            "session-b".to_string(),
            2,
            Some("timeout".to_string()),
        );
        assert!(ea.is_none());
        assert!(eb.is_none());
    }

    #[test]
    fn violation_source_parse_round_trips_token() {
        for src in [
            ViolationSource::Blastguard,
            ViolationSource::Propguard,
            ViolationSource::Specguard,
            ViolationSource::Mutategate,
        ] {
            let token = src.token();
            assert_eq!(ViolationSource::parse(token), Some(src));
        }
        assert_eq!(ViolationSource::parse("bogus"), None);
    }

    #[test]
    fn build_event_normalizes_signature() {
        let ev = build_event(
            ViolationSource::Blastguard,
            &raw_blastguard("rm-rf"),
            "task-1".to_string(),
            "session-a".to_string(),
            1000,
            Some("denied rm -rf /".to_string()),
        )
        .expect("well-formed discriminator builds an event");
        assert_eq!(ev.signature, "blastguard:rm-rf");
        assert_eq!(ev.task_key, "task-1");
    }

    fn ev(sig: &str, task: &str, session: &str, ts: i64) -> ViolationEvent {
        ViolationEvent {
            source: ViolationSource::Blastguard,
            signature: sig.to_string(),
            task_key: task.to_string(),
            session_id: session.to_string(),
            ts,
            detail: None,
        }
    }

    #[test]
    fn detect_recurrence_counts_occurrences_per_signature() {
        let events = vec![
            ev("blastguard:rm-rf", "t1", "s1", 100),
            ev("blastguard:rm-rf", "t2", "s1", 200),
            ev("propguard:prop-1", "t1", "s1", 150),
        ];
        let policy = RecurrencePolicy {
            threshold: 2,
            window_secs: 10_000,
        };
        let recur = detect_recurrence(&events, 1000, policy);
        assert_eq!(recur.len(), 2);

        let rm_rf = recur
            .iter()
            .find(|r| r.signature == "blastguard:rm-rf")
            .unwrap();
        assert_eq!(rm_rf.occurrences, 2);
        assert_eq!(rm_rf.distinct_tasks, 2);
        assert_eq!(rm_rf.first_seen, 100);
        assert_eq!(rm_rf.last_seen, 200);
    }

    #[test]
    fn detect_recurrence_excludes_events_outside_window() {
        let events = vec![
            ev("blastguard:rm-rf", "t1", "s1", 0),   // too old
            ev("blastguard:rm-rf", "t2", "s2", 950), // in window
        ];
        let policy = RecurrencePolicy {
            threshold: 1,
            window_secs: 100,
        };
        let recur = detect_recurrence(&events, 1000, policy);
        assert_eq!(recur.len(), 1);
        assert_eq!(recur[0].occurrences, 1);
    }

    #[test]
    fn detect_recurrence_ignores_future_events() {
        let events = vec![ev("blastguard:rm-rf", "t1", "s1", 5000)]; // future relative to now
        let policy = RecurrencePolicy::default();
        let recur = detect_recurrence(&events, 1000, policy);
        assert!(recur.is_empty());
    }

    #[test]
    fn is_systemic_requires_threshold_and_multiple_tasks() {
        // Same task repeated 3 times: NOT systemic (single task retry loop).
        let events = vec![
            ev("blastguard:rm-rf", "t1", "s1", 100),
            ev("blastguard:rm-rf", "t1", "s1", 200),
            ev("blastguard:rm-rf", "t1", "s1", 300),
        ];
        let policy = RecurrencePolicy {
            threshold: 3,
            window_secs: 10_000,
        };
        let recur = detect_recurrence(&events, 1000, policy);
        assert_eq!(recur.len(), 1);
        assert!(!recur[0].is_systemic, "single-task repeat is not systemic");
    }

    #[test]
    fn is_systemic_true_when_threshold_met_across_tasks() {
        let events = vec![
            ev("blastguard:rm-rf", "t1", "s1", 100),
            ev("blastguard:rm-rf", "t2", "s1", 200),
            ev("blastguard:rm-rf", "t3", "s2", 300),
        ];
        let policy = RecurrencePolicy {
            threshold: 3,
            window_secs: 10_000,
        };
        let recur = detect_recurrence(&events, 1000, policy);
        assert_eq!(recur.len(), 1);
        assert!(recur[0].is_systemic);
        assert_eq!(recur[0].distinct_tasks, 3);
    }

    #[test]
    fn is_systemic_true_when_multiple_sessions_same_task_key_reused() {
        // Different sessions could reuse the same task key across retries;
        // distinct_sessions > 1 alone should be enough.
        let events = vec![
            ev("propguard:prop-9", "retry-key", "s1", 100),
            ev("propguard:prop-9", "retry-key", "s2", 200),
        ];
        let policy = RecurrencePolicy {
            threshold: 2,
            window_secs: 10_000,
        };
        let recur = detect_recurrence(&events, 1000, policy);
        assert!(recur[0].is_systemic);
    }

    #[test]
    fn is_systemic_false_below_threshold_even_with_multiple_tasks() {
        let events = vec![
            ev("blastguard:rm-rf", "t1", "s1", 100),
            ev("blastguard:rm-rf", "t2", "s2", 200),
        ];
        let policy = RecurrencePolicy {
            threshold: 3,
            window_secs: 10_000,
        };
        let recur = detect_recurrence(&events, 1000, policy);
        assert!(!recur[0].is_systemic);
    }

    #[test]
    fn systemic_issues_filters_to_only_escalated() {
        let events = vec![
            ev("blastguard:rm-rf", "t1", "s1", 100),
            ev("blastguard:rm-rf", "t2", "s2", 200),
            ev("blastguard:rm-rf", "t3", "s3", 300),
            ev("propguard:prop-1", "t1", "s1", 150), // only 1 occurrence
        ];
        let policy = RecurrencePolicy {
            threshold: 3,
            window_secs: 10_000,
        };
        let systemic = systemic_issues(&events, 1000, policy);
        assert_eq!(systemic.len(), 1);
        assert_eq!(systemic[0].signature, "blastguard:rm-rf");
    }

    #[test]
    fn recurrence_policy_default_is_reasonable() {
        let policy = RecurrencePolicy::default();
        assert_eq!(policy.threshold, 3);
        assert_eq!(policy.window_secs, 86_400);
    }

    #[test]
    fn detect_recurrence_deterministic_across_calls() {
        let events = vec![
            ev("blastguard:rm-rf", "t1", "s1", 100),
            ev("propguard:prop-1", "t2", "s2", 200),
            ev("blastguard:rm-rf", "t3", "s3", 300),
        ];
        let policy = RecurrencePolicy {
            threshold: 2,
            window_secs: 10_000,
        };
        let r1 = detect_recurrence(&events, 1000, policy);
        let r2 = detect_recurrence(&events, 1000, policy);
        assert_eq!(r1, r2, "aggregation must be deterministic/pure");
    }
}
