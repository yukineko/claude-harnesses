//! PreToolUse output helper: build the single-line JSON that tells Claude Code
//! how to treat a tool call.

use crate::model::Decision;

/// Serialize a PreToolUse `deny` decision to the one-line JSON Claude Code reads
/// from a hook's stdout.
pub fn deny_json(reason: &str) -> String {
    decision_json("deny", reason)
}

/// Serialize a PreToolUse `ask` decision — put the call to the human.
///
/// Only emit this when a human is actually present to answer; see
/// [`crate::interactive::ask_available`]. An `ask` nobody can answer does not
/// pause, it blocks.
pub fn ask_json(reason: &str) -> String {
    decision_json("ask", reason)
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

/// The stdout line for a decision, or `None` for `Allow` (which prints nothing).
///
/// Returning `Option` rather than an empty string keeps "print nothing" a
/// distinct, un-ignorable case at the call site: an `Ask` must never fall
/// through to the silent branch, because silence IS an allow.
pub fn decision_line(decision: &Decision) -> Option<String> {
    match decision {
        Decision::Allow => None,
        Decision::Deny(reason) => Some(deny_json(reason)),
        Decision::Ask(reason) => Some(ask_json(reason)),
    }
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
        // Must be a single line so it is a valid one-shot hook payload.
        assert!(!s.contains('\n'));
    }

    #[test]
    fn ask_json_has_required_shape() {
        let s = ask_json("cannot analyse");
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["hookSpecificOutput"]["hookEventName"], "PreToolUse");
        assert_eq!(v["hookSpecificOutput"]["permissionDecision"], "ask");
        assert_eq!(
            v["hookSpecificOutput"]["permissionDecisionReason"],
            "cannot analyse"
        );
        assert!(!s.contains('\n'));
    }

    #[test]
    fn decision_line_is_silent_only_for_allow() {
        // The regression this pins: an Ask that produced no line would be a
        // silent exit 0, which Claude Code reads as an allow — the exact
        // fail-open Ask exists to close.
        assert_eq!(decision_line(&Decision::Allow), None);
        assert!(decision_line(&Decision::deny("d")).is_some());
        assert!(decision_line(&Decision::ask("a")).is_some());
    }

    #[test]
    fn decision_line_emits_the_matching_permission_decision() {
        for (decision, want) in [(Decision::deny("r"), "deny"), (Decision::ask("r"), "ask")] {
            let line = decision_line(&decision).expect("non-allow must print");
            let v: serde_json::Value = serde_json::from_str(&line).unwrap();
            assert_eq!(v["hookSpecificOutput"]["permissionDecision"], want);
        }
    }
}
