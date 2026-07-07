//! Deterministic, referentially-transparent priority scorer.
//!
//! Compass ONE #4: the scout skill's Phase-3 ranking used to be an LLM hand
//! calculation of `(severity × goal-proximity) ÷ effort`, with an L2/L5 lens
//! nudge applied by prose convention rather than code. This module moves that
//! arithmetic into a pure function so the same candidate always scores the
//! same way — no clock, no RNG, no I/O, no LLM judgment call.
//!
//! `L2` (security) and `L5` (safety) — scout's audit lenses — carry a lens
//! multiplier greater than 1.0, encoding "壊さない・安全側" (favor not
//! breaking things, favor the safe side): candidates surfaced through the
//! security or safety lens outrank an otherwise-identical candidate surfaced
//! through any other lens.

/// Fixed severity weight table. Higher severity always yields a strictly
/// higher score (all else equal).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    High,
    Medium,
    Low,
}

impl Severity {
    /// Fixed weight for this severity level. One obvious table; never derived
    /// from input at call time.
    pub fn weight(&self) -> f64 {
        match self {
            Severity::High => 3.0,
            Severity::Medium => 2.0,
            Severity::Low => 1.0,
        }
    }

    /// Best-effort parse from a case-insensitive label. Optional convenience;
    /// the pure [`score`] function is the deliverable.
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "high" => Some(Severity::High),
            "medium" => Some(Severity::Medium),
            "low" => Some(Severity::Low),
            _ => None,
        }
    }
}

/// Fixed effort divisor table, monotonically increasing with size. Lower
/// effort always yields a strictly higher score (all else equal).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effort {
    Xs,
    S,
    M,
    L,
    Xl,
}

impl Effort {
    /// Fixed divisor factor for this effort size. One obvious table; never
    /// derived from input at call time.
    pub fn factor(&self) -> f64 {
        match self {
            Effort::Xs => 1.0,
            Effort::S => 2.0,
            Effort::M => 3.0,
            Effort::L => 5.0,
            Effort::Xl => 8.0,
        }
    }

    /// Best-effort parse from a case-insensitive label. Optional convenience;
    /// the pure [`score`] function is the deliverable.
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "xs" => Some(Effort::Xs),
            "s" => Some(Effort::S),
            "m" => Some(Effort::M),
            "l" => Some(Effort::L),
            "xl" => Some(Effort::Xl),
            _ => None,
        }
    }
}

/// Scout's five audit lenses: L1 current-issues, L2 security, L3
/// industry-standard, L4 missing-work, L5 safety.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lens {
    L1,
    L2,
    L3,
    L4,
    L5,
}

impl Lens {
    /// Fixed lens multiplier. L2 (security) and L5 (safety) score strictly
    /// higher than the rest ("壊さない・安全側"); L1/L3/L4 are neutral (1.0).
    pub fn multiplier(&self) -> f64 {
        match self {
            Lens::L2 | Lens::L5 => 1.5,
            Lens::L1 | Lens::L3 | Lens::L4 => 1.0,
        }
    }

    /// Best-effort parse from a case-insensitive label (`"l1"`..`"l5"`).
    /// Optional convenience; the pure [`score`] function is the deliverable.
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "l1" => Some(Lens::L1),
            "l2" => Some(Lens::L2),
            "l3" => Some(Lens::L3),
            "l4" => Some(Lens::L4),
            "l5" => Some(Lens::L5),
            _ => None,
        }
    }
}

/// A policy/action candidate to rank. `goal_proximity` is expected in
/// `[0.0, 1.0]`; [`score`] clamps it defensively so out-of-range input
/// (e.g. a caller bug passing `5.0` or `-1.0`) can't produce a nonsense
/// (unbounded or negative) score.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Candidate {
    pub severity: Severity,
    pub effort: Effort,
    pub lens: Lens,
    pub goal_proximity: f64,
}

