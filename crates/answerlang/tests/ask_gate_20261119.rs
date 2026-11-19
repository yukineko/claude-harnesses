#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `answerlang ask` (PreToolUse hook for AskUserQuestion). Contract: deny when the
//! question text is not in the configured language; undeterminable input denies
//! (fail-closed); `preview` is excluded from the judged text.
mod common;
use common::*;
use serde_json::{json, Value};

fn ja_env() -> Env {
    let env = Env::new();
    env.write_config(JA_CONFIG);
    env
}

fn payload(tool: &str, questions: Value) -> Value {
    json!({"session_id":"s1","hook_event_name":"PreToolUse","tool_name":tool,
           "tool_input":{"questions":questions}})
}

fn q(question: &str, header: &str, label: &str, desc: &str, preview: Option<&str>) -> Value {
    let mut opt = json!({"label":label,"description":desc});
    if let Some(p) = preview {
        opt["preview"] = json!(p);
    }
    json!({"question":question,"header":header,"multiSelect":false,"options":[opt]})
}

fn ask(env: &Env, p: &Value) -> Out {
    env.run(&["ask"], Some(p))
}

fn hso(out: &Out, key: &str) -> Option<String> {
    out.json().and_then(|v| {
        v.get("hookSpecificOutput")
            .and_then(|h| h.get(key))
            .and_then(Value::as_str)
            .map(String::from)
    })
}
fn is_deny(out: &Out) -> bool {
    hso(out, "permissionDecision").as_deref() == Some("deny")
}
fn deny_reason(out: &Out) -> String {
    hso(out, "permissionDecisionReason").unwrap_or_default()
}

/// Spawn `answerlang ask` with a raw (possibly non-JSON) stdin body.
fn raw_ask(env: &Env, body: &str) -> Out {
    use std::io::Write;
    use std::process::{Command, Stdio};
    std::fs::create_dir_all(env.home()).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_answerlang"))
        .arg("ask")
        .current_dir(env.dir.path())
        .env("HOME", env.home())
        .env("CLAUDE_CODE_SESSION_ID", &env.session)
        .env_remove("LC_ALL")
        .env_remove("LC_MESSAGES")
        .env("LANG", "C")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    {
        let mut si = child.stdin.take().unwrap();
        let _ = si.write_all(body.as_bytes());
    }
    let o = child.wait_with_output().unwrap();
    Out {
        code: o.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
    }
}

#[test]
fn japanese_question_is_clean() {
    let env = ja_env();
    let p = payload(
        "AskUserQuestion",
        json!([q(&ja(80), &ja(10), &ja(20), &ja(40), None)]),
    );
    let out = ask(&env, &p);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    assert!(out.is_empty(), "stdout: {}", out.stdout);
}

#[test]
fn english_question_is_denied_naming_japanese() {
    let env = ja_env();
    let p = payload(
        "AskUserQuestion",
        json!([q(&en(100), "Scope", "Option A", &en(80), None)]),
    );
    let out = ask(&env, &p);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    assert!(is_deny(&out), "stdout: {}", out.stdout);
    assert_eq!(hso(&out, "hookEventName").as_deref(), Some("PreToolUse"));
    assert!(
        deny_reason(&out).to_lowercase().contains("japanese"),
        "reason: {}",
        deny_reason(&out)
    );
}

#[test]
fn english_preview_is_excluded_from_judgement() {
    let env = ja_env();
    let preview = en(600);
    let p = payload(
        "AskUserQuestion",
        json!([q(&ja(80), &ja(10), &ja(20), &ja(40), Some(&preview))]),
    );
    let out = ask(&env, &p);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    assert!(
        out.is_empty(),
        "preview must not be judged; stdout: {}",
        out.stdout
    );
}

#[test]
fn short_english_is_clean_by_rule() {
    let env = ja_env();
    // total letters well under 40
    let p = payload(
        "AskUserQuestion",
        json!([q("Which one?", "Scope", "Yes", "Do it", None)]),
    );
    let out = ask(&env, &p);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    assert!(out.is_empty(), "stdout: {}", out.stdout);
}

#[test]
fn code_only_question_is_clean_after_stripping() {
    let env = ja_env();
    let code = format!(
        "```rust\n{}\n```\n`{}`",
        "fn main() { let value = compute_the_result(); println!(\"{}\", value); }",
        "cargo test --workspace --all-targets"
    );
    let p = payload("AskUserQuestion", json!([q(&code, "ok", "ok", "ok", None)]));
    let out = ask(&env, &p);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    assert!(out.is_empty(), "stdout: {}", out.stdout);
}

