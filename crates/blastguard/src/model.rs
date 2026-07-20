//! The verdict type produced by the destructive-operation detector.

/// Outcome of inspecting a single tool call.
///
/// Three answers, not two. The two-valued form (`Allow`/`Deny`) forced every
/// construct blastguard cannot analyse into `Allow`, i.e. "I don't understand
/// this" was recorded as "this is fine". That is the fail-open bias this crate
/// exists to prevent, applied to the crate itself. [`Decision::Ask`] is the
/// missing third answer: it is NOT a verdict about the command, it is a refusal
/// to guess about one.
///
/// Ranking, used everywhere sub-analyses are combined: `Deny > Ask > Allow`. A
/// `Deny` found anywhere on a command line must always outrank an `Ask` found
/// anywhere else on it, or the stronger verdict would be silently downgraded.
/// See [`Decision::is_blocking`] and `detect::VerdictAcc`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Let the tool call proceed (blastguard stays silent).
    Allow,
    /// Block the tool call. The string is a short human-facing reason that is
    /// surfaced to the agent as the PreToolUse `permissionDecisionReason`.
    Deny(String),
    /// blastguard could not analyse this construct and refuses to guess: put
    /// the decision to a human. The string is the human-facing reason, surfaced
    /// as the PreToolUse `permissionDecisionReason`.
    ///
    /// An `Ask` is only safe to emit when a human is actually present to answer
    /// it. Every consumer that is not an interactive PreToolUse hook must call
    /// [`Decision::hardened`] to collapse it to a `Deny`.
    Ask(String),
}

impl Decision {
    /// Convenience constructor: `Decision::deny("...")`.
    pub fn deny(reason: impl Into<String>) -> Decision {
        Decision::Deny(reason.into())
    }

    /// Convenience constructor: `Decision::ask("...")`.
    pub fn ask(reason: impl Into<String>) -> Decision {
        Decision::Ask(reason.into())
    }

    pub fn is_deny(&self) -> bool {
        matches!(self, Decision::Deny(_))
    }

    pub fn is_ask(&self) -> bool {
        matches!(self, Decision::Ask(_))
    }

    /// True for any verdict that stops the command from running unreviewed —
    /// `Deny` OR `Ask`.
    ///
    /// This is the predicate sub-analysis loops must use when deciding whether
    /// a nested payload produced something worth propagating. Using
    /// [`Decision::is_deny`] there would drop an `Ask` on the floor and return
    /// `Allow`, re-creating the exact fail-open `Ask` was introduced to close.
    pub fn is_blocking(&self) -> bool {
        matches!(self, Decision::Deny(_) | Decision::Ask(_))
    }

    /// Collapse `Ask` into `Deny`, leaving `Allow` and `Deny` untouched.
    ///
    /// "ask にできないときは fail": an `Ask` that no human can answer must not
    /// become an `Allow`. Callers that run a command with no human in the loop
    /// (specguard's forge, condukt's check runner, daily's task runner — all of
    /// which hand the string to `sh -c`) and the hook itself when the session is
    /// not affirmatively interactive, all funnel through here.
    pub fn hardened(self) -> Decision {
        match self {
            Decision::Ask(reason) => Decision::Deny(reason),
            other => other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_blocking_covers_ask_and_deny_but_not_allow() {
        assert!(Decision::deny("d").is_blocking());
        assert!(Decision::ask("a").is_blocking());
        assert!(!Decision::Allow.is_blocking());
    }

    #[test]
    fn is_deny_does_not_report_true_for_ask() {
        assert!(!Decision::ask("a").is_deny());
        assert!(Decision::ask("a").is_ask());
        assert!(!Decision::deny("d").is_ask());
    }

    #[test]
    fn hardened_turns_ask_into_deny_and_preserves_the_reason() {
        assert_eq!(
            Decision::ask("cannot analyse").hardened(),
            Decision::deny("cannot analyse")
        );
        assert_eq!(Decision::Allow.hardened(), Decision::Allow);
        assert_eq!(Decision::deny("x").hardened(), Decision::deny("x"));
    }
}
