// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! backlog f8240219 / 60b1e065 regression guard, at the EXACT grading both
//! tickets name: `condukt policy answer --risk medium --reversible high
//! --confidence low` (the grading `specs/spec-loop.toml` R4 clause 2 mandates
//! for the spec-gap divert). The claim was that this escalates and leaves no
//! record in `condukt policy answers`. Both tickets were triaged as not
//! reproduced at the triage rev; `gate_decision_journaling.rs` covers the
//! escalate arm at a different level triple, this pins the spec's own one,
//! without `--approval` (which would turn the judgment request into a
//! self-answer).

use std::process::Command;

#[test]
fn spec_loop_r4_grading_escalates_and_is_recorded() {
    let dir = std::env::temp_dir().join(format!(
        "condukt-f8240219-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let d = dir.to_str().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_condukt"))
        .args([
            "policy",
            "answer",
            "--risk",
            "medium",
            "--reversible",
            "high",
            "--confidence",
            "low",
            "--question",
            "divert this spec gap?",
            "--option",
            "divert",
            "--option",
            "continue",
            "--recommend",
            "0",
            "--journal-dir",
            d,
        ])
        .env_remove("HARNESS_AUTONOMOUS")
        .env_remove("CONDUKT_AUTONOMOUS")
        .output()
        .expect("spawn condukt");
    assert_eq!(
        out.status.code(),
        Some(2),
        "precondition: this grading must escalate: {out:?}"
    );

    let answers = Command::new(env!("CARGO_BIN_EXE_condukt"))
        .args(["policy", "answers", "--journal-dir", d])
        .output()
        .expect("spawn condukt");
    assert!(answers.status.success(), "{answers:?}");
    let stdout = String::from_utf8_lossy(&answers.stdout);
    let rows: Vec<serde_json::Value> = stdout
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(
        rows.len(),
        1,
        "the escalation must leave exactly one record in `policy answers`: {stdout:?}"
    );
    assert_eq!(rows[0]["policy"], "escalate", "{}", rows[0]);
    assert_eq!(rows[0]["question"], "divert this spec gap?", "{}", rows[0]);
}
