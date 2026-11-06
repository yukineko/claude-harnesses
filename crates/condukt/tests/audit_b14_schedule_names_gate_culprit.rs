// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Audit B item 14 (memory `condukt-operator-gotchas`, section "Schedule
//! force-gates benign task prose"; originally `condukt-schedule-arrow-forcegate`),
//! 2026-11-07.
//!
//! The memory's recipe for a gated task is to blank fields and drop words one
//! at a time until it flips, because `condukt schedule` says only "force-gated:
//! high-risk irreversible action" without naming WHAT matched. RED: the warning
//! for a force-gated task must name the matched command / token. The task here
//! has two declared commands, one benign and one that publishes; the warning
//! must point at the publishing one. Control: the benign task alone schedules.
//!
//! (Measured 2026-11-07: prose is no longer classified — a task without
//! `commands` is gated as undetermined, and a `->` in the title of a task with
//! declared benign commands schedules in parallel. So the original arrow FP no
//! longer reproduces; only the unnamed-culprit residue does.)

use std::process::{Command, Stdio};

fn schedule(dec: &serde_json::Value) -> serde_json::Value {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("dec.json");
    std::fs::write(&f, dec.to_string()).unwrap();
    let out = harness_core::git_env::scrub(&mut Command::new(env!("CARGO_BIN_EXE_condukt")))
        .args(["schedule", "--file", f.to_str().unwrap()])
        .current_dir(tmp.path())
        .env("HOME", tmp.path())
        .env_remove("CONDUKT_DISABLE")
        .stdin(Stdio::null())
        .output()
        .expect("condukt");
    assert!(
        out.status.success(),
        "schedule: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

#[test]
#[ignore = "backlog 6e97a022: open defect, remove ignore when fixed"]
fn force_gate_warning_names_the_matched_command() {
    let v = schedule(&serde_json::json!({"goal": "g", "tasks": [
        {"id": "t1", "title": "maps Foo -> Bar", "done_criteria": "returns Bar",
         "touched_files": ["src/a.rs"], "commands": ["cargo test -p demo"]},
        {"id": "t2", "title": "ship it", "done_criteria": "done",
         "touched_files": ["src/b.rs"],
         "commands": ["cargo build -p demo", "git push origin main"]}
    ]}));
    let gated: Vec<&str> = v["gated"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|x| x.as_str())
        .collect();
    // Control: only the publishing task is gated; the arrow prose is not.
    assert_eq!(gated, vec!["t2"], "precondition: only t2 gates; got {v}");
    let warning = v["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|x| x.as_str())
        .find(|w| w.contains("'t2'"))
        .unwrap_or_else(|| panic!("no warning for t2: {v}"))
        .to_string();
    assert!(
        warning.contains("git push"),
        "the force-gate warning does not name what matched (expected the culprit \
         command `git push origin main` / token `git push`); warning: {warning}"
    );
}
