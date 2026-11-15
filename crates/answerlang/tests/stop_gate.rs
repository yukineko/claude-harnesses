#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod common;
use common::*;
use serde_json::json;

fn ja_env() -> Env {
    let env = Env::new();
    env.write_config(JA_CONFIG);
    env
}

#[test]
fn japanese_answer_is_clean() {
    let env = ja_env();
    let t = env.answer_transcript("t.jsonl", &ja(120));
    let out = env.stop("s1", Some(&t), false);
    assert_eq!(out.code, 0);
    assert!(out.is_empty(), "stdout: {}", out.stdout);
}

#[test]
fn english_answer_blocks_naming_japanese() {
    let env = ja_env();
    let t = env.answer_transcript("t.jsonl", &en(200));
    let out = env.stop("s1", Some(&t), false);
    assert_eq!(out.code, 0);
    assert!(out.is_block(), "stdout: {}", out.stdout);
    assert!(
        out.reason().to_lowercase().contains("japanese"),
        "reason: {}",
        out.reason()
    );
}

#[test]
fn short_english_message_is_clean_by_rule() {
    let env = ja_env();
    let t = env.answer_transcript("t.jsonl", "Done. All tests pass now.");
    let out = env.stop("s1", Some(&t), false);
    assert!(out.is_empty(), "stdout: {}", out.stdout);
}

#[test]
fn forty_letter_boundary() {
    let env = ja_env();
    let below = "a".repeat(39);
    let t = env.answer_transcript("below.jsonl", &below);
    assert!(
        !env.stop("s1", Some(&t), false).is_block(),
        "39 letters is clean"
    );
    let at = "a".repeat(40);
    let t2 = env.answer_transcript("at.jsonl", &at);
    assert!(
        env.stop("s2", Some(&t2), false).is_block(),
        "40 letters is checked"
    );
}

#[test]
fn only_last_assistant_text_entry_is_checked() {
    let env = ja_env();
    // earlier English, later Japanese -> clean
    let t = env.transcript(
        "t.jsonl",
        &[assistant(&en(200)), user("next"), assistant(&ja(120))],
    );
    assert!(!env.stop("s1", Some(&t), false).is_block());
    // earlier Japanese, later English -> block
    let t2 = env.transcript(
        "t2.jsonl",
        &[assistant(&ja(120)), user("next"), assistant(&en(200))],
    );
    assert!(env.stop("s2", Some(&t2), false).is_block());
}

