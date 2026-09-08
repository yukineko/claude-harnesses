//! Session-wide concurrency ceiling — how many units of work one Claude session
//! may have in flight at once.
//!
//! The number lives here rather than in the crate that enforces it because it
//! is a policy about a session, not about any one plugin, and because the thing
//! it bounds (a frozen WSL2 host) is a property of the machine.
//!
//! **Who enforces it.** Exactly one consumer: `parallelguard`, whose PreToolUse
//! hook counts the `Bash` calls and subagents actually in flight and denies the
//! call that would exceed the cap. Enforcement deliberately does NOT live in
//! the planners (condukt's scheduler, the fan-out sentences in the skills):
//! a planner can only bound what it plans, and the number that freezes a
//! machine is the number of processes *running*, which no planner can see. A
//! plan-side cap was tried and removed (`483a0d6e`, reverted) — it made batches
//! narrower without ever being able to say no.
//!
//! [`SESSION_MAX_PARALLEL`] is a **ceiling, not a default**: configuration and
//! environment may lower it, never raise it. A call site asking how wide it may
//! go calls [`session_cap`].
//!
//! **Undetermined resolves to the ceiling, and that is the restrictive answer
//! here** (CLAUDE.md 3): the invariant this module defends is "never more than
//! [`SESSION_MAX_PARALLEL`] at once". An unparsable override cannot raise the
//! cap — [`clamp`] bounds every path above — so falling back to the ceiling
//! keeps the guarantee intact, while an arbitrary drop to 1 would silently
//! serialize a whole session on a typo.

/// Maximum concurrent units of work per Claude session, per pool. **A ceiling.**
/// Config and env may lower it; nothing raises it.
pub const SESSION_MAX_PARALLEL: usize = 3;

/// Environment variable that may LOWER the session cap (values above the
/// ceiling are clamped down, not honored).
pub const ENV_OVERRIDE: &str = "HARNESS_MAX_PARALLEL";

/// Bound `requested` into `[1, SESSION_MAX_PARALLEL]`.
///
/// The floor of 1 is deliberate: a cap of 0 is not "more restrictive", it is a
/// session in which no shell may ever run — unrecoverable, and not what any
/// operator lowering a concurrency limit is asking for.
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
    fn clamp_floors_at_one_so_a_session_can_always_run_something() {
        // 0 is not "safer"; it is a session that can never run a shell.
        assert_eq!(clamp(0), 1);
    }
}