/// Score a candidate: `severity_weight × goal_proximity ÷ effort_factor ×
/// lens_multiplier`.
///
/// Pure and referentially transparent: the same `Candidate` value always
/// produces the same `f64` (bit-for-bit) — no clock, no RNG, no I/O, no
/// hidden state. `goal_proximity` is clamped into `[0.0, 1.0]` before use, so
/// out-of-range input is defused rather than propagated into the result.
pub fn score(candidate: &Candidate) -> f64 {
    let goal_proximity = candidate.goal_proximity.clamp(0.0, 1.0);
    candidate.severity.weight() * goal_proximity / candidate.effort.factor()
        * candidate.lens.multiplier()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Candidate {
        Candidate {
            severity: Severity::Medium,
            effort: Effort::M,
            lens: Lens::L1,
            goal_proximity: 0.5,
        }
    }

    #[test]
    fn higher_severity_yields_higher_score() {
        let high = score(&Candidate {
            severity: Severity::High,
            ..base()
        });
        let medium = score(&Candidate {
            severity: Severity::Medium,
            ..base()
        });
        let low = score(&Candidate {
            severity: Severity::Low,
            ..base()
        });
        assert!(
            high > medium,
            "High ({high}) should outrank Medium ({medium})"
        );
        assert!(medium > low, "Medium ({medium}) should outrank Low ({low})");
    }

    #[test]
    fn lower_effort_yields_higher_score() {
        let xs = score(&Candidate {
            effort: Effort::Xs,
            ..base()
        });
        let s = score(&Candidate {
            effort: Effort::S,
            ..base()
        });
        let m = score(&Candidate {
            effort: Effort::M,
            ..base()
        });
        let l = score(&Candidate {
            effort: Effort::L,
            ..base()
        });
        let xl = score(&Candidate {
            effort: Effort::Xl,
            ..base()
        });
        assert!(xs > s, "Xs ({xs}) should outrank S ({s})");
        assert!(s > m, "S ({s}) should outrank M ({m})");
        assert!(m > l, "M ({m}) should outrank L ({l})");
        assert!(l > xl, "L ({l}) should outrank Xl ({xl})");
    }

    #[test]
    fn higher_goal_proximity_yields_higher_score() {
        let low = score(&Candidate {
            goal_proximity: 0.1,
            ..base()
        });
        let mid = score(&Candidate {
            goal_proximity: 0.5,
            ..base()
        });
        let high = score(&Candidate {
            goal_proximity: 0.9,
            ..base()
        });
        assert!(low < mid, "0.1 ({low}) should score lower than 0.5 ({mid})");
        assert!(
            mid < high,
            "0.5 ({mid}) should score lower than 0.9 ({high})"
        );
    }

    #[test]
    fn security_and_safety_lenses_outrank_the_rest() {
        let l1 = score(&Candidate {
            lens: Lens::L1,
            ..base()
        });
        let l2 = score(&Candidate {
            lens: Lens::L2,
            ..base()
        });
        let l3 = score(&Candidate {
            lens: Lens::L3,
            ..base()
        });
        let l4 = score(&Candidate {
            lens: Lens::L4,
            ..base()
        });
        let l5 = score(&Candidate {
            lens: Lens::L5,
            ..base()
        });

        assert!(
            l2 > l1 && l2 > l3 && l2 > l4,
            "L2 (security) must strictly outrank L1/L3/L4"
        );
        assert!(
            l5 > l1 && l5 > l3 && l5 > l4,
            "L5 (safety) must strictly outrank L1/L3/L4"
        );
        // The non-security/safety lenses are all neutral (equal) among themselves.
        assert_eq!(l1, l3);
        assert_eq!(l3, l4);
        // L2 and L5 carry the same fixed multiplier, so they tie each other.
        assert_eq!(l2, l5);
    }

    #[test]
    fn referentially_transparent_same_input_same_bits() {
        let c = base();
        let a = score(&c);
        let b = score(&c);
        assert_eq!(
            a.to_bits(),
            b.to_bits(),
            "same Candidate must score identically bit-for-bit"
        );
    }

    #[test]
    fn out_of_range_goal_proximity_is_clamped() {
        let over = score(&Candidate {
            goal_proximity: 5.0,
            ..base()
        });
        let at_max = score(&Candidate {
            goal_proximity: 1.0,
            ..base()
        });
        assert_eq!(
            over, at_max,
            "goal_proximity > 1.0 must clamp to the score at 1.0"
        );

        let under = score(&Candidate {
            goal_proximity: -1.0,
            ..base()
        });
        let at_min = score(&Candidate {
            goal_proximity: 0.0,
            ..base()
        });
        assert_eq!(
            under, at_min,
            "goal_proximity < 0.0 must clamp to the score at 0.0"
        );
        assert!(under >= 0.0, "clamped score must never go negative");
    }
}
