//! The pure trajectory-matcher core — no IO, no panics.
//!
//! condukt's online verifier checks a task's OUTPUT (its done_criteria). This is
//! the sibling that checks the PATH the worker took: the ordered sequence of tool
//! calls it made, against an *expected* trajectory spec. Inspired by the trajectory
//! matchers in langchain-ai/agentevals.
//!
//! Everything here is a pure function of `(spec, actual)` so it can be exhaustively
//! unit-tested without touching the filesystem.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// How the expected `steps` are compared against the actual tool sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// The actual sequence must equal the expected REQUIRED steps in order.
    /// Optional steps may be absent, but if present must sit in their slot.
    Strict,
    /// Order is ignored; only set membership matters.
    Unordered,
    /// The required steps must appear in `actual` in order, but not necessarily
    /// contiguously — other tools may interleave.
    Subsequence,
}

/// One expected step in the trajectory.
#[derive(Debug, Clone, Deserialize)]
pub struct Step {
    /// The tool name we expect (e.g. `"Bash"`, `"Read"`).
    pub tool: String,
    /// Optional steps may be absent without failing the match. Defaults to false.
    #[serde(default)]
    pub optional: bool,
}

/// The expected trajectory, deserialized from JSON.
#[derive(Debug, Clone, Deserialize)]
pub struct Spec {
    pub mode: Mode,
    #[serde(default)]
    pub steps: Vec<Step>,
}

impl Spec {
    /// True when the spec cannot reject any trajectory: a `subsequence` spec
    /// with no required step (no steps, or only optional ones) requires
    /// nothing and reports no extras, so every `actual` would "pass". Such a
    /// spec is a spec error, not a satisfied check — the CLI refuses it
    /// (exit 2) instead of evaluating it (CLAUDE.md §3: an empty set is not
    /// a pass). `strict`/`unordered` are unaffected: with no steps they still
    /// reject any tool call as unexpected.
    pub fn constrains_nothing(&self) -> bool {
        self.mode == Mode::Subsequence && self.steps.iter().all(|s| s.optional)
    }
}

/// The verdict of comparing an actual trajectory against a [`Spec`].
///
/// `pass = missing.is_empty() && unexpected.is_empty() && !out_of_order` in every mode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MatchResult {
    pub pass: bool,
    /// Required expected tools that were not matched.
    pub missing: Vec<String>,
    /// Actual tools that had no place in the expected order/set.
    pub unexpected: Vec<String>,
    /// The right set appeared but in the wrong order (strict mode only).
    pub out_of_order: bool,
}

impl MatchResult {
    fn finalize(mut self) -> Self {
        self.pass = self.missing.is_empty() && self.unexpected.is_empty() && !self.out_of_order;
        self
    }
}

/// Compare an actual ordered tool sequence against the expected [`Spec`].
pub fn evaluate(spec: &Spec, actual: &[String]) -> MatchResult {
    match spec.mode {
        Mode::Strict => strict(&spec.steps, actual),
        Mode::Unordered => unordered(&spec.steps, actual),
        Mode::Subsequence => subsequence(&spec.steps, actual),
    }
}

// ── strict ──────────────────────────────────────────────────────────────────

fn strict(steps: &[Step], actual: &[String]) -> MatchResult {
    let mut missing = Vec::new();
    let mut unexpected = Vec::new();

    let mut i = 0; // expected index
    let mut j = 0; // actual index
    while i < steps.len() && j < actual.len() {
        if steps[i].tool == actual[j] {
            // matched this slot
            i += 1;
            j += 1;
        } else if steps[i].optional {
            // optional step is absent here — skip it and retry the slot
            i += 1;
        } else {
            // required mismatch: does actual[j] have a place further in the spec?
            // If so, the current required step is what's missing; otherwise the
            // actual tool is genuinely unexpected.
            if steps[i + 1..].iter().any(|s| s.tool == actual[j]) {
                missing.push(steps[i].tool.clone());
                i += 1;
            } else {
                unexpected.push(actual[j].clone());
                j += 1;
            }
        }
    }
    // leftover required expected steps are missing
    while i < steps.len() {
        if !steps[i].optional {
            missing.push(steps[i].tool.clone());
        }
        i += 1;
    }
    // leftover actual tools are unexpected
    while j < actual.len() {
        unexpected.push(actual[j].clone());
        j += 1;
    }

    // Reordering: tools that are BOTH missing and unexpected appeared, just in the
    // wrong place. That is an ordering problem, not a set problem — pull the common
    // bag out of both lists and flag out_of_order.
    let out_of_order = lift_reordering(&mut missing, &mut unexpected);

    MatchResult {
        pass: false,
        missing,
        unexpected,
        out_of_order,
    }
    .finalize()
}

