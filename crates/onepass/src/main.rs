//! onepass — at most ONE refutation/verification/audit subagent spawn per
//! user prompt.
//!
//! * `onepass gate` (PreToolUse, matcher `Agent|Task`): a spawn whose
//!   `description` or `subagent_type` matches (case-insensitive)
//!   refut|verif|audit|adversar|反証|検証|監査 takes this prompt's single slot;
//!   a second one is denied. Non-matching spawns produce no output.
//!   Spawns whose `subagent_type` starts with `condukt:` are exempt: never
//!   denied and never counted.
//! * `onepass reset` (UserPromptSubmit): frees the session's slot.
//!
//! The slot is a per-session marker file taken with `create_new`, so two
//! concurrent spawns cannot both take it. Anything that prevents judging a
//! matching spawn (unparseable payload, missing session id, state that cannot
//! be created) denies it (CLAUDE.md §3).

use std::fs::OpenOptions;
use std::io::ErrorKind;
use std::path::PathBuf;

use serde_json::Value;

const RULE: &str =
    "「1度の反証とテスト、証明できなければ破棄、テストがREDなら起票」「何段階も深くほらない」";

const NEEDLES: [&str; 7] = [
    "refut", "verif", "audit", "adversar", "反証", "検証", "監査",
];

fn is_refutation(s: &str) -> bool {
    let lower = s.to_lowercase();
    NEEDLES.iter().any(|n| lower.contains(n))
}

fn state_dir() -> PathBuf {
    match std::env::var_os("ONEPASS_STATE_DIR") {
        Some(d) if !d.is_empty() => PathBuf::from(d),
        _ => harness_core::config::base_dir("onepass"),
    }
}

/// A filesystem-safe session id, or None when absent / not safe to use as a
/// file name (cannot key the counter).
fn session_id(v: &Value) -> Option<&str> {
    let sid = v.get("session_id")?.as_str()?;
    let ok = !sid.is_empty()
        && sid
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    ok.then_some(sid)
}

fn marker(sid: &str) -> PathBuf {
    state_dir().join(format!("{sid}.spent"))
}

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

/// A string field: absent/null reads as "", a non-string is undeterminable.
fn text_field<'a>(input: &'a Value, key: &str) -> Option<&'a str> {
    match input.get(key) {
        None | Some(Value::Null) => Some(""),
        Some(Value::String(s)) => Some(s),
        Some(_) => None,
    }
}

fn gate() {
    let raw = harness_core::hook::read_stdin();
    let Ok(v) = serde_json::from_str::<Value>(raw.trim()) else {
        deny("[onepass] hook payload unparseable: cannot tell whether this spawn is a refutation/verification agent, so it is denied (fail-closed).");
        return;
    };
    let Some(input) = v.get("tool_input").filter(|i| i.is_object()) else {
        deny("[onepass] tool_input missing or not an object: cannot judge this spawn, so it is denied (fail-closed).");
        return;
    };
    let (Some(desc), Some(ty)) = (
        text_field(input, "description"),
        text_field(input, "subagent_type"),
    ) else {
        deny("[onepass] description/subagent_type is not a string: cannot judge this spawn, so it is denied (fail-closed).");
        return;
    };
    // condukt's own roles (researcher, interpreter, worker, verifier) are the
    // orchestrator's pipeline, not an extra refutation layer over a finding:
    // they are never denied and never take the slot (user ruling 2026-11-01).
    if ty.starts_with("condukt:") {
        return;
    }
    if !is_refutation(desc) && !is_refutation(ty) {
        return;
    }
    let Some(sid) = session_id(&v) else {
        deny("[onepass] no usable session_id: the per-prompt refutation count cannot be kept, so this refutation/verification spawn is denied (fail-closed).");
        return;
    };
    let dir = state_dir();
    if let Err(e) = std::fs::create_dir_all(&dir) {
        deny(&format!(
            "[onepass] state dir {} unusable ({e}): cannot count refutation spawns, so this one is denied (fail-closed).",
            dir.display()
        ));
        return;
    }
    let path = marker(sid);
    match OpenOptions::new().write(true).create_new(true).open(&path) {
        Ok(_) => {}
        Err(e) if e.kind() == ErrorKind::AlreadyExists => deny(&format!(
            "[onepass] a refutation/verification/audit agent was already spawned for this prompt. Rule (2026-10-17): {RULE}. One refutation and one test per finding: if it cannot be proven, discard it; if the test is RED, file it. Do not spawn another verification layer."
        )),
        Err(e) => deny(&format!(
            "[onepass] state {} unwritable ({e}): cannot count refutation spawns, so this one is denied (fail-closed).",
            path.display()
        )),
    }
}

fn reset() {
    let raw = harness_core::hook::read_stdin();
    let sid = serde_json::from_str::<Value>(raw.trim())
        .ok()
        .and_then(|v| session_id(&v).map(str::to_string));
    let Some(sid) = sid else {
        eprintln!("[onepass] reset: no usable session_id; nothing reset");
        return;
    };
    let path = marker(&sid);
    match std::fs::remove_file(&path) {
        Ok(()) => {}
        Err(e) if e.kind() == ErrorKind::NotFound => {}
        Err(e) => {
            let out = serde_json::json!({
                "hookSpecificOutput": {
                    "hookEventName": "UserPromptSubmit",
                    "additionalContext": format!(
                        "[onepass] could not reset the refutation counter ({}: {e}); refutation/verification agent spawns stay denied this prompt.",
                        path.display()
                    ),
                }
            });
            println!("{out}");
        }
    }
}

fn main() {
    let sub = std::env::args().nth(1).unwrap_or_default();
    match sub.as_str() {
        "gate" => harness_core::hook::run_pretooluse_guarded("onepass", gate),
        // A failed or panicking reset leaves the marker in place, which only
        // makes `gate` deny: the restrictive side.
        "reset" => harness_core::hook::run_hook(reset),
        _ => {
            eprintln!("usage: onepass gate|reset");
            std::process::exit(2);
        }
    }
}
