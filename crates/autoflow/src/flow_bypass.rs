//! UserPromptSubmit verdict: a `/flow` prompt is only accepted while the
//! session is in `bypassPermissions` mode (memory flow-bypass-permissions-mode;
//! user 2026-08-12 「flowのときは常にそうして」). This used to rest on the model
//! remembering to ask for Shift+Tab; it is now a machine check.
//!
//! This is a verdict (it blocks prompts), so per CLAUDE.md §3 every "cannot
//! determine" resolves to block:
//!   * the payload could not be read or parsed — whether the prompt is `/flow`
//!     is unknown, so the prompt is blocked;
//!   * the prompt is `/flow` but `permission_mode` is absent or not a string —
//!     the mode is unknown, so the prompt is blocked.
//!
//! Only a positive observation passes: a prompt that is observably not `/flow`,
//! or a `/flow` prompt whose `permission_mode` is exactly `bypassPermissions`.
//!
//! Downstream consumer of the block: Claude Code's UserPromptSubmit protocol
//! (`{"decision":"block","reason":...}` on stdout, exit 0) — it erases the
//! prompt and shows `reason` to the user.

/// The only permission mode in which `/flow` may run.
const BYPASS_MODE: &str = "bypassPermissions";

/// Whether `prompt` invokes the `/flow` skill: its first whitespace-delimited
/// token is `/flow` or a plugin-namespaced `/flow:<skill>`. `/flowchart` and a
/// `/flow` that is not the leading token are not invocations.
pub(crate) fn is_flow_invocation(prompt: &str) -> bool {
    let first = prompt.split_whitespace().next().unwrap_or("");
    first == "/flow" || first.starts_with("/flow:")
}

/// The block reason for a `/flow` prompt in mode `mode` (`None` = unknown).
fn not_bypass_reason(mode: Option<&str>) -> String {
    let observed = match mode {
        Some(m) => format!("現在の permission_mode は {m:?} です"),
        None => "permission_mode が hook payload に無く、現在のモードを判定できません（判定不能）"
            .to_string(),
    };
    format!(
        "autoflow: /flow は Bypass permissions モードでのみ実行します（{observed}）。\n\
         Shift+Tab で bypassPermissions に切り替えてから、もう一度 /flow を送ってください。"
    )
}

/// Decide a UserPromptSubmit payload. `Some(reason)` = block with `reason`;
/// `None` = not a `/flow` prompt, or `/flow` under `bypassPermissions` (pass).
/// There is no separate error arm: an unparseable payload is itself a block
/// (see the module doc).
pub(crate) fn decide(raw: &str) -> Option<String> {
    let value: serde_json::Value = match serde_json::from_str(raw.trim()) {
        Ok(v @ serde_json::Value::Object(_)) => v,
        Ok(_) | Err(_) => {
            return Some(
                "autoflow: UserPromptSubmit の hook payload を解釈できず、このプロンプトが \
                 /flow かどうかを判定できません（判定不能のためブロック）。"
                    .to_string(),
            )
        }
    };
    // An absent `prompt` is an observed non-/flow prompt (the field is the
    // prompt text; hosts that send no prompt send nothing to invoke /flow
    // with). A present non-string `prompt` is not interpretable.
    let prompt = match value.get("prompt") {
        None | Some(serde_json::Value::Null) => "",
        Some(serde_json::Value::String(s)) => s.as_str(),
        Some(_) => {
            return Some(
                "autoflow: hook payload の prompt が文字列ではなく、このプロンプトが /flow \
                 かどうかを判定できません（判定不能のためブロック）。"
                    .to_string(),
            )
        }
    };
    if !is_flow_invocation(prompt) {
        return None;
    }
    match value.get("permission_mode").and_then(|m| m.as_str()) {
        Some(BYPASS_MODE) => None,
        mode => Some(not_bypass_reason(mode)),
    }
}

/// The UserPromptSubmit block line.
pub(crate) fn block_json(reason: &str) -> serde_json::Value {
    serde_json::json!({ "decision": "block", "reason": reason })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invocation_matching() {
        assert!(is_flow_invocation("/flow"));
        assert!(is_flow_invocation("  /flow 続きを"));
        assert!(is_flow_invocation("/flow:flow"));
        assert!(!is_flow_invocation("/flowchart"));
        assert!(!is_flow_invocation("please run /flow"));
        assert!(!is_flow_invocation(""));
    }

    #[test]
    fn non_string_mode_is_undetermined_and_blocks() {
        let r = decide(r#"{"prompt":"/flow","permission_mode":1}"#).unwrap();
        assert!(r.contains("判定不能"), "got: {r}");
        assert!(r.contains("Shift+Tab"), "got: {r}");
    }

    #[test]
    fn unparseable_payload_blocks() {
        assert!(decide("{ not json").is_some());
        assert!(decide("").is_some());
        assert!(decide("[]").is_some());
        assert!(decide(r#"{"prompt":7}"#).is_some());
    }

    #[test]
    fn non_flow_prompt_without_mode_passes() {
        assert!(decide(r#"{"session_id":"s"}"#).is_none());
        assert!(decide(r#"{"prompt":"hello"}"#).is_none());
    }
}
