//! modelguard — PreToolUse gate on `Agent|Task` subagent spawns.
//!
//! User ruling 2026-11-27, verbatim:
//! 「fableの使用は禁止、実装担当は今後つくるagentではsonnetにして、これはhookで強制して」
//!
//! `modelguard gate` reads the hook payload and denies (JSON on stdout, exit 0)
//! when:
//! * Rule A (every spawn): `tool_input.model`, lowercased, contains "fable" or
//!   "mythos"; or `model` is present but not a string (including null); or the
//!   payload is not JSON / `tool_input` is missing or not an object.
//! * Rule B (implementers): `subagent_type` is not in the fixed control
//!   allowlist (a missing, empty or unknown type is an implementer) and the
//!   model is neither "sonnet" (case-insensitive) nor starting with
//!   "claude-sonnet-". An absent model is therefore denied for implementers.
//! * `subagent_type` "fork" is always denied: a fork ignores `model` and
//!   inherits the parent model, which cannot be determined from the payload.
//!
//! Otherwise nothing is printed (allow). The gate runs under
//! `run_pretooluse_guarded`, so a panic is not turned into an allow.

use serde_json::Value;

/// Subagent types that are controls (read-only / verification roles); they are
/// exempt from Rule B only. Rule A still applies to them.
const CONTROLS: [&str; 11] = [
    "Explore",
    "Plan",
    "condukt:condukt-verifier",
    "condukt:condukt-researcher",
    "condukt:condukt-interpreter",
    "specguard:specguard-auditor",
    "reviewgate:reviewgate-typing",
    "ctxrot:ctxrot-distiller",
    "claude-code-guide",
    "web-fetch",
    "statusline-setup",
];

fn deny(reason: &str) {
    let out = serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": reason,
        }
    });
    println!("{out}");
}

fn is_sonnet(model: &str) -> bool {
    let m = model.to_lowercase();
    m == "sonnet" || m.starts_with("claude-sonnet-")
}

/// Ok(()) = allow; Err(reason) = deny.
fn judge(raw: &str) -> Result<(), String> {
    let v: Value = serde_json::from_str(raw.trim()).map_err(|_| {
        "[modelguard] hook payload unparseable: cannot judge this spawn's model, so it is denied (fail-closed).".to_string()
    })?;
    let input = v.get("tool_input").filter(|i| i.is_object()).ok_or_else(|| {
        "[modelguard] tool_input missing or not an object: cannot judge this spawn, so it is denied (fail-closed).".to_string()
    })?;
    let model: Option<&str> = match input.get("model") {
        None => None,
        Some(Value::String(s)) => Some(s),
        Some(_) => {
            return Err("[modelguard] `model` is present but not a string: cannot judge this spawn, so it is denied (fail-closed). Pass model: \"sonnet\" for implementers.".to_string());
        }
    };
    if let Some(m) = model {
        let lower = m.to_lowercase();
        if lower.contains("fable") || lower.contains("mythos") {
            return Err(format!(
                "[modelguard] model {m:?} is forbidden (Fable/Mythos are banned, user ruling 2026-11-27). Implementers must use sonnet: pass model: \"sonnet\"."
            ));
        }
    }
    // A missing, null or non-string type cannot be shown to be a control, so
    // it is treated as an implementer.
    let ty = input
        .get("subagent_type")
        .and_then(Value::as_str)
        .unwrap_or("");
    // A fork ignores `model` and always inherits the parent model, so its real
    // model cannot be determined from the payload: undetermined denies.
    if ty == "fork" {
        return Err("[modelguard] subagent_type \"fork\" always inherits the parent model and ignores `model`, which may be Fable or non-sonnet, so its model cannot be verified and it is denied (fail-closed). Spawn a typed agent with model: \"sonnet\" instead.".to_string());
    }
    if CONTROLS.contains(&ty) {
        return Ok(());
    }
    match model {
        Some(m) if is_sonnet(m) => Ok(()),
        _ => Err(format!(
            "[modelguard] implementer spawn (subagent_type {ty:?}) must use a Sonnet model (user ruling 2026-11-27); got {:?}. Pass model: \"sonnet\".",
            model.unwrap_or("<none>")
        )),
    }
}

fn gate() {
    let raw = harness_core::hook::read_stdin();
    if let Err(reason) = judge(&raw) {
        deny(&reason);
    }
}

fn main() {
    let sub = std::env::args().nth(1).unwrap_or_default();
    match sub.as_str() {
        "gate" => harness_core::hook::run_pretooluse_guarded("modelguard", gate),
        _ => {
            eprintln!("usage: modelguard gate");
            std::process::exit(2);
        }
    }
}
