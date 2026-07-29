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

/// Serialize the observe-only PreToolUse output: an `additionalContext`-only
/// `hookSpecificOutput` body carrying **no `permissionDecision` at all**, plus a
/// **top-level** `systemMessage` sibling of `hookSpecificOutput`.
///
/// Emitting no `permissionDecision` is the point: the normal permission flow —
/// the user's own rules and every other hook's verdict — is left completely
/// untouched, and taintguard contributes information instead of a decision.
///
/// This is deliberately NOT `permissionDecision: "allow"`. An explicit `allow`
/// is a positive verdict that would *override* the remaining permission checks
/// and other gates' answers, so a mode whose entire purpose is to stop
/// enforcing would end up enforcing something far broader than what it
/// suppressed. An `additionalContext`-plus-`systemMessage` body cannot do that.
///
/// (A `context_json` helper emitting the same body WITHOUT `systemMessage` used
/// to live here. Once `emit_gate` switched to this function it had zero
/// production callers and only a test still exercised it — the same
/// all-call-sites-are-`#[cfg(test)]` defect this release fixes elsewhere — so it
/// was removed rather than left to drift out of sync with what is emitted.)
///
/// Why two fields for one event. Per the Claude Code hooks reference
/// (<https://code.claude.com/docs/en/hooks.md>), `additionalContext` is
/// "injected into Claude's context" as a system reminder and "doesn't appear as
/// a chat message in the interface" — so it is a model-facing channel, not a
/// human-facing one. `systemMessage` is documented there only as a "Warning
/// message shown to the user", which is the closest thing this hook protocol
/// offers to a user-facing channel on a PreToolUse response.
///
/// **`systemMessage` here is best-effort only, and nothing may depend on it.**
/// The docs carry no example pairing `systemMessage` with a PreToolUse response
/// that omits `permissionDecision`, so whether it renders on a non-blocking
/// response like this one is **undocumented** — unverified either way, not
/// verified-absent. The guaranteed human readout of suppressed enforcements is
/// therefore elsewhere and is load-bearing: the durable append-only ledger
/// ([`crate::observe::append`]) and the `taintguard tally` subcommand that reads
/// it. This field is an extra chance at immediacy on top of that, never a
/// substitute for it.
pub fn observe_json(context: &str, system_message: &str) -> String {
    serde_json::json!({
        "systemMessage": system_message,
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
