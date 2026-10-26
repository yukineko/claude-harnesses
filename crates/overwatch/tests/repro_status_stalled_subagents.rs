// 統合テスト: テスト内の unwrap/expect/panic は意図的な assert（production 側は workspace の deny のまま）。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro: `overwatch status` must surface stalled subagents.
//!
//! Rule (user decision): a subagent transcript
//! `~/.claude/projects/<slug>/<session>/subagents/agent-<id>.jsonl` whose LAST
//! entry is a `tool_use` with no `tool_result`, and whose file mtime is 5+ minutes
//! old, is listed as stalled with agent id, minutes idle and the pending command.
//! Report only: never kill/restart. A growing transcript (fresh mtime) or one
//! whose last entry carries a result must NOT be listed.
//! The transcript itself is the signal, not an Agent output-file size (710bc8c7).
//!
//! Evidence: agent-af007aac375a7a7a3 idle 16 min on a pending Bash call;
//! agent-a53a1b1f825c994de gaps of 1425/1571/13350/45448 s after a pending tool_use.

use serde_json::json;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime};
use tempfile::TempDir;

const BIN: &str = env!("CARGO_BIN_EXE_overwatch");

fn tool_use(id: &str, cmd: &str) -> String {
    json!({"type":"assistant","isSidechain":true,"message":{"role":"assistant",
        "content":[{"type":"tool_use","id":id,"name":"Bash","input":{"command":cmd}}]}})
    .to_string()
}

fn tool_result(id: &str) -> String {
    json!({"type":"user","isSidechain":true,"message":{"role":"user",
        "content":[{"type":"tool_result","tool_use_id":id,"content":"ok"}]}})
    .to_string()
}

fn write_agent(dir: &Path, agent: &str, lines: &[String], age_secs: u64) {
    std::fs::create_dir_all(dir).unwrap();
    let p = dir.join(format!("agent-{agent}.jsonl"));
    std::fs::write(&p, lines.join("\n") + "\n").unwrap();
    let f = std::fs::OpenOptions::new().write(true).open(&p).unwrap();
    f.set_modified(SystemTime::now() - Duration::from_secs(age_secs))
        .unwrap();
}

#[test]
fn status_lists_idle_pending_tool_use_subagent_and_not_the_controls() {
    let t = TempDir::new().unwrap();
    let home = t.path().join("home");
    let proj = t.path().join("proj");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&proj).unwrap();
    assert!(Command::new("git")
        .current_dir(&proj)
        .args(["init", "-q"])
        .status()
        .unwrap()
        .success());
    let canon: PathBuf = proj.canonicalize().unwrap();
    let slug: String = canon
        .to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let sub = home
        .join(".claude/projects")
        .join(&slug)
        .join("sess-1111")
        .join("subagents");

    // (a) pending tool_use, idle 16 minutes -> MUST be listed.
    write_agent(
        &sub,
        "astalled0000000001",
        &[tool_use("toolu_a1", "sleep 99999 # STALLED-CMD-MARKER")],
        16 * 60,
    );
    // (b1) control: same shape but transcript is growing (mtime now).
    write_agent(
        &sub,
        "afresh00000000002",
        &[tool_use("toolu_b1", "echo FRESH-CMD-MARKER")],
        0,
    );
    // (b2) control: old, but last entry is a tool_result -> not pending.
    write_agent(
        &sub,
        "aresolved00000003",
        &[
            tool_use("toolu_c1", "echo RESOLVED-CMD-MARKER"),
            tool_result("toolu_c1"),
        ],
        16 * 60,
    );

    let out = Command::new(BIN)
        .current_dir(&proj)
        .env("HOME", &home)
        .env("PATH", "/usr/bin:/bin")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .env_remove("OVERWATCH_RUN_ID")
        .stdin(Stdio::null())
        .arg("status")
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout).to_string()
        + &String::from_utf8_lossy(&out.stderr);

    assert!(
        text.contains("astalled0000000001"),
        "stalled subagent (pending tool_use, idle 16 min) is not listed:\n{text}"
    );
    assert!(
        text.contains("STALLED-CMD-MARKER"),
        "pending command not shown:\n{text}"
    );
    assert!(
        text.contains("16"),
        "minutes idle (16) not shown:\n{text}"
    );
    assert!(
        !text.contains("afresh00000000002") && !text.contains("FRESH-CMD-MARKER"),
        "actively growing transcript must not be listed:\n{text}"
    );
    assert!(
        !text.contains("aresolved00000003") && !text.contains("RESOLVED-CMD-MARKER"),
        "transcript whose last entry has a result must not be listed:\n{text}"
    );
}
