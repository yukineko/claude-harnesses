// Integration test file: unwrap/expect/panic are allowed here (the workspace
// lint deny targets production code).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Adversarial verification (modelguard-verify): a `subagent_type: "fork"`
//! spawn ignores `model`. The Agent tool's own schema text says, verbatim:
//! "a fork always inherits parent model; a `model` override is ignored" and
//! "Ignored for subagent_type: \"fork\" — forks always inherit the parent
//! model." So `{"subagent_type":"fork","model":"sonnet"}` runs on the PARENT
//! model (which may be Fable/Opus), i.e. an implementer that is not on Sonnet.
//! The gate must not accept `model: "sonnet"` as proof of Sonnet for a fork.

use std::io::Write;
use std::process::{Command, Stdio};

use serde_json::{json, Value};

fn run(stdin: &str) -> (i32, String) {
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
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

fn is_deny(stdout: &str) -> bool {
    serde_json::from_str::<Value>(stdout.trim())
        .ok()
        .and_then(|v| {
            v.get("hookSpecificOutput")?
                .get("permissionDecision")?
                .as_str()
                .map(|s| s == "deny")
        })
        .unwrap_or(false)
}

fn fork_payload(tool: &str, model: &str) -> String {
    json!({
        "session_id": "sess-modelguard-verify",
        "hook_event_name": "PreToolUse",
        "tool_name": tool,
        "tool_input": {
            "description": "implement the fix",
            "prompt": "edit src/main.rs and commit",
            "subagent_type": "fork",
            "model": model,
        },
    })
    .to_string()
}

#[test]
#[ignore = "RED(modelguard-verify): fork ignores model, so model:\"sonnet\" on a fork is not a Sonnet implementer; gate allows it"]
fn fork_with_sonnet_model_is_denied_because_fork_ignores_model() {
    for tool in ["Agent", "Task"] {
        for model in ["sonnet", "claude-sonnet-4-5"] {
            let (code, stdout) = run(&fork_payload(tool, model));
            assert!(
                code == 2 || is_deny(&stdout),
                "fork spawn with model {model:?} via {tool} was allowed (code={code}, stdout={stdout:?}); \
                 the fork runs on the parent model, not Sonnet"
            );
        }
    }
}
