#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (2026-11-15): answerlang's Stop hook judged a STALE interim assistant
//! text instead of the final answer of the turn that was stopping.
//!
//! Observed facts (verbatim, transcript
//! `/Users/yuki/.claude/projects/-Users-yuki-src-harness/6dcca0ad-a40c-4645-956c-9bae0affdc31.jsonl`):
//! * line 2126 (user, Stop hook feedback): "answerlang: your final answer is not in japanese: only 0% of its 101 prose letters are in japanese script"
//! * line 2117 (assistant, 08:31:54.049Z): English interim text, 101 prose letters
//!   (backtick spans removed, alphabetic chars counted).
//! * line 2123 (assistant, 08:32:12.105Z): Japanese final text, 1276 prose letters.
//! * line 2124 (Stop hook attachment, 08:32:12.245Z): 140 ms after line 2123.
//!   => the hook decided on line 2117's text, not on line 2123's.
//! * Earlier firing, user line 1662 "113 prose letters": the last transcript
//!   assistant text before it was line 1652 (Japanese, 1009 letters, 04:09:54.731Z);
//!   an English interim text at line 1616 (04:04:53.110Z) counts 115 by my
//!   count (hook says 113; counting rules differ slightly, so this second case is
//!   only SUGGESTIVE, not confirmed equal).
//! * `transcript::last_assistant_text` reads the transcript FILE and takes the last
//!   non-sidechain assistant text line (crates/answerlang/src/transcript.rs).
//! * Claude Code changelog (~/.claude/cache/changelog.md:5962): "Added
//!   `last_assistant_message` field to Stop and SubagentStop hook inputs, providing the
//!   final assistant response text so hooks can access it without parsing transcript
//!   files." Observed in ~/.overwatch/harness-e77d23c7/overwatch/subagents.json:
//!   6928 `stop_last_assistant_message` values captured from SubagentStop payloads.
//!   NOT observed: a raw MAIN-session Stop payload carrying the field.
//!   Contradicting note: crates/context-governor/src/io.rs:10 claims "there is no
//!   `last_assistant_message` field" (unsourced there; contradicted by the SubagentStop capture).
//!
//! Two readings:
//! (1) real defect: at Stop time the transcript file lags the final message
//!     (async flush), so reading the file yields a stale text. The payload's
//!     `last_assistant_message` is the authoritative source and must win.
//! (2) intended / not a defect: the main-session Stop payload does NOT carry the
//!     field and the lag is not reproducible; the right fix is then different
//!     (e.g. treat "last assistant text precedes the last tool_result" as
//!     undetermined) and this test asserts a contract Claude Code does not honour.
//! Tell apart: capture one raw main-session Stop payload (a debug hook that tees
//! stdin to a file) and check for `last_assistant_message`; present => reading (1).
//!
//! DOES NOT PROVE: that the main-session Stop payload carries the field (UNVERIFIED);
//! that file lag (rather than another cause) produced the stale read; that the
//! 1662/113 firing is the same pattern.
mod common;
use common::*;
use serde_json::json;

fn ja_env() -> Env {
    let env = Env::new();
    env.write_config(JA_CONFIG);
    env
}

/// Transcript still ends at the English interim text (the Japanese final answer
/// is not flushed yet); the payload carries the Japanese final message.
#[test]
fn payload_last_assistant_message_wins_over_stale_transcript_line() {
    let env = ja_env();
    let t = env.answer_transcript("t.jsonl", &en(200));
    let p = json!({
        "session_id": "s1",
        "hook_event_name": "Stop",
        "stop_hook_active": false,
        "transcript_path": t.to_string_lossy(),
        "last_assistant_message": ja(300),
    });
    let out = env.run(&["stop"], Some(&p));
    assert_eq!(out.code, 0);
    assert!(
        !out.is_block(),
        "judged the stale transcript text, not last_assistant_message; stdout: {}",
        out.stdout
    );
}

/// Control: the payload field must not launder an English final answer.
#[test]
fn payload_english_final_message_still_blocks() {
    let env = ja_env();
    let t = env.answer_transcript("t.jsonl", &ja(300));
    let p = json!({
        "session_id": "s1",
        "hook_event_name": "Stop",
        "stop_hook_active": false,
        "transcript_path": t.to_string_lossy(),
        "last_assistant_message": en(200),
    });
    let out = env.run(&["stop"], Some(&p));
    assert!(out.is_block(), "stdout: {}", out.stdout);
}
