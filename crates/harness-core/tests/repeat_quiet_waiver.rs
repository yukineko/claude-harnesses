#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Quiet-stop item 1: the second-occurrence waiver of `repeat::emit_stop_block`
//! prints exactly ONE short single-line `systemMessage` (no reason text); the full
//! reason lives only in `waivers.log`. The first occurrence still blocks.
//! `emit_stop_block` prints, so it is observed out of process via a re-exec'd
//! child test fn with isolated HOME and a controlled session id.

use std::path::PathBuf;
use std::process::{Command, Output};

const CHILD_ENV: &str = "REPEAT_QUIET_CHILD";
const GATE: &str = "probe-gate";
const SESSION: &str = "s-repeat-quiet-waiver";
const REASON: &str =
    "LONG-REASON-TEXT-MARKER: tests are red in crate foo\nsecond line of the reason";

#[test]
fn child_emit_twice() {
    let Ok(n) = std::env::var(CHILD_ENV) else {
        return;
    };
    for _ in 0..n.parse::<usize>().unwrap() {
        println!();
        harness_core::repeat::emit_stop_block(GATE, REASON);
    }
}

fn scratch(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let d = std::env::temp_dir().join(format!("hc-rq-{tag}-{}-{nanos}", std::process::id()));
    for sub in ["home", "state", "data", "config", "cache", "cwd"] {
        std::fs::create_dir_all(d.join(sub)).unwrap();
    }
    d
}

fn run(root: &PathBuf, times: usize) -> Output {
    Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "child_emit_twice",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD_ENV, times.to_string())
        .env("CLAUDE_CODE_SESSION_ID", SESSION)
        .env("HOME", root.join("home"))
        .env("XDG_STATE_HOME", root.join("state"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_CACHE_HOME", root.join("cache"))
        .current_dir(root.join("cwd"))
        .output()
        .unwrap()
}

/// JSON object lines (parsed from first `{`) of stdout.
fn json_lines(o: &Output) -> Vec<serde_json::Value> {
    String::from_utf8_lossy(&o.stdout)
        .lines()
        .filter_map(|l| {
            let s = l.find('{')?;
            serde_json::from_str(l[s..].trim()).ok()
        })
        .collect()
}

#[test]
fn first_occurrence_still_blocks() {
    let root = scratch("first");
    let o = run(&root, 1);
    let _ = std::fs::remove_dir_all(&root);
    let lines = json_lines(&o);
    assert_eq!(lines.len(), 1, "one JSON line expected: {:?}", o);
    assert_eq!(lines[0]["decision"], "block");
    assert_eq!(lines[0]["reason"], REASON);
}

#[test]
fn second_occurrence_waiver_is_one_short_line_and_reason_goes_to_log() {
    let root = scratch("second");
    let o = run(&root, 2);
    let log = root
        .join("home/.claude/harness-state/repeat")
        .join(SESSION)
        .join("waivers.log");
    let log_text = std::fs::read_to_string(&log).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&root);

    let lines = json_lines(&o);
    assert_eq!(lines.len(), 2, "block then waiver line: {:?}", o);
    assert_eq!(lines[0]["decision"], "block");
    let w = &lines[1];
    assert!(w.get("decision").is_none(), "waiver must not be a decision");
    let msg = w["systemMessage"].as_str().expect("systemMessage string");
    assert_eq!(
        msg,
        format!("{GATE}: 同じ指摘の2度目のため通しました（未解消）"),
        "waiver one-liner must be exactly the short message"
    );
    assert!(!msg.contains('\n'), "systemMessage must be single line");
    assert!(
        !msg.contains("LONG-REASON-TEXT-MARKER"),
        "systemMessage must not carry the reason text: {msg}"
    );
    assert!(
        log_text.contains(REASON) || log_text.contains("LONG-REASON-TEXT-MARKER"),
        "full reason must be durable in waivers.log ({}): {log_text:?}",
        log.display()
    );
}
