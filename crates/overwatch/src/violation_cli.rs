/// CLI-facing glue for recording gate-violation events and rendering
/// fleet-level recurrence/escalation reports. Keeps the pure logic
/// (`violation::normalize_signature`, `violation::detect_recurrence`) free of
/// I/O so it stays unit-testable; this module is the thin, fail-soft shell
/// that wires it to `store` and stdout.
use crate::store;
use crate::violation::{
    self, RawViolation, RecurrencePolicy, SignatureRecurrence, ViolationSource,
};
use anyhow::Result;

/// Resolve the session id the same way `lease.rs` does: explicit arg, then
/// env var, then a pid-derived fallback.
fn resolve_session_id(session: Option<&str>) -> String {
    if let Some(s) = session {
        return s.to_string();
    }
    if let Ok(s) = std::env::var("CLAUDE_CODE_SESSION_ID") {
        return s;
    }
    format!("pid-{}", std::process::id())
}

/// Record one gate-violation event to the project-wide ledger.
///
/// `discriminator` carries the source-specific identifying field (blastguard
/// rule id, propguard PROP id, mutategate mutation operator). For specguard,
/// pass the drift kind here and the drifted symbol via `symbol`.
#[allow(clippy::too_many_arguments)]
pub fn record(
    source: ViolationSource,
    discriminator: &str,
    symbol: Option<&str>,
    task_key: &str,
    session: Option<&str>,
    detail: Option<&str>,
) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let now = store::now();
    let session_id = resolve_session_id(session);

    let raw = match source {
        ViolationSource::Blastguard => RawViolation {
            rule_id: Some(discriminator),
            ..Default::default()
        },
        ViolationSource::Propguard => RawViolation {
            property_id: Some(discriminator),
            ..Default::default()
        },
        ViolationSource::Specguard => RawViolation {
            drift_kind: Some(discriminator),
            symbol,
            ..Default::default()
        },
        ViolationSource::Mutategate => RawViolation {
            mutation_operator: Some(discriminator),
            ..Default::default()
        },
    };

    // An empty / whitespace-only / missing discriminator is not bucketable:
    // recording it under a catch-all `<source>:unknown` signature would merge
    // unrelated failures into one false "systemic" pattern. Fail-soft skip it
    // (report `recorded: false`) instead of polluting the ledger.
    let Some(event) = violation::build_event(
        source,
        &raw,
        task_key.to_string(),
        session_id,
        now,
        detail.map(str::to_string),
    ) else {
        println!(
            "{}",
            serde_json::json!({
                "recorded": false,
                "reason": "empty-or-missing-discriminator",
            })
        );
        return Ok(());
    };

    println!(
        "{}",
        serde_json::json!({ "recorded": true, "signature": event.signature })
    );

    store::append_violation(&cwd, &event)
}

/// Compute recurrence/escalation for all recorded violations, using `now`
/// and the given policy.
///
/// **Fail-CLOSED on an undetermined store.** This feeds
/// [`print_escalations`], whose empty output is read as "no signature has gone
/// systemic" — a verdict. So an unreadable ledger, or one holding a line this
/// build cannot decode, returns an `Err` (a diagnostic on stderr and a non-zero
/// exit) instead of an empty report. Only a genuinely `Absent` ledger — nothing
/// was ever recorded — yields the empty report, because that empty is a real
/// observation.
pub fn recurrence_report(policy: RecurrencePolicy) -> Result<Vec<SignatureRecurrence>> {
    let cwd = std::env::current_dir()?;
    let now = store::now();
    let events = match store::scan_violations(&cwd) {
        store::ViolationScan::Absent => Vec::new(),
        store::ViolationScan::Events(events) => events,
        store::ViolationScan::Undetermined => anyhow::bail!(
            "the violation ledger could not be read, or holds a line this build cannot \
             decode — the recurrence report would UNDER-count. Refusing to report \
             'no systemic signatures' from a store that could not be trusted."
        ),
    };
    Ok(violation::detect_recurrence(&events, now, policy))
}

/// Print the recurrence report (all signatures, systemic or not).
pub fn print_recurrence(policy: RecurrencePolicy, json: bool) -> Result<()> {
    let report = recurrence_report(policy)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    if report.is_empty() {
        println!("(no recorded violations)");
        return Ok(());
    }
    for r in &report {
        let marker = if r.is_systemic {
            "SYSTEMIC"
        } else {
            "isolated"
        };
        println!(
            "[{marker}] {}  occurrences={} tasks={} sessions={} last_seen={}",
            r.signature, r.occurrences, r.distinct_tasks, r.distinct_sessions, r.last_seen
        );
    }
    Ok(())
}

/// Print only the escalated (systemic) signatures.
pub fn print_escalations(policy: RecurrencePolicy, json: bool) -> Result<()> {
    let report = recurrence_report(policy)?;
    let systemic: Vec<_> = report.into_iter().filter(|r| r.is_systemic).collect();
    if json {
        println!("{}", serde_json::to_string_pretty(&systemic)?);
        return Ok(());
    }
    if systemic.is_empty() {
        println!("(no systemic issues)");
        return Ok(());
    }
    for r in &systemic {
        println!(
            "SYSTEMIC  {}  occurrences={} tasks={} sessions={} window=[{}, {}]",
            r.signature,
            r.occurrences,
            r.distinct_tasks,
            r.distinct_sessions,
            r.first_seen,
            r.last_seen
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_session_id_uses_arg_over_env() {
        std::env::remove_var("CLAUDE_CODE_SESSION_ID");
        assert_eq!(resolve_session_id(Some("explicit")), "explicit");
    }

    #[test]
    fn resolve_session_id_falls_back_to_pid() {
        std::env::remove_var("CLAUDE_CODE_SESSION_ID");
        let id = resolve_session_id(None);
        assert!(id.starts_with("pid-"));
    }
}
