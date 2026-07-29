//! PreToolUse output helper: the single-line JSON `gate` prints to tell
//! Claude Code how to treat a tool call. Same shape as
//! `blastguard::hookio::{ask_json, deny_json}` — Claude Code reads this
//! `hookSpecificOutput.permissionDecision` shape regardless of which hook
//! emitted it.

/// Serialize a PreToolUse `deny` decision.
pub fn deny_json(reason: &str) -> String {
    decision_json("deny", reason)
}

/// Serialize a PreToolUse `ask` decision. Only emit this when
/// [`crate::interactive::ask_available`] is true — an `ask` nobody can answer
/// does not pause, it blocks.
pub fn ask_json(reason: &str) -> String {
    decision_json("ask", reason)
}

/// Serialize a PreToolUse output that carries **only** `additionalContext`: an
/// advisory warning injected into the turn's context, expressing no
/// `permissionDecision` at all.
///
/// Used by observe-only mode ([`crate::observe`]). Emitting no
/// `permissionDecision` is the point: the normal permission flow — the user's
/// own rules and every other hook's verdict — is left completely untouched,
/// and taintguard contributes information instead of a decision.
///
/// This is deliberately NOT `permissionDecision: "allow"`. An explicit `allow`
/// is a positive verdict that would *override* the remaining permission checks
/// and other gates' answers, so a mode whose entire purpose is to stop
/// enforcing would end up enforcing something far broader than what it
/// suppressed. `additionalContext` alone cannot do that.
pub fn context_json(context: &str) -> String {
    serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "additionalContext": context,
        }
    })
    .to_string()
}

fn decision_json(decision: &str, reason: &str) -> String {
    serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": decision,
            "permissionDecisionReason": reason,
        }
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deny_json_has_required_shape() {
        let s = deny_json("boom");
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["hookSpecificOutput"]["hookEventName"], "PreToolUse");
        assert_eq!(v["hookSpecificOutput"]["permissionDecision"], "deny");
        assert_eq!(v["hookSpecificOutput"]["permissionDecisionReason"], "boom");
        assert!(!s.contains('\n'));
    }

    #[test]
    fn ask_json_has_required_shape() {
        let s = ask_json("tainted");
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["hookSpecificOutput"]["permissionDecision"], "ask");
        assert_eq!(
            v["hookSpecificOutput"]["permissionDecisionReason"],
            "tainted"
        );
        assert!(!s.contains('\n'));
    }
}
