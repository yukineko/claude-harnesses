// Integration test file: unwrap/expect/panic are allowed here (the workspace
// lint deny targets production code).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! modelguard spawn-model policy (user ruling 2026-11-27, verbatim:
//! 「fableの使用は禁止、実装担当は今後つくるagentではsonnetにして、これはhookで強制して」).
//!
//! Rule A: every spawn whose model contains "fable"/"mythos" (case-insensitive)
//! is denied; a non-string model, an unparseable payload or a missing
//! tool_input is denied (fail-closed).
//! Rule B: every spawn outside the control list is an implementer and must use
//! "sonnet" or "claude-sonnet-*"; otherwise denied with a reason naming sonnet.
//!
//! Allow cases are included so that an always-deny implementation fails too.

use std::io::Write;
use std::process::{Command, Stdio};

use serde_json::{json, Value};

struct Out {
    code: i32,
    stdout: String,
    stderr: String,
}

fn run_raw(stdin: &str) -> Out {
    let mut child = Command::new(env!("CARGO_BIN_EXE_modelguard"))
        .arg("gate")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn modelguard");
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(stdin.as_bytes())
        .expect("write stdin");
    let out = child.wait_with_output().expect("wait");
    Out {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

/// Build a spawn payload. `subagent_type`/`model` of `None` omit the key.
fn payload(tool: &str, subagent_type: Option<&str>, model: Option<Value>) -> String {
    let mut input = json!({
        "description": "do the task",
        "prompt": "please do the task",
    });
    if let Some(t) = subagent_type {
        input["subagent_type"] = json!(t);
    }
    if let Some(m) = model {
        input["model"] = m;
    }
    json!({
        "session_id": "sess-modelguard-test",
        "hook_event_name": "PreToolUse",
        "tool_name": tool,
        "tool_input": input,
    })
    .to_string()
}

/// The deny reason if stdout carries a PreToolUse deny decision.
fn deny_reason(out: &Out) -> Option<String> {
    let v: Value = serde_json::from_str(out.stdout.trim()).ok()?;
    let hso = v.get("hookSpecificOutput")?;
    if hso.get("hookEventName")?.as_str()? != "PreToolUse" {
        return None;
    }
    if hso.get("permissionDecision")?.as_str()? != "deny" {
        return None;
    }
    Some(
        hso.get("permissionDecisionReason")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
    )
}

fn assert_deny(stdin: &str) -> String {
    let out = run_raw(stdin);
    match deny_reason(&out) {
        Some(r) => {
            assert_eq!(out.code, 0, "deny is emitted as JSON with exit 0");
            r
        }
        None => panic!(
            "expected deny, got code={} stdout={:?} stderr={:?} for {stdin}",
            out.code, out.stdout, out.stderr
        ),
    }
}

fn assert_allow(stdin: &str) {
    let out = run_raw(stdin);
    assert_eq!(
        out.code, 0,
        "allow must exit 0; stdout={:?} stderr={:?}",
        out.stdout, out.stderr
    );
    assert!(
        deny_reason(&out).is_none(),
        "expected allow, got deny: {:?} for {stdin}",
        out.stdout
    );
}

fn assert_sonnet_deny(stdin: &str) {
    let reason = assert_deny(stdin);
    assert!(
        reason.to_lowercase().contains("sonnet"),
        "implementer deny reason must mention sonnet: {reason:?}"
    );
}

const WORKER: &str = "condukt:condukt-worker";
const VERIFIER: &str = "condukt:condukt-verifier";

// ---- allow controls -------------------------------------------------------

#[test]
fn allow_worker_with_sonnet() {
    assert_allow(&payload("Agent", Some(WORKER), Some(json!("sonnet"))));
}

#[test]
fn allow_general_purpose_with_claude_sonnet_id() {
    assert_allow(&payload(
        "Agent",
        Some("general-purpose"),
        Some(json!("claude-sonnet-5-5")),
    ));
}

#[test]
fn allow_verifier_with_opus() {
    assert_allow(&payload("Agent", Some(VERIFIER), Some(json!("opus"))));
}

#[test]
fn allow_verifier_with_haiku() {
    assert_allow(&payload("Agent", Some(VERIFIER), Some(json!("haiku"))));
}

#[test]
fn allow_explore_without_model() {
    assert_allow(&payload("Agent", Some("Explore"), None));
}

#[test]
fn allow_plan_with_opus() {
    assert_allow(&payload("Agent", Some("Plan"), Some(json!("opus"))));
}

#[test]
fn allow_task_tool_worker_with_sonnet() {
    assert_allow(&payload("Task", Some(WORKER), Some(json!("sonnet"))));
}

// ---- Rule B: implementers must use sonnet --------------------------------

#[test]
fn deny_worker_without_model() {
    assert_sonnet_deny(&payload("Agent", Some(WORKER), None));
}

#[test]
fn deny_worker_with_opus() {
    assert_sonnet_deny(&payload("Agent", Some(WORKER), Some(json!("opus"))));
}

#[test]
fn deny_general_purpose_with_empty_model() {
    assert_sonnet_deny(&payload("Agent", Some("general-purpose"), Some(json!(""))));
}

// ---- Rule A: fable / mythos banned everywhere ----------------------------

#[test]
fn deny_general_purpose_with_fable() {
    assert_deny(&payload(
        "Agent",
        Some("general-purpose"),
        Some(json!("fable")),
    ));
}

#[test]
fn deny_explore_with_claude_fable_id() {
    // Explore is a control, so only Rule A can deny this.
    assert_deny(&payload(
        "Agent",
        Some("Explore"),
        Some(json!("claude-fable-5-2")),
    ));
}

#[test]
fn deny_verifier_with_mythos_mixed_case() {
    // Verifier is a control, so only Rule A can deny this.
    assert_deny(&payload("Agent", Some(VERIFIER), Some(json!("Mythos"))));
}

// ---- fail-closed ----------------------------------------------------------

#[test]
fn deny_non_string_model() {
    assert_deny(&payload("Agent", Some(VERIFIER), Some(json!(42))));
}

#[test]
fn deny_missing_tool_input() {
    let p = json!({
        "session_id": "sess-modelguard-test",
        "hook_event_name": "PreToolUse",
        "tool_name": "Agent",
    })
    .to_string();
    assert_deny(&p);
}

#[test]
fn deny_unparseable_stdin() {
    assert_deny("this is not json {");
}

// ---- additional spec coverage ---------------------------------------------

#[test]
fn deny_implementer_with_haiku_or_inherit() {
    assert_sonnet_deny(&payload("Agent", Some(WORKER), Some(json!("haiku"))));
    assert_sonnet_deny(&payload("Agent", Some(WORKER), Some(json!("inherit"))));
}

#[test]
fn deny_missing_or_empty_or_unknown_subagent_type_without_sonnet() {
    assert_sonnet_deny(&payload("Agent", None, Some(json!("opus"))));
    assert_sonnet_deny(&payload("Agent", Some(""), None));
    assert_sonnet_deny(&payload(
        "Agent",
        Some("some:unknown-agent"),
        Some(json!("opus")),
    ));
}

#[test]
fn allow_sonnet_case_insensitive_for_implementer() {
    assert_allow(&payload(
        "Agent",
        Some("general-purpose"),
        Some(json!("Sonnet")),
    ));
}

#[test]
fn deny_fable_capitalised_on_control() {
    assert_deny(&payload("Agent", Some("Plan"), Some(json!("Fable"))));
}
