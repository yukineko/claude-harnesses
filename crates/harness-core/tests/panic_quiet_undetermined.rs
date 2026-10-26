#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Quiet-stop item 3c: `run_guarded`'s UndeterminedAllow path prints exactly one
//! short single-line `systemMessage` (no long text) and appends the long text to
//! `$HOME/.harness/state/panic-markers/log.jsonl`. FailClosedBlock is unchanged.
//! Observed out of process (panic_exit ends in process::exit).

use std::path::PathBuf;
use std::process::{Command, Output};

const CHILD_ENV: &str = "PANIC_QUIET_CHILD";
const ACTIVE_ENV: &str = "PANIC_QUIET_ACTIVE";
const GATE: &str = "probe-gate";

#[test]
fn child_panicking_body() {
    if std::env::var(CHILD_ENV).as_deref() != Ok("panic") {
        return;
    }
    let active = std::env::var(ACTIVE_ENV).as_deref() == Ok("1");
    println!();
    harness_core::gate::run::run_guarded(GATE, false, active, || -> () {
        panic!("deterministic gate panic (panic_quiet_undetermined probe)")
    });
}

fn scratch(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let d = std::env::temp_dir().join(format!("hc-pq-{tag}-{}-{nanos}", std::process::id()));
    for sub in ["home", "state", "data", "config", "cache", "cwd"] {
        std::fs::create_dir_all(d.join(sub)).unwrap();
    }
    d
}

/// No CLAUDE_CODE_SESSION_ID: the panic streak is undetermined.
fn run(root: &std::path::Path, active: bool) -> Output {
    Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "child_panicking_body",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD_ENV, "panic")
        .env(ACTIVE_ENV, if active { "1" } else { "0" })
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .env("HOME", root.join("home"))
        .env("XDG_STATE_HOME", root.join("state"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_CACHE_HOME", root.join("cache"))
        .current_dir(root.join("cwd"))
        .output()
        .unwrap()
}

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
fn first_panic_fail_closed_block_unchanged() {
    let root = scratch("first");
    let o = run(&root, false);
    let _ = std::fs::remove_dir_all(&root);
    let lines = json_lines(&o);
    assert_eq!(o.status.code(), Some(0));
    assert_eq!(lines.len(), 1, "{o:?}");
    assert_eq!(lines[0]["decision"], "block");
}

#[test]
fn undetermined_allow_is_one_short_line_and_long_text_goes_to_log() {
    let root = scratch("undet");
    let o = run(&root, true);
    let log = root.join("home/.harness/state/panic-markers/log.jsonl");
    let log_text = std::fs::read_to_string(&log).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&root);

    assert_eq!(o.status.code(), Some(0), "{o:?}");
    let lines = json_lines(&o);
    assert_eq!(lines.len(), 1, "exactly one JSON line on stdout: {o:?}");
    assert!(lines[0].get("decision").is_none());
    let msg = lines[0]["systemMessage"].as_str().expect("systemMessage");
    assert_eq!(
        msg,
        format!("{GATE}: 内部エラーで未検査のまま通しました（判定不能）")
    );
    assert!(!msg.contains('\n'));
    assert!(!msg.contains("did NOT check"), "long text leaked: {msg}");

    // durable log: one JSON line with the old long text.
    let rec: serde_json::Value = serde_json::from_str(
        log_text
            .lines()
            .last()
            .unwrap_or_else(|| panic!("no log line at {}", log.display())),
    )
    .expect("log line is JSON");
    assert_eq!(rec["event"], "undetermined_allow");
    assert_eq!(rec["gate"], GATE);
    assert!(rec["why"].as_str().is_some_and(|s| !s.is_empty()));
    assert!(
        rec["message"]
            .as_str()
            .is_some_and(|s| s.contains("did NOT check")),
        "message must hold the old long text: {rec}"
    );
}
