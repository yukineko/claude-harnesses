//! Session-wide concurrency ceiling — how many units of work one Claude session
//! may have in flight at once.
//!
//! Every fan-out in this harness (condukt's parallel batches, its consensus and
//! adversarial panels, the skill-driven subagent sweeps in scout / specguard /
//! continuous-audit) spends the SAME budget: one session's concurrent workers.
//! Before this module each site carried its own ceiling (`max_parallel = 4`,
//! `MAX_SAMPLES = 5`, `MAX_PANEL = 5`, "5 レンズを1メッセージで並列起動"), so the
//! real per-session width was the *sum* of whatever happened to fire — a number
//! no single call site could see, let alone bound.
//!
//! [`SESSION_MAX_PARALLEL`] is that missing number. It is a **ceiling, not a
//! default**: configuration and environment may lower it, never raise it. A
//! call site that wants N workers asks [`cap_fanout`] and gets what it is
//! actually allowed to spawn.
//!
//! **Undetermined resolves to the ceiling, and that is the restrictive answer
//! here** (CLAUDE.md 3): the invariant this module defends is "never more than
//! [`SESSION_MAX_PARALLEL`] at once". An unparsable override cannot raise the
//! cap — [`clamp`] bounds every path above — so falling back to the ceiling
//! keeps the guarantee intact, while an arbitrary drop to 1 would silently
//! serialize a whole run on a typo.

/// Maximum concurrent units of work per Claude session. **A ceiling.** Config
/// and env may lower it; nothing raises it.
pub const SESSION_MAX_PARALLEL: usize = 3;

/// Environment variable that may LOWER the session cap (values above the
/// ceiling are clamped down, not honored).
pub const ENV_OVERRIDE: &str = "HARNESS_MAX_PARALLEL";

/// Bound `requested` into `[1, SESSION_MAX_PARALLEL]`.
///
/// The floor of 1 is deliberate: a cap of 0 is not "more restrictive", it is a
/// scheduler that can never place a task. Callers chunk by this value, so 0
/// would either stall the run or panic on a zero-sized chunk.
#[must_use]
pub fn clamp(requested: usize) -> usize {
    requested.clamp(1, SESSION_MAX_PARALLEL)
}

/// The cap in force for this process: [`SESSION_MAX_PARALLEL`], lowered by
/// [`ENV_OVERRIDE`] when it parses to something smaller. Unset / unparsable /
/// too-large values all resolve to the ceiling (see the module docs).
#[must_use]
pub fn session_cap() -> usize {
    match std::env::var(ENV_OVERRIDE) {
        Ok(raw) => match raw.trim().parse::<usize>() {
            Ok(n) => clamp(n),
            Err(_) => SESSION_MAX_PARALLEL,
        },
        Err(_) => SESSION_MAX_PARALLEL,
    }
}

/// How many workers a call site asking for `requested` may actually spawn:
/// `min(requested, session_cap())`, never below 1.
#[must_use]
pub fn cap_fanout(requested: usize) -> usize {
    clamp(requested.min(session_cap()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ceiling_is_three() {
        // The number the whole harness is bounded by. Changing it is a policy
        // change, not a refactor — this test makes that explicit.
        assert_eq!(SESSION_MAX_PARALLEL, 3);
    }

    #[test]
    fn clamp_never_exceeds_the_ceiling() {
        for n in 0..64 {
            let c = clamp(n);
            assert!(
                c <= SESSION_MAX_PARALLEL,
                "clamp({n}) = {c} exceeded the ceiling"
            );
        }
        assert_eq!(clamp(99), SESSION_MAX_PARALLEL);
        assert_eq!(clamp(2), 2);
    }

    #[test]
    fn clamp_floors_at_one_so_a_scheduler_can_always_place_work() {
        // 0 is not "safer"; it is a cap that can never run anything.
        assert_eq!(clamp(0), 1);
    }

    #[test]
    fn cap_fanout_lowers_but_never_raises_a_request() {
        // A caller asking for less than the cap keeps its own smaller number.
        assert_eq!(cap_fanout(1), 1);
        assert!(cap_fanout(99) <= SESSION_MAX_PARALLEL);
    }
}
