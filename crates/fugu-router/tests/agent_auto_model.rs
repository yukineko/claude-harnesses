#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `fugu-router agent-model` (PreToolUse on Agent|Task) picks a model for a
//! sub-agent spawn from condukt's population (every class except "subagent",
//! never unscored episodes). `route` must ignore class-"subagent" episodes.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const TITLE: &str = "D\nP";

fn temp_home(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "fugu-router-agentmodel-{tag}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn store_path(home: &Path) -> PathBuf {
    home.join(".fugu-router").join("episodes.jsonl")
}

/// Seed `n` passing episodes of `class` on `model` (title "D\nP").
fn seed(home: &Path, class: &str, model: &str, n: usize) {
    std::fs::create_dir_all(home.join(".fugu-router")).unwrap();
    let mut s = String::new();
    for i in 0..n {
        let ep = serde_json::json!({
            "ts": 1_700_000_000u64 + i as u64,
            "title": TITLE,
            "touched_files": ["agent:general-purpose"],
            "class": class,
            "model": model,
            "role": "worker",
            "pass": true,
        });
        s.push_str(&ep.to_string());
        s.push('\n');
    }
    std::fs::write(store_path(home), s).unwrap();
}

fn run(home: &Path, args: &[&str], payload: &str, envs: &[(&str, &str)]) -> (i32, String, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_fugu-router"));
    harness_core::git_env::scrub(&mut cmd);
    cmd.args(args)
        .env("HOME", home)
        .env_remove("FUGU_ROUTER_AUTO_MODEL")
        .current_dir(home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn payload(model: Option<&str>) -> String {
    let mut ti = serde_json::json!({
        "description": "D",
        "prompt": "P",
        "subagent_type": "general-purpose",
    });
    if let Some(m) = model {
        ti["model"] = serde_json::json!(m);
    }
    serde_json::json!({
        "session_id": "S",
        "hook_event_name": "PreToolUse",
        "tool_name": "Agent",
        "tool_use_id": "toolu_1",
        "transcript_path": "/tmp/S.jsonl",
        "permission_mode": "auto",
        "prompt_id": "P1",
        "effort": {"level": "medium"},
        "cwd": "/tmp",
        "tool_input": ti,
    })
    .to_string()
}

#[test]
fn a_no_model_with_condukt_history_emits_updated_input() {
    let home = temp_home("a");
    seed(&home, "parallel", "sonnet", 5);
    let (code, stdout, stderr) = run(&home, &["agent-model"], &payload(None), &[]);
    assert_eq!(code, 0, "case a: must exit 0; stderr={stderr}");
    let v: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("case a: stdout must be one JSON object ({e}): {stdout:?}"));
    let hso = &v["hookSpecificOutput"];
    assert_eq!(
        hso["hookEventName"], "PreToolUse",
        "case a: hookEventName; got {v}"
    );
    let ui = &hso["updatedInput"];
    assert_eq!(ui["description"], "D", "case a: description kept; got {v}");
    assert_eq!(ui["prompt"], "P", "case a: prompt kept; got {v}");
    assert_eq!(
        ui["subagent_type"], "general-purpose",
        "case a: subagent_type kept; got {v}"
    );
    let m = ui["model"].as_str().unwrap_or("");
    assert!(
        ["haiku", "sonnet", "opus"].contains(&m),
        "case a: model must be a tier, got {m:?} in {v}"
    );
}

#[test]
fn a2_only_subagent_history_emits_nothing() {
    let home = temp_home("a2");
    seed(&home, "subagent", "sonnet", 5);
    let (code, stdout, stderr) = run(&home, &["agent-model"], &payload(None), &[]);
    assert_eq!(code, 0, "case a2: must exit 0; stderr={stderr}");
    assert!(
        stdout.trim().is_empty(),
        "case a2: class-subagent-only history must not override; got {stdout:?}"
    );
    assert!(
        !stderr.contains("unrecognized subcommand"),
        "case a2: agent-model subcommand must exist; stderr={stderr}"
    );
}

#[test]
fn a3_unscored_condukt_history_emits_nothing() {
    let home = temp_home("a3");
    std::fs::create_dir_all(home.join(".fugu-router")).unwrap();
    let mut s = String::new();
    for i in 0..5u64 {
        let ep = serde_json::json!({
            "ts": 1_700_000_000u64 + i, "title": TITLE,
            "touched_files": ["agent:general-purpose"], "class": "parallel",
            "model": "sonnet", "role": "worker", "pass": true, "outcome": "unscored",
        });
        s.push_str(&ep.to_string());
        s.push('\n');
    }
    std::fs::write(store_path(&home), s).unwrap();
    let (code, stdout, stderr) = run(&home, &["agent-model"], &payload(None), &[]);
    assert_eq!(code, 0, "case a3: must exit 0; stderr={stderr}");
    assert!(
        stdout.trim().is_empty(),
        "case a3: unscored episodes must never drive a choice; got {stdout:?}"
    );
}

#[test]
fn b_explicit_model_is_left_alone() {
    let home = temp_home("b");
    seed(&home, "parallel", "sonnet", 5);
    let (code, stdout, stderr) = run(&home, &["agent-model"], &payload(Some("opus")), &[]);
    assert_eq!(code, 0, "case b: must exit 0; stderr={stderr}");
    assert!(
        stdout.trim().is_empty(),
        "case b: explicit model must produce empty stdout, got {stdout:?}"
    );
    // The unknown-subcommand failure also yields empty stdout; require the
    // subcommand to be recognized so this case cannot pass vacuously.
    assert!(
        !stderr.contains("unrecognized subcommand"),
        "case b: agent-model subcommand must exist; stderr={stderr}"
    );
}

#[test]
fn c1_empty_store_is_silent_but_explained() {
    let home = temp_home("c1");
    let (code, stdout, stderr) = run(&home, &["agent-model"], &payload(None), &[]);
    assert_eq!(code, 0, "case c1: must exit 0; stderr={stderr}");
    assert!(
        stdout.trim().is_empty(),
        "case c1: empty store must produce empty stdout, got {stdout:?}"
    );
    assert!(
        !stderr.trim().is_empty() && !stderr.contains("unrecognized subcommand"),
        "case c1: empty store must explain itself on stderr; stderr={stderr:?}"
    );
}

#[test]
fn c2_unreadable_store_is_silent_but_explained() {
    let home = temp_home("c2");
    std::fs::create_dir_all(store_path(&home)).unwrap(); // a directory, not a file
    let (code, stdout, stderr) = run(&home, &["agent-model"], &payload(None), &[]);
    assert_eq!(code, 0, "case c2: must exit 0; stderr={stderr}");
    assert!(
        stdout.trim().is_empty(),
        "case c2: unreadable store must produce empty stdout, got {stdout:?}"
    );
    assert!(
        !stderr.trim().is_empty() && !stderr.contains("unrecognized subcommand"),
        "case c2: unreadable store must explain itself on stderr; stderr={stderr:?}"
    );
}

#[test]
fn d_env_opt_out_is_silent() {
    let home = temp_home("d");
    seed(&home, "parallel", "sonnet", 5);
    let (code, stdout, stderr) = run(
        &home,
        &["agent-model"],
        &payload(None),
        &[("FUGU_ROUTER_AUTO_MODEL", "0")],
    );
    assert_eq!(code, 0, "case d: must exit 0; stderr={stderr}");
    assert!(
        stdout.trim().is_empty(),
        "case d: FUGU_ROUTER_AUTO_MODEL=0 must produce empty stdout, got {stdout:?}"
    );
    assert!(
        !stderr.contains("unrecognized subcommand"),
        "case d: agent-model subcommand must exist; stderr={stderr}"
    );
}

#[test]
fn g_route_ignores_subagent_episodes() {
    let home = temp_home("g");
    seed(&home, "subagent", "sonnet", 5);
    let decomp = serde_json::json!({
        "goal": "g",
        "tasks": [{
            "id": "t1",
            "title": TITLE,
            "class": "refactor",
            "touched_files": ["agent:general-purpose"],
        }],
    })
    .to_string();
    let report = home.join("report.json");
    let (code, _stdout, stderr) = run(
        &home,
        &["route", "--report", report.to_str().unwrap()],
        &decomp,
        &[],
    );
    assert_eq!(code, 0, "case g: route must exit 0; stderr={stderr}");
    let rep: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&report).unwrap()).unwrap();
    assert_eq!(
        rep["t1"]["basis"], "prior",
        "case g: condukt routing must ignore class-subagent episodes (basis must be prior); got {rep}"
    );
}