/// Remove the multiset intersection from `missing`/`unexpected`; return true if any
/// common tool was found (meaning the right set appeared but out of order).
fn lift_reordering(missing: &mut Vec<String>, unexpected: &mut Vec<String>) -> bool {
    let mut want: HashMap<&str, usize> = HashMap::new();
    for m in missing.iter() {
        *want.entry(m.as_str()).or_insert(0) += 1;
    }
    let mut common: HashMap<String, usize> = HashMap::new();
    for u in unexpected.iter() {
        if let Some(c) = want.get_mut(u.as_str()) {
            if *c > 0 {
                *c -= 1;
                *common.entry(u.clone()).or_insert(0) += 1;
            }
        }
    }
    if common.is_empty() {
        return false;
    }
    let mut remove = common.clone();
    missing.retain(|m| match remove.get_mut(m) {
        Some(c) if *c > 0 => {
            *c -= 1;
            false
        }
        _ => true,
    });
    let mut remove = common;
    unexpected.retain(|u| match remove.get_mut(u) {
        Some(c) if *c > 0 => {
            *c -= 1;
            false
        }
        _ => true,
    });
    true
}

// ── unordered ───────────────────────────────────────────────────────────────

fn unordered(steps: &[Step], actual: &[String]) -> MatchResult {
    let expected_set: Vec<&str> = steps.iter().map(|s| s.tool.as_str()).collect();

    let mut missing = Vec::new();
    let mut seen_missing: Vec<&str> = Vec::new();
    for s in steps.iter() {
        if s.optional {
            continue;
        }
        if !actual.iter().any(|a| a == &s.tool) && !seen_missing.contains(&s.tool.as_str()) {
            missing.push(s.tool.clone());
            seen_missing.push(s.tool.as_str());
        }
    }

    let mut unexpected = Vec::new();
    let mut seen_unexpected: Vec<&str> = Vec::new();
    for a in actual.iter() {
        if !expected_set.contains(&a.as_str()) && !seen_unexpected.contains(&a.as_str()) {
            unexpected.push(a.clone());
            seen_unexpected.push(a.as_str());
        }
    }

    MatchResult {
        pass: false,
        missing,
        unexpected,
        out_of_order: false,
    }
    .finalize()
}

// ── subsequence ───────────────────────────────────────────────────────────────