#[test]
fn unparseable_stdin_denies() {
    let env = ja_env();
    let out = raw_ask(&env, "{not json");
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    assert!(is_deny(&out), "stdout: {}", out.stdout);
}

#[test]
fn missing_questions_denies() {
    let env = ja_env();
    let p = json!({"session_id":"s1","hook_event_name":"PreToolUse",
                   "tool_name":"AskUserQuestion","tool_input":{}});
    let out = ask(&env, &p);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    assert!(is_deny(&out), "stdout: {}", out.stdout);
}

#[test]
fn missing_tool_input_denies() {
    let env = ja_env();
    let p = json!({"session_id":"s1","hook_event_name":"PreToolUse","tool_name":"AskUserQuestion"});
    let out = ask(&env, &p);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    assert!(is_deny(&out), "stdout: {}", out.stdout);
}

#[test]
fn questions_not_array_denies() {
    let env = ja_env();
    let p = payload("AskUserQuestion", json!("not an array"));
    let out = ask(&env, &p);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    assert!(is_deny(&out), "stdout: {}", out.stdout);
}

#[test]
fn question_element_not_object_denies() {
    let env = ja_env();
    let p = payload("AskUserQuestion", json!(["just a string"]));
    let out = ask(&env, &p);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    assert!(is_deny(&out), "stdout: {}", out.stdout);
}

#[test]
fn numeric_label_denies() {
    let env = ja_env();
    let p = payload(
        "AskUserQuestion",
        json!([{"question":ja(80),"header":ja(10),"multiSelect":false,
                "options":[{"label":42,"description":ja(40)}]}]),
    );
    let out = ask(&env, &p);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    assert!(is_deny(&out), "stdout: {}", out.stdout);
}

#[test]
fn numeric_question_denies() {
    let env = ja_env();
    let p = payload(
        "AskUserQuestion",
        json!([{"question":7,"header":ja(10),"multiSelect":false,
                "options":[{"label":ja(10),"description":ja(40)}]}]),
    );
    let out = ask(&env, &p);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    assert!(is_deny(&out), "stdout: {}", out.stdout);
}

#[test]
fn enforce_false_is_silent() {
    let env = Env::new();
    env.write_config("language = \"japanese\"\nenforce = false\n");
    let p = payload(
        "AskUserQuestion",
        json!([q(&en(100), "Scope", "Option A", &en(80), None)]),
    );
    let out = ask(&env, &p);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    assert!(out.is_empty(), "stdout: {}", out.stdout);
}

#[test]
fn enabled_false_is_silent() {
    let env = Env::new();
    env.write_config("language = \"japanese\"\nenabled = false\n");
    let p = payload(
        "AskUserQuestion",
        json!([q(&en(100), "Scope", "Option A", &en(80), None)]),
    );
    let out = ask(&env, &p);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    assert!(out.is_empty(), "stdout: {}", out.stdout);
}

#[test]
fn no_config_does_not_deny() {
    let env = Env::new();
    assert!(env.read_config().is_none());
    let p = payload(
        "AskUserQuestion",
        json!([q(&en(100), "Scope", "Option A", &en(80), None)]),
    );
    let out = ask(&env, &p);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    assert!(!is_deny(&out), "stdout: {}", out.stdout);
}

#[test]
fn unknown_language_does_not_deny() {
    let env = Env::new();
    env.write_config("language = \"klingon\"\n");
    let p = payload(
        "AskUserQuestion",
        json!([q(&en(100), "Scope", "Option A", &en(80), None)]),
    );
    let out = ask(&env, &p);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    assert!(!is_deny(&out), "stdout: {}", out.stdout);
}

#[test]
fn other_tool_name_does_not_deny() {
    let env = ja_env();
    let p = payload(
        "Bash",
        json!([q(&en(100), "Scope", "Option A", &en(80), None)]),
    );
    let out = ask(&env, &p);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    assert!(!is_deny(&out), "stdout: {}", out.stdout);
}

#[test]
fn second_question_english_drags_share_below_min_share() {
    let env = Env::new();
    env.write_config("language = \"japanese\"\nmin_share = 0.6\n");
    let p = payload(
        "AskUserQuestion",
        json!([
            q(&ja(60), &ja(5), &ja(10), &ja(10), None),
            q(&en(400), "Scope", "Option B", &en(100), None)
        ]),
    );
    let out = ask(&env, &p);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    assert!(is_deny(&out), "stdout: {}", out.stdout);
    assert!(deny_reason(&out).to_lowercase().contains("japanese"));
}
