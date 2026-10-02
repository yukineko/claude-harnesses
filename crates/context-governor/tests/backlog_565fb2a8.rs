#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Repro for backlog 565fb2a8: `session_start_on_compact_source_exits_zero`
//! (tests/context_governor_coexistence_with_ctxrot.rs) is docstringed "this is
//! exactly where CG's rehydrator is *most* active", but it invokes the binary
//! DIRECTLY (bypassing the hooks.json matcher) against an EMPTY store and
//! asserts only exit 0 + a valid envelope. It cannot fail when the matcher
//! excludes `compact`, nor when rehydration emits nothing.
//!
//! This is the test that docstring promises: a real PreCompact snapshot of a
//! transcript carrying a normative section, then a SessionStart(compact) that
//! is ROUTED THROUGH THE SHIPPED MATCHER (Claude Code only spawns the hook when
//! the matcher accepts the source), and an assertion on the re-injected pin.
//!
//! The control proves the binary half works when invoked directly, so the RED
//! is attributable to the routing — the exact gap the vacuous test hides.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

const SESSION: &str = "backlog-565fb2a8";

fn run_cg(payload: &serde_json::Value, state: &Path) -> String {
    let mut child = Command::new(env!("CARGO_BIN_EXE_context-governor"))
        .env("CONTEXT_GOVERNOR_STATE_DIR", state)
        .env("CLAUDE_CODE_SESSION_ID", SESSION)
        .env("HOME", state)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn context-governor");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.to_string().as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "exit {:?} stderr={}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// The SessionStart matchers Claude Code applies, from the shipped hooks.json.
fn session_start_matchers() -> Vec<String> {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("hooks/hooks.json");
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(p).expect("hooks.json")).expect("json");
    v["hooks"]["SessionStart"]
        .as_array()
        .expect("SessionStart registered")
        .iter()
        .map(|g| g["matcher"].as_str().unwrap_or("").to_string())
        .collect()
}

fn matcher_routes(source: &str) -> bool {
    session_start_matchers()
        .iter()
        .any(|m| m.is_empty() || m == "*" || m.split('|').any(|alt| alt == source))
}

/// Snapshot a transcript with a normative section via PreCompact, then return
/// the SessionStart(compact) stdout — or None when the shipped matcher would
/// never have spawned the hook.
fn compact_cycle(routed: bool) -> Option<String> {
    let td = tempfile::tempdir().unwrap();
    let state = td.path().join("state");
    let transcript = td.path().join("t.jsonl");
    let line = serde_json::json!({
        "type": "user",
        "message": {"role": "user",
                    "content": "# Acceptance Criteria\nEvery request MUST be authenticated.\n"}
    });
    std::fs::write(&transcript, format!("{line}\n")).unwrap();
    let cwd = td.path().to_str().unwrap();

    run_cg(
        &serde_json::json!({
            "session_id": SESSION, "transcript_path": transcript.to_str().unwrap(),
            "cwd": cwd, "hook_event_name": "PreCompact", "trigger": "auto",
        }),
        &state,
    );
    if routed && !matcher_routes("compact") {
        return None;
    }
    Some(run_cg(
        &serde_json::json!({
            "session_id": SESSION, "transcript_path": transcript.to_str().unwrap(),
            "cwd": cwd, "hook_event_name": "SessionStart", "source": "compact",
        }),
        &state,
    ))
}

/// Control: invoked directly, the rehydrator re-injects the pinned norm. So a
/// RED below is about routing, not about the snapshot/rehydrate plumbing.
#[test]
fn control_direct_invocation_rehydrates_the_pin() {
    let out = compact_cycle(false).unwrap();
    assert!(
        out.contains("[pinned]") && out.contains("authenticated"),
        "direct SessionStart(compact) did not rehydrate: {out}"
    );
}

#[test]
#[ignore = "backlog 565fb2a8: open defect, remove ignore when fixed"]
fn compact_session_start_through_shipped_matcher_rehydrates_the_pin() {
    match compact_cycle(true) {
        None => panic!(
            "SessionStart matcher(s) {:?} never route source=compact to context-governor, \
             so the pin is not rehydrated after compaction",
            session_start_matchers()
        ),
        Some(out) => assert!(
            out.contains("[pinned]") && out.contains("authenticated"),
            "routed SessionStart(compact) did not rehydrate the pin: {out}"
        ),
    }
}
