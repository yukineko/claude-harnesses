//! The `condukt testscope` (PreToolUse) and `condukt testscope-confirm`
//! (PostToolUse / PostToolUseFailure) hook entry points; the judgments live in
//! `testscope.rs`.

use std::path::{Path, PathBuf};

use crate::{testscope, try_read_stdin};

/// PreToolUse(Bash) test-scope gate (see `testscope.rs`). Writes the
/// per-session marker `<ledger parent>/sessions/<session_id>` whenever the
/// payload carries a session id (what `state set --status running` requires),
/// then judges the command and prints a `permissionDecision: deny` JSON or
/// nothing. An unreadable / unparseable payload, a Bash payload without a
/// command, an undeterminable cwd and an unset `$HOME` (no ledger location)
/// deny. Called under `harness_core::hook::run_pretooluse_guarded`: a panic
/// exits 2 (fail-closed).
pub(crate) fn run_testscope() {
    // An unreadable payload is undetermined: which command is about to run is
    // unknown, so deny (CLAUDE.md §3).
    let deny = |reason: String| {
        let out = serde_json::json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "deny",
                "permissionDecisionReason": reason,
            }
        });
        println!("{out}");
    };
    let raw = match try_read_stdin() {
        Ok(raw) => raw,
        Err(e) => {
            deny(format!(
                "condukt testscope could not read the PreToolUse payload: {e}"
            ));
            return;
        }
    };
    let v: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(e) => {
            deny(format!(
                "condukt testscope could not parse the PreToolUse payload: {e}"
            ));
            return;
        }
    };
    if v.get("tool_name").and_then(|t| t.as_str()) != Some("Bash") {
        return;
    }
    let Some(command) = v.pointer("/tool_input/command").and_then(|c| c.as_str()) else {
        deny("condukt testscope: Bash payload carries no tool_input.command".to_string());
        return;
    };
    let cwd = match v.get("cwd").and_then(|c| c.as_str()) {
        Some(c) => std::path::PathBuf::from(c),
        None => match std::env::current_dir() {
            Ok(d) => d,
            Err(e) => {
                deny(format!("condukt testscope cannot determine the cwd: {e}"));
                return;
            }
        },
    };
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    let Some(ledger) = testscope::ledger_path(home.as_deref()) else {
        deny("condukt testscope: HOME is unset, so the rerun ledger has no location".to_string());
        return;
    };
    // Rule A: record that this hook is loaded in this session, so `condukt
    // state set --status running` can refuse a session that never loaded it. A
    // marker that cannot be written leaves the session unmarked, which is the
    // restricted side (its `state set running` is refused); it is named on
    // stderr, and this command is still judged below.
    if let Some(sid) = v.get("session_id").and_then(|s| s.as_str()) {
        if let Err(e) = testscope::write_session_marker(&ledger, sid) {
            eprintln!(
                "condukt testscope: could not write the session marker for '{sid}' ({e}); \
                 `condukt state set --status running` will be refused in this session"
            );
        }
    }
    let tool_use_id = v
        .get("tool_use_id")
        .and_then(|t| t.as_str())
        .filter(|t| !t.is_empty());
    match testscope::judge(command, &cwd, home.as_deref(), &ledger, tool_use_id) {
        testscope::Decision::Allow => {}
        testscope::Decision::Deny(reason) => deny(reason),
        testscope::Decision::Ask(reason) => {
            let out = serde_json::json!({
                "hookSpecificOutput": {
                    "hookEventName": "PreToolUse",
                    "permissionDecision": "ask",
                    "permissionDecisionReason": reason,
                }
            });
            println!("{out}");
        }
    }
}

/// Print a `systemMessage` (the PostToolUse channel that never blocks).
fn testscope_confirm_message(msg: &str) {
    println!("{}", serde_json::json!({ "systemMessage": msg }));
}

