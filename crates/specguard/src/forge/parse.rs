//! Parse the normalize agent's stdout: split the report body (the requirement
//! TOML, or the escalation request) from the machine-readable trailer.
//!
//! Trailer contract (the prompt forces this at the very end):
//!
//! ```text
//! <<<SPEC_DRAFT>>>
//! rigor: <pass|fail>
//! needs_user: <yes|no>
//! summary: <one line>
//! ```
//!
//! Mirrors specguard's parser: the LAST marker wins; a missing marker means the
//! output is incomplete and must NOT be acted on.

pub const MARKER: &str = "<<<SPEC_DRAFT>>>";

#[derive(Debug, PartialEq, Eq)]
pub struct Parsed {
    /// Everything before the marker line, trailing-trimmed.
    pub body: String,
    /// True only when the rigor value is exactly the token `pass` (G1–G4 passed).
    pub rigor_pass: bool,
    /// True when a human must review (escalation). Only a bare `no` is false;
    /// absent/empty/unrecognised values fail closed to true (indeterminate).
    pub needs_user: bool,
    pub summary: String,
    /// False when no marker — the output is incomplete (do not act).
    pub marker_found: bool,
}

pub fn parse(stdout: &str) -> Parsed {
    let marker_idx = stdout
        .lines()
        .enumerate()
        .filter(|(_, l)| l.contains(MARKER))
        .map(|(i, _)| i)
        .last();

    let Some(marker_idx) = marker_idx else {
        return Parsed {
            body: stdout.trim_end().to_string(),
            rigor_pass: false,
            needs_user: false,
            summary: String::new(),
            marker_found: false,
        };
    };

    let lines: Vec<&str> = stdout.lines().collect();
    let body = lines[..marker_idx].join("\n").trim_end().to_string();
    let trailer = &lines[marker_idx + 1..];

    // `rigor` is an exact contract token: only a bare `pass` (first token,
    // case-insensitive) satisfies the G1–G4 gate. A prefix match would accept
    // hedges such as `pass?` / `passable`; anything that is not exactly `pass`
    // (including `fail`, absent, empty or unrecognised) is `rigor_pass = false`.
    let rigor_pass = first_token(trailer, "rigor").as_deref() == Some("pass");

    // `needs_user` mirrors specguard's own parser (`crate::parse`): the clean
    // side is ONLY the bare contract token `no`. A `yes…` value escalates, and
    // so does everything else — absent, empty, or an unrecognised/hedged value
    // is indeterminate, and indeterminate is not "clean": it fails closed to
    // `needs_user = true` so the draft path escalates via the sentinel instead
    // of hiding the hedge from the human.
    let needs_user_raw = field(trailer, "needs_user").map(|v| v.trim().to_ascii_lowercase());
    let needs_user = match needs_user_raw.as_deref() {
        Some("no") => false,
        Some(v) if v.split_whitespace().next().unwrap_or("").starts_with("yes") => true,
        other => {
            let seen = match other {
                Some(v) if !v.is_empty() => format!("an unrecognised value ('{v}')"),
                _ => "absent/empty".to_string(),
            };
            eprintln!(
                "specforge: WARN parse: marker found but 'needs_user' verdict is {seen}; failing closed to needs_user=true (indeterminate) — a verdict that could not be determined is not 'clean'"
            );
            true
        }
    };

    let summary = field(trailer, "summary").unwrap_or_default();

    Parsed {
        body,
        rigor_pass,
        needs_user,
        summary,
        marker_found: true,
    }
}

/// First whitespace-delimited token of `key`'s value, lowercased.
fn first_token(lines: &[&str], key: &str) -> Option<String> {
    field(lines, key).map(|v| {
        v.split_whitespace()
            .next()
            .unwrap_or("")
            .to_ascii_lowercase()
    })
}

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
    fn parses_pass_with_body() {
        let s = "[[requirement]]\nid = \"R1\"\n\n<<<SPEC_DRAFT>>>\nrigor: pass\nneeds_user: no\nsummary: ok";
        let p = parse(s);
        assert!(p.marker_found);
        assert!(p.rigor_pass);
        assert!(!p.needs_user);
        assert_eq!(p.summary, "ok");
        assert!(p.body.contains("[[requirement]]"));
    }

    #[test]
    fn parses_fail_escalation() {
        let s = "canon が rate-limit について沈黙\n\n<<<SPEC_DRAFT>>>\nrigor: fail\nneeds_user: yes\nsummary: 閾値が未定義";
        let p = parse(s);
        assert!(!p.rigor_pass);
        assert!(p.needs_user);
        assert_eq!(p.summary, "閾値が未定義");
    }

    #[test]
    fn no_marker_is_incomplete() {
        let p = parse("just some text without a trailer");
        assert!(!p.marker_found);
        assert!(!p.rigor_pass);
    }

    #[test]
    fn last_marker_wins() {
        let s = "<<<SPEC_DRAFT>>>\nrigor: pass\nneeds_user: no\nsummary: early\n\
                 body\n<<<SPEC_DRAFT>>>\nrigor: fail\nneeds_user: yes\nsummary: late";
        let p = parse(s);
        assert!(!p.rigor_pass);
        assert_eq!(p.summary, "late");
    }

    #[test]
    fn case_insensitive_keys() {
        let s = "b\n<<<SPEC_DRAFT>>>\nRigor: PASS\nNeeds_User: NO\nSummary: Cap";
        let p = parse(s);
        assert!(p.rigor_pass);
        assert!(!p.needs_user);
        assert_eq!(p.summary, "Cap");
    }

    #[test]
    fn ca_specguard_02_hedged_needs_user_must_fail_closed_to_escalate() {
        // CA-specguard-02: `needs_user` uses `starts_with("yes")` and defaults
        // `false` when that check fails — so a hedged/ambiguous value (neither a
        // clean "yes" nor a clean "no") silently resolves to "no escalation
        // needed" and the hedge never reaches the human at ratify. The sibling
        // `crates/specguard/src/parse.rs` fails closed on the same shape of
        // input (indeterminate -> needs_user=true); this parser does not.
        let s = "b\n<<<SPEC_DRAFT>>>\nrigor: pass\nneeds_user: unclear\nsummary: x";
        let p = parse(s);
        assert!(
            p.needs_user,
            "a hedged needs_user value must fail closed to escalate (true), \
             not silently default to no-escalation (false)"
        );
    }

    #[test]
    fn ca_specguard_03_hedged_rigor_pass_must_not_satisfy_prefix_leniency() {
        // CA-specguard-03: `rigor_pass` accepts anything whose first token
        // starts with "pass" (`starts_with("pass")`), so a hedged verdict like
        // "pass?" or "passable" is wrongly treated as a clean G1-G4 rigor pass
        // instead of failing the gate.
        let s = "b\n<<<SPEC_DRAFT>>>\nrigor: pass?\nneeds_user: no\nsummary: x";
        let p = parse(s);
        assert!(
            !p.rigor_pass,
            "a hedged 'pass?' rigor value must NOT satisfy the rigor gate via prefix leniency"
        );
    }
}
