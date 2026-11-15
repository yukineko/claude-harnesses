// このファイルは丸ごと integration test なので unwrap/expect を許可する。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! **Subagent Ask relay: the two Stop-hook fail-closed paths that
//! `subagent_ask_relay_20261114.rs` does not cover.**
//!
//! 1. `blastguard relay stop` given an unparseable (non-terminal) stdin must
//!    block, not allow (`main.rs::relay_stop`, the `let Some(input) = input else`
//!    arm).
//! 2. A queue file holding an `executed` / `relayed` / `dismissed` event whose
//!    id has no `queued` line is undetermined (`relay::load`), so the Stop hook
//!    must block and name the queue file.
//!
//! Only decisions, JSON fields, exit codes and the queue path are asserted; no
//! prose. Every block assertion is paired with an allow control driven through
//! the same binary and the same helper so it cannot pass vacuously.

#[path = "support/bounded.rs"]
mod bounded;

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

struct Env {
    home: PathBuf,
    relay: PathBuf,
    approvals: PathBuf,
    cwd: PathBuf,
}

fn env(name: &str) -> Env {
    let root = std::env::temp_dir().join(format!("bg-relay-fc-{}-{}", name, std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let e = Env {
        home: root.join("home"),
        relay: root.join("relay"),
        approvals: root.join("approvals"),
        cwd: root.join("cwd"),
    };
    std::fs::create_dir_all(&e.home).unwrap();
    std::fs::create_dir_all(&e.approvals).unwrap();
    std::fs::create_dir_all(&e.cwd).unwrap();
    e
}

fn relay_stop(e: &Env, stdin: &str) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_blastguard"));
    c.args(["relay", "stop"])
        .env("HOME", &e.home)
        .env("BLASTGUARD_RELAY_DIR", &e.relay)
        .env("BLASTGUARD_APPROVALS_DIR", &e.approvals)
        .env("CLAUDE_CODE_SESSION_ID", "sessEnvIgnored")
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .env_remove("BLASTGUARD_ASK")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    bounded::run_bounded(&mut c, stdin.as_bytes(), bounded::HOOK_DEADLINE)
}

fn stop_payload(session: &str, cwd: &Path) -> String {
    json!({
        "hook_event_name": "Stop",
        "session_id": session,
        "cwd": cwd.to_string_lossy(),
        "stop_hook_active": false,
    })
    .to_string()
}

fn stdout_json(o: &Output) -> Option<Value> {
    let s = String::from_utf8_lossy(&o.stdout);
    if s.trim().is_empty() {
        return None;
    }
    serde_json::from_str(s.trim()).ok()
}

fn blocks(o: &Output) -> bool {
    stdout_json(o).is_some_and(|d| d["decision"] == "block")
}

fn qfile(e: &Env, session: &str) -> PathBuf {
    e.relay.join(format!("{session}.jsonl"))
}

#[test]
fn stop_with_unparseable_stdin_blocks() {
    let e = env("garbage");
    let o = relay_stop(&e, "garbage");
    assert_eq!(o.status.code(), Some(0));
    assert!(
        blocks(&o),
        "unparseable payload must block; stdout: {}",
        String::from_utf8_lossy(&o.stdout)
    );
    assert!(stdout_json(&o).unwrap()["reason"]
        .as_str()
        .is_some_and(|r| !r.is_empty()));

    // Control: the same invocation with a valid payload and no queue allows,
    // so the block above comes from the parse failure, not from the setup.
    let o = relay_stop(&e, &stop_payload("sessFcGarbage", &e.cwd));
    assert_eq!(o.status.code(), Some(0));
    assert!(
        !blocks(&o),
        "stdout: {}",
        String::from_utf8_lossy(&o.stdout)
    );
}

#[test]
fn stop_blocks_on_event_whose_id_has_no_queued_line_and_names_the_file() {
    let events: [(&str, Value); 3] = [
        (
            "executed",
            json!({"event": "executed", "id": "bgr-0123456789ab", "ts": 1, "exit_code": 0}),
        ),
        (
            "relayed",
            json!({"event": "relayed", "id": "bgr-0123456789ab", "ts": 1}),
        ),
        (
            "dismissed",
            json!({"event": "dismissed", "id": "bgr-0123456789ab", "ts": 1, "reason": "r"}),
        ),
    ];
    for (kind, ev) in events {
        let e = env(&format!("orphan-{kind}"));
        let s = "sessFcOrphan";
        std::fs::create_dir_all(&e.relay).unwrap();
        std::fs::write(qfile(&e, s), format!("{ev}\n")).unwrap();

        let o = relay_stop(&e, &stop_payload(s, &e.cwd));
        assert_eq!(o.status.code(), Some(0), "{kind}");
        assert!(
            blocks(&o),
            "{kind}: orphan event must block; stdout: {}",
            String::from_utf8_lossy(&o.stdout)
        );
        let reason = stdout_json(&o).unwrap()["reason"]
            .as_str()
            .expect("reason")
            .to_string();
        assert!(
            reason.contains(qfile(&e, s).to_string_lossy().as_ref()),
            "{kind}: reason must name the queue file"
        );
    }
}

#[test]
fn control_empty_session_without_queue_file_is_allowed() {
    let e = env("control");
    let o = relay_stop(&e, &stop_payload("sessFcEmpty", &e.cwd));
    assert_eq!(o.status.code(), Some(0));
    assert!(!qfile(&e, "sessFcEmpty").exists());
    assert!(
        !blocks(&o),
        "stdout: {}",
        String::from_utf8_lossy(&o.stdout)
    );
}
