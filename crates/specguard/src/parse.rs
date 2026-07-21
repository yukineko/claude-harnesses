//! Parse the agent's stdout: split the human-readable report from the
//! machine-readable trailer the prompt forces the agent to emit.
//!
//! Trailer contract (the prompt instructs the agent to print exactly this at
//! the very end):
//!
//! ```text
//! <<<SPEC_AUDIT>>>
//! needs_user: <yes|no>
//! summary: <one line>
//! ```

/// The marker token that delimits the trailer. Kept identical between the
/// prompt template and the parser.
pub const MARKER: &str = "<<<SPEC_AUDIT>>>";

#[derive(Debug, PartialEq, Eq)]
pub struct Parsed {
    /// Report body (everything before the marker line), trailing-trimmed.
    pub report: String,
    /// True when at least one finding needs human review.
    pub needs_user: bool,
    /// One-line summary from the trailer (empty when absent).
    pub summary: String,
    /// False when no marker was found — the report is incomplete and the
    /// caller must NOT raise a sentinel (avoids false positives).
    pub marker_found: bool,
    /// True when the marker WAS found but the `needs_user` verdict could not be
    /// determined (field absent, empty, or an unrecognised token). The audit ran
    /// but did not state a clear verdict — "cannot determine" is NOT "clean", so
    /// `needs_user` is forced true (fail-closed, fail-open #7). Kept distinct
    /// from a genuine `yes` so this meta-condition stays STICKY through the
    /// verify/refute pass: a skeptic can quote-refute a concrete finding, but it
    /// has nothing to refute here, so an indeterminate verdict must never be
    /// dropped back to clean.
    pub indeterminate: bool,
}

/// Parse agent stdout. When the marker is missing, `marker_found` is false and
/// the whole text is returned as the report so nothing is lost.
pub fn parse(stdout: &str) -> Parsed {
    // Use the LAST marker line, mirroring the reference bash runner: if the
    // model echoes the contract earlier, only the final emission counts.
    let marker_idx = stdout
        .lines()
        .enumerate()
        .filter(|(_, l)| l.contains(MARKER))
        .map(|(i, _)| i)
        .last();

    let Some(marker_idx) = marker_idx else {
        return Parsed {
            report: stdout.trim_end().to_string(),
            needs_user: false,
            summary: String::new(),
            marker_found: false,
            indeterminate: false,
        };
    };

    let lines: Vec<&str> = stdout.lines().collect();
    let report = lines[..marker_idx].join("\n").trim_end().to_string();
    let trailer = &lines[marker_idx + 1..];

    // Fail-closed verdict (fail-open #7): the marker IS present, so the audit
    // ran — but "the audit ran and could not state a verdict" is NOT the same as
    // "the audit ran and found nothing". The old code mapped `starts_with("yes")
    // else false`, so an absent field, an empty value, or any unrecognised token
    // (`maybe`, `error`, a truncated line) silently became `no`/clean and
    // advanced the baseline over content whose verdict was never established.
    //
    // Now: only an explicit, recognised `no` is clean; an explicit `yes` is a
    // finding; ANYTHING ELSE is indeterminate → `needs_user = true`, mirroring
    // the `completeness`/`fold_inconclusive` fail-safe ("cannot confirm →
    // surface") already used one module over. The prompt (`audit-prompt.md`) is
    // fixed in lockstep to tell a well-behaved auditor to emit `yes` when it
    // cannot compare; this is the deterministic backstop for when it does not.
    let verdict = field(trailer, "needs_user").map(|v| {
        // Take only the first whitespace token so "yes (3 findings)" -> "yes".
        v.split_whitespace()
            .next()
            .unwrap_or("")
            .to_ascii_lowercase()
    });
    let (needs_user, indeterminate) = match verdict.as_deref() {
        Some(t) if t.starts_with("yes") => (true, false),
        Some(t) if t.starts_with("no") => (false, false),
        other => {
            let seen = match other {
                Some(t) if !t.is_empty() => format!("an unrecognised value ('{t}')"),
                _ => "absent/empty".to_string(),
            };
            eprintln!(
                "specguard: WARN parse: marker found but 'needs_user' verdict is {seen}; failing closed to needs_user=true (indeterminate) — a verdict that could not be determined is not 'clean'"
            );
            (true, true)
        }
    };

    let summary = field(trailer, "summary").unwrap_or_default();

    Parsed {
        report,
        needs_user,
        summary,
        marker_found: true,
        indeterminate,
    }
}