#[test]
fn tool_use_only_assistant_entries_are_skipped() {
    let env = ja_env();
    let tool = json!({"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"ls"}}]}});
    let t = env.transcript("t.jsonl", &[assistant(&ja(120)), tool]);
    let out = env.stop("s1", Some(&t), false);
    assert!(out.is_empty(), "stdout: {}", out.stdout);
}

#[test]
fn sidechain_entries_are_skipped() {
    let env = ja_env();
    let side = json!({"type":"assistant","isSidechain":true,"message":{"role":"assistant","content":[{"type":"text","text":en(200)}]}});
    let t = env.transcript("t.jsonl", &[assistant(&ja(120)), side]);
    let out = env.stop("s1", Some(&t), false);
    assert!(
        !out.is_block(),
        "sidechain English must be ignored: {}",
        out.stdout
    );
}

#[test]
fn text_blocks_of_one_entry_are_joined() {
    let env = ja_env();
    let entry = json!({"type":"assistant","message":{"role":"assistant","content":[
        {"type":"text","text":ja(60)},
        {"type":"tool_use","id":"t1","name":"Bash","input":{}},
        {"type":"text","text":ja(60)}]}});
    let t = env.transcript("t.jsonl", &[entry]);
    assert!(env.stop("s1", Some(&t), false).is_empty());
}

// ---- stop_hook_active bounded re-entry ----

#[test]
fn second_failure_with_marker_is_allowed_with_system_message() {
    let env = ja_env();
    let t = env.answer_transcript("t.jsonl", &en(200));
    let first = env.stop("s1", Some(&t), false);
    assert!(first.is_block(), "stdout: {}", first.stdout);
    let second = env.stop("s1", Some(&t), true);
    assert_eq!(second.code, 0);
    assert!(!second.is_block(), "stdout: {}", second.stdout);
    let msg = second.system_message().expect("systemMessage");
    assert!(msg.contains("answerlang"), "{msg}");
    assert!(msg.contains("failed twice"), "{msg}");
}

#[test]
fn marker_is_cleared_after_allow() {
    let env = ja_env();
    let t = env.answer_transcript("t.jsonl", &en(200));
    assert!(env.stop("s1", Some(&t), false).is_block());
    assert!(!env.stop("s1", Some(&t), true).is_block());
    // Marker consumed: another re-entry without a fresh block must block again.
    assert!(env.stop("s1", Some(&t), true).is_block());
}

#[test]
fn stop_hook_active_without_prior_answerlang_block_still_blocks() {
    let env = ja_env();
    let t = env.answer_transcript("t.jsonl", &en(200));
    let out = env.stop("s1", Some(&t), true);
    assert!(
        out.is_block(),
        "another gate may have set stop_hook_active: {}",
        out.stdout
    );
}

#[test]
fn marker_is_per_session() {
    let env = ja_env();
    let t = env.answer_transcript("t.jsonl", &en(200));
    assert!(env.stop("A", Some(&t), false).is_block());
    // Different session with stop_hook_active: no marker for it -> block.
    assert!(env.stop("B", Some(&t), true).is_block());
}

#[test]
fn clean_stop_clears_marker() {
    let env = ja_env();
    let bad = env.answer_transcript("bad.jsonl", &en(200));
    let good = env.answer_transcript("good.jsonl", &ja(120));
    assert!(env.stop("s1", Some(&bad), false).is_block());
    assert!(env.stop("s1", Some(&good), false).is_empty());
    // A later violation under stop_hook_active has no marker any more -> block.
    assert!(env.stop("s1", Some(&bad), true).is_block());
}

// ---- undetermined transcript ----

#[test]
fn missing_transcript_path_blocks_mentioning_transcript() {
    let env = ja_env();
    let out = env.stop("s1", None, false);
    assert_eq!(out.code, 0);
    assert!(out.is_block(), "stdout: {}", out.stdout);
    assert!(
        out.reason().contains("transcript"),
        "reason: {}",
        out.reason()
    );
}

#[test]
fn nonexistent_transcript_file_blocks_mentioning_transcript() {
    let env = ja_env();
    let p = env.dir.path().join("nope.jsonl");
    let out = env.stop("s1", Some(&p), false);
    assert!(out.is_block(), "stdout: {}", out.stdout);
    assert!(
        out.reason().contains("transcript"),
        "reason: {}",
        out.reason()
    );
}

#[test]
fn transcript_without_assistant_text_blocks_mentioning_transcript() {
    let env = ja_env();
    let tool = json!({"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"t1","name":"Bash","input":{}}]}});
    let t = env.transcript("t.jsonl", &[user("hi"), tool]);
    let out = env.stop("s1", Some(&t), false);
    assert!(out.is_block(), "stdout: {}", out.stdout);
    assert!(
        out.reason().contains("transcript"),
        "reason: {}",
        out.reason()
    );
}

#[test]
fn only_sidechain_text_counts_as_no_assistant_text() {
    let env = ja_env();
    let side = json!({"type":"assistant","isSidechain":true,"message":{"role":"assistant","content":[{"type":"text","text":ja(120)}]}});
    let t = env.transcript("t.jsonl", &[user("hi"), side]);
    let out = env.stop("s1", Some(&t), false);
    assert!(out.is_block(), "stdout: {}", out.stdout);
    assert!(out.reason().contains("transcript"));
}

#[test]
fn undetermined_transcript_reentry_with_marker_is_allowed() {
    let env = ja_env();
    let p = env.dir.path().join("nope.jsonl");
    assert!(env.stop("s1", Some(&p), false).is_block());
    let second = env.stop("s1", Some(&p), true);
    assert!(!second.is_block(), "stdout: {}", second.stdout);
    assert!(
        second.system_message().is_some(),
        "stdout: {}",
        second.stdout
    );
}

#[test]
fn undetermined_reentry_without_marker_still_blocks() {
    let env = ja_env();
    let p = env.dir.path().join("nope.jsonl");
    assert!(env.stop("s1", Some(&p), true).is_block());
}

#[test]
fn unparseable_payload_blocks() {
    let env = ja_env();
    let mut cmd_out = env.run(&["stop"], None);
    // empty stdin = unparseable payload
    assert_eq!(cmd_out.code, 0);
    assert!(cmd_out.is_block(), "stdout: {}", cmd_out.stdout);
    cmd_out = env.run(&["stop"], Some(&json!("just a string")));
    assert!(cmd_out.is_block(), "stdout: {}", cmd_out.stdout);
}
