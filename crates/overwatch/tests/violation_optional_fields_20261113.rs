// このファイルは丸ごと integration test なので unwrap/expect を許可する
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (condukt run-20261113-044355-24760 t1): `ViolationEvent` gains seven
//! OPTIONAL context fields — `decision`, `tool`, `command`, `target`, `cwd`,
//! `actor`, `system_harm` — defaulted on read and omitted on write when None.
//!
//! Only `serde_json::Value` round-trips are used, so this file compiles both
//! before and after the struct change: the assertions, not the type checker,
//! decide.

use overwatch::violation::ViolationEvent;
use serde_json::{json, Value};

const NEW_KEYS: [&str; 7] = [
    "decision",
    "tool",
    "command",
    "target",
    "cwd",
    "actor",
    "system_harm",
];

fn old_line() -> Value {
    json!({
        "source": "blastguard",
        "signature": "blastguard:rm-recursive",
        "task_key": "Bash",
        "session_id": "sess-old",
        "ts": 1_794_512_717_i64,
        "detail": "recursive rm (-r) can delete an entire directory tree",
    })
}

fn new_line() -> Value {
    let mut v = old_line();
    v["session_id"] = json!("sess-new");
    v["decision"] = json!("ask");
    v["tool"] = json!("Write");
    v["command"] = json!("rm \"$f\"");
    v["target"] = json!("/etc/hosts");
    v["cwd"] = json!("/tmp/some project");
    v["actor"] = json!("subagent");
    v["system_harm"] = json!(true);
    v
}

fn round_trip(v: &Value) -> Value {
    let ev: ViolationEvent = serde_json::from_value(v.clone()).expect("line deserializes");
    serde_json::to_value(&ev).expect("event serializes")
}

// ---- 1. control: old-format line is unchanged by the round trip ----

#[test]
fn b1_old_format_line_round_trips_without_new_keys() {
    let out = round_trip(&old_line());
    for k in NEW_KEYS {
        assert!(
            out.get(k).is_none(),
            "absent optional field `{k}` must be omitted, not serialized: {out}"
        );
    }
    assert_eq!(out, old_line(), "old-format line must round-trip exactly");
}

// ---- 2. new-format line preserves every new field ----

#[test]
fn b2_new_format_line_preserves_all_new_fields() {
    let input = new_line();
    let out = round_trip(&input);
    let mut missing = Vec::new();
    for k in NEW_KEYS {
        if out.get(k) != input.get(k) {
            missing.push(format!(
                "{k}: want {:?}, got {:?}",
                input.get(k),
                out.get(k)
            ));
        }
    }
    assert!(
        missing.is_empty(),
        "new fields not preserved through ViolationEvent:\n{}",
        missing.join("\n")
    );
    assert_eq!(out, input);
}

// ---- 3. scan over a mixed old/new ledger stays Events ----

/// The ONLY test in this binary that mutates the process-global `HOME`; the
/// other two never read it, so no lock is needed.
#[test]
fn b3_scan_violations_reads_mixed_old_and_new_lines() {
    let home = tempfile::Builder::new()
        .prefix("overwatch-vof-home-")
        .tempdir()
        .unwrap();
    let proj = tempfile::Builder::new()
        .prefix("overwatch-vof-proj-")
        .tempdir()
        .unwrap();
    let prev = std::env::var_os("HOME");
    std::env::set_var("HOME", home.path());

    let path = overwatch::store::violations_path(proj.path()).expect("ledger path");
    assert!(
        path.starts_with(home.path()),
        "ledger must live under the sandboxed HOME: {}",
        path.display()
    );
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let body = format!("{}\n{}\n", old_line(), new_line());
    std::fs::write(&path, body).unwrap();

    let scan = overwatch::store::scan_violations(proj.path());

    match prev {
        Some(h) => std::env::set_var("HOME", h),
        None => std::env::remove_var("HOME"),
    }

    match scan {
        overwatch::store::ViolationScan::Events(evs) => {
            assert_eq!(evs.len(), 2, "both lines must be read: {evs:?}");
            let sessions: Vec<&str> = evs.iter().map(|e| e.session_id.as_str()).collect();
            assert_eq!(sessions, ["sess-old", "sess-new"]);
            // The new line's context must survive the scan, not just parse.
            let new = serde_json::to_value(&evs[1]).unwrap();
            assert_eq!(new["actor"], "subagent", "scanned new line: {new}");
            assert_eq!(new["system_harm"], true, "scanned new line: {new}");
        }
        other => panic!("mixed ledger must scan as Events, got {other:?}"),
    }
}