/// Find `key:` in the trailer (case-insensitive) and return its value, trimmed.
fn field(lines: &[&str], key: &str) -> Option<String> {
    let key_lower = key.to_ascii_lowercase();
    for line in lines {
        let trimmed = line.trim_start();
        if let Some((lhs, rhs)) = trimmed.split_once(':') {
            if lhs.trim().to_ascii_lowercase() == key_lower {
                return Some(rhs.trim().to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_yes_with_report() {
        let s =
            "# Report\n\nbody line\n\n<<<SPEC_AUDIT>>>\nneeds_user: yes\nsummary: fix the thing";
        let p = parse(s);
        assert!(p.marker_found);
        assert!(p.needs_user);
        assert_eq!(p.summary, "fix the thing");
        assert_eq!(p.report, "# Report\n\nbody line");
    }

    #[test]
    fn yes_with_trailing_tokens_still_yes() {
        let s = "r\n<<<SPEC_AUDIT>>>\nneeds_user: yes (3 findings)\nsummary: x";
        assert!(parse(s).needs_user);
    }

    #[test]
    fn no_marker_means_not_found_and_no_pending() {
        let s = "# Report\nbody without trailer\n";
        let p = parse(s);
        assert!(!p.marker_found);
        assert!(!p.needs_user);
        assert_eq!(p.report, "# Report\nbody without trailer");
    }

    #[test]
    fn needs_user_no_is_false() {
        let s = "r\n<<<SPEC_AUDIT>>>\nneeds_user: no\nsummary: none";
        let p = parse(s);
        assert!(p.marker_found);
        assert!(!p.needs_user);
    }

    #[test]
    fn last_marker_wins() {
        let s = "<<<SPEC_AUDIT>>>\nneeds_user: no\nsummary: early\n\
                 real report\n<<<SPEC_AUDIT>>>\nneeds_user: yes\nsummary: late";
        let p = parse(s);
        assert!(p.needs_user);
        assert_eq!(p.summary, "late");
    }

    #[test]
    fn case_insensitive_keys() {
        let s = "r\n<<<SPEC_AUDIT>>>\nNeeds_User: YES\nSummary: Cap";
        let p = parse(s);
        assert!(p.needs_user);
        assert_eq!(p.summary, "Cap");
    }

    // --- Regression tests for backlog 61599e06 ("fail-open #7") ---------------
    // When the marker IS present (the audit RAN) but the `needs_user` verdict
    // cannot be determined — field absent, empty, or an unrecognised token — the
    // parser must fail CLOSED (needs_user=true, indeterminate=true), NOT default
    // to clean. "The audit ran and could not state a verdict" is not "the audit
    // ran and found nothing". These tests pin the fixed contract.

    #[test]
    fn absent_needs_user_field_fails_closed_indeterminate() {
        // fail-open #7 (61599e06): marker present, but the trailer has only a
        // `summary:` line and NO `needs_user:` line. This was `false`/clean
        // before the fix; it must now be indeterminate → needs_user=true.
        let s = "r\n<<<SPEC_AUDIT>>>\nsummary: audit could not compare";
        let p = parse(s);
        assert!(p.marker_found, "marker is present");
        assert!(
            p.needs_user,
            "absent needs_user verdict must fail closed to needs_user=true"
        );
        assert!(
            p.indeterminate,
            "absent verdict is indeterminate, not a clean pass"
        );
    }

    #[test]
    fn empty_needs_user_value_fails_closed_indeterminate() {
        // fail-open #7 (61599e06): `needs_user:` with nothing after the colon.
        let s = "r\n<<<SPEC_AUDIT>>>\nneeds_user:\nsummary: x";
        let p = parse(s);
        assert!(p.marker_found);
        assert!(
            p.needs_user,
            "empty needs_user value must fail closed to needs_user=true"
        );
        assert!(p.indeterminate, "empty value is indeterminate");
    }

    #[test]
    fn unrecognised_needs_user_tokens_fail_closed_indeterminate() {
        // fail-open #7 (61599e06): any token that is neither `yes*` nor `no*`
        // is indeterminate → needs_user=true. Note `"n"` does NOT start with
        // `"no"` and `"y"` does NOT start with `"yes"`, so both are
        // indeterminate — asserted explicitly here.
        for tok in ["maybe", "error", "unknown", "y", "n", "true", "42", "???"] {
            let s = format!("r\n<<<SPEC_AUDIT>>>\nneeds_user: {tok}\nsummary: s");
            let p = parse(&s);
            assert!(p.marker_found, "marker present for token {tok:?}");
            assert!(
                p.needs_user,
                "unrecognised token {tok:?} must fail closed to needs_user=true"
            );
            assert!(
                p.indeterminate,
                "unrecognised token {tok:?} must be indeterminate"
            );
        }
    }

    #[test]
    fn explicit_no_stays_clean_not_indeterminate() {
        // A recognised `no` (and `no findings`, which starts with "no") is the
        // ONLY clean verdict: needs_user=false AND indeterminate=false.
        for val in ["no", "no findings"] {
            let s = format!("r\n<<<SPEC_AUDIT>>>\nneeds_user: {val}\nsummary: none");
            let p = parse(&s);
            assert!(p.marker_found);
            assert!(!p.needs_user, "{val:?} is a clean verdict");
            assert!(!p.indeterminate, "{val:?} is a determined verdict");
        }
    }

    #[test]
    fn explicit_yes_is_finding_not_indeterminate() {
        // A genuine `yes` finding raises needs_user but is NOT indeterminate.
        // This discriminates a real finding from the fail-closed default: both
        // set needs_user=true, but only the unparseable one is indeterminate.
        for val in ["yes", "yes (3 findings)"] {
            let s = format!("r\n<<<SPEC_AUDIT>>>\nneeds_user: {val}\nsummary: fix");
            let p = parse(&s);
            assert!(p.marker_found);
            assert!(p.needs_user, "{val:?} is a real finding");
            assert!(
                !p.indeterminate,
                "{val:?} is a determined finding, not a fail-closed default"
            );
        }
    }

    #[test]
    fn no_marker_is_not_indeterminate() {
        // Unchanged contract: no marker at all → marker_found=false,
        // needs_user=false, indeterminate=false (caller handles missing marker).
        let s = "# Report\nbody without any trailer\n";
        let p = parse(s);
        assert!(!p.marker_found);
        assert!(!p.needs_user);
        assert!(
            !p.indeterminate,
            "a missing marker is a separate condition, not indeterminate"
        );
    }
}