fn subsequence(steps: &[Step], actual: &[String]) -> MatchResult {
    let mut missing = Vec::new();
    let mut j = 0; // actual cursor
    for s in steps.iter() {
        // advance through actual looking for this step's tool
        let mut found = None;
        for (k, a) in actual.iter().enumerate().skip(j) {
            if a == &s.tool {
                found = Some(k);
                break;
            }
        }
        match found {
            Some(k) => j = k + 1,
            None => {
                if !s.optional {
                    missing.push(s.tool.clone());
                }
                // optional-and-absent: just skip without advancing j
            }
        }
    }

    // Interleaved extras are allowed in subsequence mode, so unexpected stays empty.
    MatchResult {
        pass: false,
        missing,
        unexpected: Vec::new(),
        out_of_order: false,
    }
    .finalize()
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn step(tool: &str) -> Step {
        Step {
            tool: tool.to_string(),
            optional: false,
        }
    }
    fn opt(tool: &str) -> Step {
        Step {
            tool: tool.to_string(),
            optional: true,
        }
    }
    fn names(xs: &[&str]) -> Vec<String> {
        xs.iter().map(|s| s.to_string()).collect()
    }

    // ── strict ──

    #[test]
    fn strict_exact_match_passes() {
        let spec = Spec {
            mode: Mode::Strict,
            steps: vec![step("Read"), step("Edit")],
        };
        let r = evaluate(&spec, &names(&["Read", "Edit"]));
        assert!(r.pass);
        assert!(r.missing.is_empty());
        assert!(r.unexpected.is_empty());
        assert!(!r.out_of_order);
    }

    #[test]
    fn strict_wrong_order_flags_out_of_order() {
        let spec = Spec {
            mode: Mode::Strict,
            steps: vec![step("Read"), step("Edit")],
        };
        let r = evaluate(&spec, &names(&["Edit", "Read"]));
        assert!(!r.pass);
        assert!(r.out_of_order);
        assert!(r.missing.is_empty());
        assert!(r.unexpected.is_empty());
    }

    #[test]
    fn strict_extra_tool_is_unexpected() {
        let spec = Spec {
            mode: Mode::Strict,
            steps: vec![step("Read"), step("Edit")],
        };
        let r = evaluate(&spec, &names(&["Read", "Bash", "Edit"]));
        assert!(!r.pass);
        assert_eq!(r.unexpected, names(&["Bash"]));
        assert!(r.missing.is_empty());
        assert!(!r.out_of_order);
    }

    #[test]
    fn strict_missing_required_step() {
        let spec = Spec {
            mode: Mode::Strict,
            steps: vec![step("Read"), step("Edit"), step("Bash")],
        };
        let r = evaluate(&spec, &names(&["Read", "Edit"]));
        assert!(!r.pass);
        assert_eq!(r.missing, names(&["Bash"]));
        assert!(r.unexpected.is_empty());
        assert!(!r.out_of_order);
    }

    #[test]
    fn strict_optional_present_in_slot_passes() {
        let spec = Spec {
            mode: Mode::Strict,
            steps: vec![step("Read"), opt("Grep"), step("Edit")],
        };
        let r = evaluate(&spec, &names(&["Read", "Grep", "Edit"]));
        assert!(r.pass);
    }

    #[test]
    fn strict_optional_absent_passes() {
        let spec = Spec {
            mode: Mode::Strict,
            steps: vec![step("Read"), opt("Grep"), step("Edit")],
        };
        let r = evaluate(&spec, &names(&["Read", "Edit"]));
        assert!(r.pass);
    }

    #[test]
    fn strict_empty_actual_all_required_missing() {
        let spec = Spec {
            mode: Mode::Strict,
            steps: vec![step("Read"), opt("Grep"), step("Edit")],
        };
        let r = evaluate(&spec, &[]);
        assert_eq!(r.missing, names(&["Read", "Edit"]));
        assert!(!r.pass);
    }

    #[test]
    fn strict_empty_spec_extra_actual_is_unexpected() {
        let spec = Spec {
            mode: Mode::Strict,
            steps: vec![],
        };
        let r = evaluate(&spec, &names(&["Read"]));
        assert_eq!(r.unexpected, names(&["Read"]));
        assert!(!r.pass);
    }

    #[test]
    fn strict_empty_spec_empty_actual_passes() {
        let spec = Spec {
            mode: Mode::Strict,
            steps: vec![],
        };
        let r = evaluate(&spec, &[]);
        assert!(r.pass);
    }

    // ── unordered ──

    #[test]
    fn unordered_ignores_order() {
        let spec = Spec {
            mode: Mode::Unordered,
            steps: vec![step("Read"), step("Edit")],
        };
        let r = evaluate(&spec, &names(&["Edit", "Read"]));
        assert!(r.pass);
        assert!(!r.out_of_order);
    }

    #[test]
    fn unordered_reports_missing_and_unexpected_sets() {
        let spec = Spec {
            mode: Mode::Unordered,
            steps: vec![step("Read"), step("Edit")],
        };
        let r = evaluate(&spec, &names(&["Read", "Bash", "Bash"]));
        assert_eq!(r.missing, names(&["Edit"]));
        assert_eq!(r.unexpected, names(&["Bash"])); // deduped
        assert!(!r.pass);
    }

    #[test]
    fn unordered_optional_not_required() {
        let spec = Spec {
            mode: Mode::Unordered,
            steps: vec![step("Read"), opt("Grep")],
        };
        let r = evaluate(&spec, &names(&["Read"]));
        assert!(r.pass);
    }

    #[test]
    fn unordered_empty_actual_all_missing() {
        let spec = Spec {
            mode: Mode::Unordered,
            steps: vec![step("Read"), step("Edit")],
        };
        let r = evaluate(&spec, &[]);
        assert_eq!(r.missing, names(&["Read", "Edit"]));
    }

    // ── subsequence ──

    #[test]
    fn subsequence_interleaved_passes() {
        let spec = Spec {
            mode: Mode::Subsequence,
            steps: vec![step("Read"), step("Edit")],
        };
        let r = evaluate(&spec, &names(&["Read", "Bash", "Grep", "Edit"]));
        assert!(r.pass);
        assert!(r.unexpected.is_empty()); // extras allowed
    }

    #[test]
    fn subsequence_out_of_order_is_missing_not_reorder() {
        let spec = Spec {
            mode: Mode::Subsequence,
            steps: vec![step("Read"), step("Edit")],
        };
        let r = evaluate(&spec, &names(&["Edit", "Read"]));
        // Read found at idx 1, then Edit must come after idx 1 -> absent
        assert_eq!(r.missing, names(&["Edit"]));
        assert!(!r.out_of_order);
        assert!(!r.pass);
    }

    #[test]
    fn subsequence_optional_absent_ok() {
        let spec = Spec {
            mode: Mode::Subsequence,
            steps: vec![step("Read"), opt("Grep"), step("Edit")],
        };
        let r = evaluate(&spec, &names(&["Read", "Edit"]));
        assert!(r.pass);
    }

    #[test]
    fn subsequence_empty_actual_all_missing() {
        let spec = Spec {
            mode: Mode::Subsequence,
            steps: vec![step("Read"), step("Edit")],
        };
        let r = evaluate(&spec, &[]);
        assert_eq!(r.missing, names(&["Read", "Edit"]));
    }

    // ── serde defaults ──

    #[test]
    fn step_optional_defaults_false() {
        let s: Step = serde_json::from_str(r#"{"tool":"Bash"}"#).unwrap();
        assert!(!s.optional);
    }

    #[test]
    fn spec_deserializes_from_json() {
        let spec: Spec = serde_json::from_str(
            r#"{"mode":"subsequence","steps":[{"tool":"Read"},{"tool":"Edit","optional":true}]}"#,
        )
        .unwrap();
        assert_eq!(spec.mode, Mode::Subsequence);
        assert_eq!(spec.steps.len(), 2);
        assert!(spec.steps[1].optional);
    }

    #[test]
    fn result_serializes() {
        let r = evaluate(
            &Spec {
                mode: Mode::Strict,
                steps: vec![step("Read")],
            },
            &names(&["Read"]),
        );
        let j = serde_json::to_string(&r).unwrap();
        assert!(j.contains("\"pass\":true"));
        assert!(j.contains("\"out_of_order\":false"));
    }
}
