#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro (scout 2026-11-05): `schemaguard check --schema decomposition`
//! returns `valid:true` for a decomposition that condukt's own parser rejects.
//!
//! Observed: the schema inspects only `goal` and `tasks[].{id,title,class,
//! done_criteria,suggested_model,confidence}` (crates/schemaguard/src/registry.rs).
//! `condukt::model::Task` also declares `touched_files: Vec<String>` and
//! `deps: Vec<String>` (crates/condukt/src/model.rs); a wrong-typed value there
//! reports `valid:true` with `not_checked: "not declared in this schema —
//! unknown extra fields are allowed and were not inspected"`.
//!
//! Two readings: (1) INTENDED — schemaguard is a precheck, unknown fields are
//! deliberately waived and surfaced in `not_checked`; (2) DEFECT — these are not
//! unknown fields (condukt declares them and deserialization fails on them), and
//! a consumer reading `valid:true`/exit 0 gets "clean" for input condukt rejects
//! (CLAUDE.md section 1: silence reads as pass). Tell apart: does `condukt` accept
//! this JSON? A task with `"touched_files": "not-a-list"` fails serde
//! (`Vec<String>`), so the precheck's `valid:true` is wrong about condukt's view.

use std::io::Write;
use std::process::{Command, Stdio};

#[test]
fn decomposition_with_wrong_typed_declared_task_fields_is_not_valid() {
    let payload = r#"{"goal":"g","tasks":[{"id":"t1","title":"x","touched_files":"not-a-list","deps":5,"class":"parallel"}]}"#;
    let mut child = Command::new(env!("CARGO_BIN_EXE_schemaguard"))
        .args(["check", "--schema", "decomposition"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.as_bytes())
        .unwrap();
    let o = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&o.stdout).into_owned();
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("json");
    assert_ne!(
        v["valid"],
        serde_json::Value::Bool(true),
        "wrong-typed touched_files/deps must not report valid:true: {stdout}"
    );
}