/// Run [`run_testscope_confirm`] so that nothing it does can block the call.
/// `harness_core::hook::run_posttooluse_guarded` resolves a panic to
/// `{"decision":"block"}`; a PostToolUse cannot stop a call that already ran,
/// so the unrecorded confirmation is surfaced as a `systemMessage` instead and
/// the exit status stays 0. The restricted side is kept by the ledger: the
/// pending row stays pending, so a rerun asks.
pub(crate) fn run_testscope_confirm_guarded() {
    let outcome = std::panic::catch_unwind(run_testscope_confirm);
    if let Err(payload) = outcome {
        let why = payload
            .downcast_ref::<&str>()
            .map(|s| (*s).to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "(non-string panic payload)".to_string());
        testscope_confirm_message(&format!(
            "condukt testscope-confirm crashed (panic: {why}); this test run's confirmation was \
             NOT recorded, so a rerun of it will ask instead of being refused"
        ));
    }
}

/// PostToolUse / PostToolUseFailure(Bash): see [`testscope::confirm_post`].
/// Silent for a non-Bash call, a Bash call without a test command, and a
/// successful confirmation. Unreadable payload, unknown cwd, unset `$HOME` and
/// ledger IO failures print a `systemMessage` that the confirmation was not
/// recorded. Never blocks.
fn run_testscope_confirm() {
    let raw = match try_read_stdin() {
        Ok(raw) => raw,
        Err(e) => {
            testscope_confirm_message(&format!(
                "condukt testscope-confirm could not read the hook payload ({e}); no test run \
                 confirmation was recorded"
            ));
            return;
        }
    };
    let v: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(e) => {
            testscope_confirm_message(&format!(
                "condukt testscope-confirm could not parse the hook payload ({e}); no test run \
                 confirmation was recorded"
            ));
            return;
        }
    };
    if v.get("tool_name").and_then(|t| t.as_str()) != Some("Bash") {
        return;
    }
    let Some(command) = v.pointer("/tool_input/command").and_then(|c| c.as_str()) else {
        return;
    };
    let cwd = match v.get("cwd").and_then(|c| c.as_str()) {
        Some(c) => std::path::PathBuf::from(c),
        None => match std::env::current_dir() {
            Ok(d) => d,
            Err(e) => {
                testscope_confirm_message(&format!(
                    "condukt testscope-confirm cannot determine the cwd ({e}); no test run \
                     confirmation was recorded"
                ));
                return;
            }
        },
    };
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    let Some(ledger) = testscope::ledger_path(home.as_deref()) else {
        testscope_confirm_message(
            "condukt testscope-confirm: HOME is unset, so the rerun ledger has no location; no \
             test run confirmation was recorded",
        );
        return;
    };
    let tool_use_id = v
        .get("tool_use_id")
        .and_then(|t| t.as_str())
        .filter(|t| !t.is_empty());
    if let Err(e) = testscope::confirm_post(command, &cwd, home.as_deref(), &ledger, tool_use_id) {
        testscope_confirm_message(&format!(
            "condukt testscope-confirm: the confirmation of this test run was NOT recorded \
             ({e}); its row stays pending, so a rerun will ask instead of being refused"
        ));
    }
}

// ── session marker (rule A; re-exported by `testscope`) ─────────────────────

/// `<ledger parent>/sessions/<safe session id>`: the file whose existence says
/// "the testscope hook ran in this session". `None` when the ledger path has no
/// parent or the session id is empty.
pub fn session_marker_path(ledger: &Path, session_id: &str) -> Option<PathBuf> {
    if session_id.trim().is_empty() {
        return None;
    }
    let dir = ledger.parent()?;
    Some(
        dir.join("sessions")
            .join(harness_core::store::safe_session(session_id)),
    )
}

/// Record that the testscope hook is loaded in `session_id`. Called by the hook
/// on every payload that carries a session id; `condukt state set --status
/// running` refuses a session that has no marker ([`session_marker_present`]).
pub fn write_session_marker(ledger: &Path, session_id: &str) -> std::io::Result<()> {
    let Some(p) = session_marker_path(ledger, session_id) else {
        return Ok(());
    };
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&p, b"")
}

/// Whether the marker of `session_id` exists, as an observation: `Err` (never
/// `false`, never `true`) when existence cannot be told.
pub fn session_marker_present(ledger: &Path, session_id: &str) -> Result<bool, String> {
    let Some(p) = session_marker_path(ledger, session_id) else {
        return Ok(false);
    };
    p.try_exists()
        .map_err(|e| format!("cannot tell whether {} exists: {e}", p.display()))
}
