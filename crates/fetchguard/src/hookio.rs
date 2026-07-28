//! PostToolUse output helper: the single-line JSON `scan` prints to inject
//! an untrusted-content warning back into the model's context.
//!
//! Same `hookSpecificOutput` shape `ctxrot::hooks::toolguard` already uses
//! for `PostToolUse` (`hookEventName` + `additionalContext`) — Claude Code
//! reads this shape regardless of which hook emitted it. `additionalContext`
//! (not `updatedToolOutput`) is the right channel here: fetchguard does not
//! want to alter the fetched content itself (that could hide evidence), only
//! to append a warning telling the model how to treat it.

/// Serialize a PostToolUse `additionalContext` warning.
pub fn warning_json(context: &str) -> String {
    serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "PostToolUse",
            "additionalContext": context,
        }
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warning_json_has_required_shape() {
        let s = warning_json("be careful");
        let v: serde_json::Value = serde_json::from_str(&s).expect("warning_json is valid JSON");
        assert_eq!(v["hookSpecificOutput"]["hookEventName"], "PostToolUse");
        assert_eq!(v["hookSpecificOutput"]["additionalContext"], "be careful");
        assert!(!s.contains('\n'));
    }
}
