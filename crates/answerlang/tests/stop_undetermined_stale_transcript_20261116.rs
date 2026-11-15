#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (2026-11-16, backlog c80effc2): answerlang's Stop hook judges the last
//! assistant text read from the transcript FILE, which can be a stale interim
//! text when the final answer is not yet flushed. Intended contract (from the
//! task spec, not from observed Claude Code behaviour):
//! * payload `last_assistant_message` is judged first;
//! * the transcript is the fallback only when the field is absent;
//! * absent field AND the transcript's last assistant text precedes its last
//!   `tool_result` => the final answer is evidently not in the transcript, so
//!   the check is UNDETERMINED and resolves through the bounded block
//!   (`{"decision":"block"}` on the first stop of a chain), never a clean allow;
//! * the block reason names the judged source.
//!
//! What each test proves:
//! 1. `stale_transcript_tool_result_after_last_text_is_undetermined_not_clean`:
//!    with no payload field and a transcript ending in tool_use + tool_result
//!    after a Japanese text (which WOULD pass the language check), the first
//!    stop blocks. Proves the stale-shape is not read as clean.
//! 2. `control_transcript_ending_in_final_text_is_clean`: same Japanese text as
//!    the LAST entry (no later tool_result) is allowed. Proves test 1's block is
//!    caused by the trailing tool_result, not by the text or config.
//! 3a. `payload_english_message_blocks_and_reason_names_last_assistant_message`:
//!    an English payload field blocks even though the transcript is Japanese,
//!    and the reason mentions `last_assistant_message`.
//! 3b. `transcript_fallback_english_blocks_and_reason_names_transcript`: absent
//!    field + English final transcript text blocks with a reason mentioning
//!    `transcript`.
//!
//! DOES NOT PROVE: that real main-session Stop payloads carry
//! `last_assistant_message` (UNVERIFIED, see stop_payload_final_message_20261115.rs);
//! the re-entry (`stop_hook_active: true`) behaviour of the undetermined path;
//! that the undetermined detection keys on "last text precedes last
//! tool_result" rather than some other heuristic that also happens to block
//! this fixture; the exact wording of reasons beyond the substrings checked;
//! sidechain or multi-text-block handling.
mod common;
use common::*;
use serde_json::{json, Value};

fn ja_env() -> Env {
    let env = Env::new();
    env.write_config(JA_CONFIG);
    env
}

fn tool_use(id: &str) -> Value {
    json!({"type":"assistant","message":{"role":"assistant","content":[
        {"type":"tool_use","id":id,"name":"Bash","input":{"command":"ls"}}
    ]}})
}

fn tool_result(id: &str) -> Value {
    json!({"type":"user","message":{"role":"user","content":[
        {"type":"tool_result","tool_use_id":id,"content":"file.txt"}
    ]}})
}

fn stop_payload(t: &std::path::Path, last_message: Option<&str>) -> Value {
    let mut p = json!({
        "session_id": "s1",
        "hook_event_name": "Stop",
        "stop_hook_active": false,
        "transcript_path": t.to_string_lossy(),
    });
    if let Some(m) = last_message {
        p["last_assistant_message"] = json!(m);
    }
    p
}

#[test]
#[ignore = "RED: 2026-11-16 c80effc2 — remove when fixed"]
fn stale_transcript_tool_result_after_last_text_is_undetermined_not_clean() {
    let env = ja_env();
    let t = env.transcript(
        "t.jsonl",
        &[
            user("do the thing"),
            assistant(&ja(300)),
            tool_use("toolu_1"),
            tool_result("toolu_1"),
        ],
    );
    let out = env.run(&["stop"], Some(&stop_payload(&t, None)));
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    assert!(
        out.is_block(),
        "last assistant text precedes the last tool_result and no last_assistant_message: \
         must be undetermined -> block, not clean; stdout: {:?} stderr: {:?}",
        out.stdout,
        out.stderr
    );
}

#[test]
fn control_transcript_ending_in_final_text_is_clean() {
    let env = ja_env();
    let t = env.transcript(
        "t.jsonl",
        &[
            user("do the thing"),
            tool_use("toolu_1"),
            tool_result("toolu_1"),
            assistant(&ja(300)),
        ],
    );
    let out = env.run(&["stop"], Some(&stop_payload(&t, None)));
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    assert!(!out.is_block(), "stdout: {:?}", out.stdout);
}

#[test]
#[ignore = "RED: 2026-11-16 c80effc2 — remove when fixed"]
fn payload_english_message_blocks_and_reason_names_last_assistant_message() {
    let env = ja_env();
    let t = env.answer_transcript("t.jsonl", &ja(300));
    let out = env.run(&["stop"], Some(&stop_payload(&t, Some(&en(200)))));
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    assert!(out.is_block(), "stdout: {:?}", out.stdout);
    assert!(
        out.reason().contains("last_assistant_message"),
        "reason must name the judged source; reason: {:?}",
        out.reason()
    );
}

#[test]
#[ignore = "RED: 2026-11-16 c80effc2 — remove when fixed"]
fn transcript_fallback_english_blocks_and_reason_names_transcript() {
    let env = ja_env();
    let t = env.answer_transcript("t.jsonl", &en(200));
    let out = env.run(&["stop"], Some(&stop_payload(&t, None)));
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    assert!(out.is_block(), "stdout: {:?}", out.stdout);
    assert!(
        out.reason().contains("transcript"),
        "reason must name the judged source; reason: {:?}",
        out.reason()
    );
}
