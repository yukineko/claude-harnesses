//! Integration coverage for the deterministic in-code RUN-POLICY gate wired
//! into `condukt verify launch --run-policy`. Spawns the built binary so it
//! exercises the new purely-additive `--run-policy` mode end-to-end:
//!
//!   - the gate consults `decide_run_policy` internally and launches the
//!     container ONLY on the `EscalateDocker` verdict (`container_launched:true`);
//!   - the other three verdicts (VerifyOnly / EscalateShip / AskHuman) never take
//!     the container path (`container_launched:false`);
//!   - `--run <id>` records the chosen verdict to the run-policy JSONL log.
//!
//! No real docker is required: on the EscalateDocker path the container launcher
//! is the fail-soft `launch_in_container`, which — with docker absent — returns a
//! `docker_unavailable` verdict while the gate still marks the container path as
//! taken (`container_launched:true`). Every path exits 0 (never-break-a-turn).

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_condukt");

/// Drive `verify launch --run-policy` with the graded signals and return the
/// parsed verdict JSON plus the process exit code.
fn run_policy_gate_launch(
    cheap_verify: &str,
    divergence: &str,
    change_risk: &str,
) -> (serde_json::Value, i32) {
    let out = Command::new(BIN)
        .args([
            "verify",
            "launch",
            "--run-policy",
            "--cmd",
            "true",
            "--timeout",
            "5",
            "--cheap-verify",
            cheap_verify,
            "--divergence",
            divergence,
            "--change-risk",
            change_risk,
        ])
        .output()
        .expect("spawn condukt verify launch --run-policy");
    let code = out.status.code().expect("exited with a code");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("verdict must be JSON");
    (v, code)
}

/// EscalateDocker inputs must take the container path: `container_launched:true`
/// and an embedded `launch` verdict. Proven without real docker via the
/// fail-soft launcher.
#[test]
fn run_policy_gate_escalate_docker_launches_container() {
    // cheap_verify=fail + divergence=high -> EscalateDocker (per the matrix).
    let (v, code) = run_policy_gate_launch("fail", "high", "low");
    assert_eq!(code, 0, "must exit 0 (fail-soft): {v}");
    assert_eq!(
        v.get("verdict").and_then(|x| x.as_str()),
        Some("escalate_docker"),
        "verdict must be escalate_docker: {v}"
    );
    assert_eq!(
        v.get("container_launched"),
        Some(&serde_json::Value::Bool(true)),
        "EscalateDocker must take the container path: {v}"
    );
    assert!(
        v.get("launch").map(|l| l.is_object()).unwrap_or(false),
        "EscalateDocker must embed the nested container launch verdict: {v}"
    );
}

/// VerifyOnly inputs must NOT take the container path.
#[test]
fn run_policy_gate_verify_only_no_container() {
    // cheap_verify=pass + divergence=low + change_risk=medium -> VerifyOnly.
    let (v, code) = run_policy_gate_launch("pass", "low", "medium");
    assert_eq!(code, 0, "{v}");
    assert_eq!(
        v.get("verdict").and_then(|x| x.as_str()),
        Some("verify_only"),
        "{v}"
    );
    assert_eq!(
        v.get("container_launched"),
        Some(&serde_json::Value::Bool(false)),
        "VerifyOnly must not launch a container: {v}"
    );
    assert!(
        v.get("launch").is_none(),
        "no container path -> no nested launch verdict: {v}"
    );
}

/// EscalateShip inputs must NOT take the container path.
#[test]
fn run_policy_gate_escalate_ship_no_container() {
    // cheap_verify=pass + divergence=low + change_risk=low -> EscalateShip.
    let (v, code) = run_policy_gate_launch("pass", "low", "low");
    assert_eq!(code, 0, "{v}");
    assert_eq!(
        v.get("verdict").and_then(|x| x.as_str()),
        Some("escalate_ship"),
        "{v}"
    );
    assert_eq!(
        v.get("container_launched"),
        Some(&serde_json::Value::Bool(false)),
        "EscalateShip must not launch a container: {v}"
    );
}

/// AskHuman inputs must NOT take the container path.
#[test]
fn run_policy_gate_ask_human_no_container() {
    // cheap_verify=fail + divergence=low + change_risk=low -> AskHuman.
    let (v, code) = run_policy_gate_launch("fail", "low", "low");
    assert_eq!(code, 0, "{v}");
    assert_eq!(
        v.get("verdict").and_then(|x| x.as_str()),
        Some("ask_human"),
        "{v}"
    );
    assert_eq!(
        v.get("container_launched"),
        Some(&serde_json::Value::Bool(false)),
        "AskHuman must not launch a container: {v}"
    );
}

/// The existing (non-`--run-policy`) launch path must stay byte-for-byte
/// backward-compatible: `verify launch --cmd true` still refluxes the host run
/// with NO run-policy fields (no `verdict`/`container_launched` keys).
#[test]
fn run_policy_gate_absent_flag_keeps_legacy_launch() {
    let out = Command::new(BIN)
        .args(["verify", "launch", "--cmd", "true", "--timeout", "5"])
        .output()
        .expect("spawn condukt verify launch");
    assert_eq!(out.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("verdict must be JSON");
    assert_eq!(
        v.get("kind").and_then(|x| x.as_str()),
        Some("runtime"),
        "legacy launch verdict shape must be unchanged: {v}"
    );
    assert!(
        v.get("container_launched").is_none(),
        "legacy launch must NOT carry run-policy fields: {v}"
    );
}

/// `--run <id>` records the chosen verdict to the run-policy JSONL log (reusing
/// the same recording path as `run-policy decide --run`). HOME is redirected to
/// a temp dir so the state file lands under a controlled `.condukt/state` tree.
#[test]
fn run_policy_gate_records_verdict_with_run_flag() {
    let home = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let out = Command::new(BIN)
        .args([
            "verify",
            "launch",
            "--run-policy",
            "--cmd",
            "true",
            "--timeout",
            "5",
            "--cheap-verify",
            "pass",
            "--divergence",
            "low",
            "--change-risk",
            "low",
            "--run",
            "rp-test-run",
        ])
        .env("HOME", home.path())
        .current_dir(work.path())
        .output()
        .expect("spawn condukt verify launch --run-policy --run");
    assert_eq!(out.status.code(), Some(0));

    // Find the run-policy JSONL log anywhere under the redirected state tree.
    let mut found: Option<String> = None;
    let state_root = home.path().join(".condukt").join("state");
    let mut stack = vec![state_root];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in rd.flatten() {
            let p = entry.path();
            if p.is_dir() {
                stack.push(p);
            } else if p
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with(".run-policy-log.jsonl"))
            {
                found = Some(std::fs::read_to_string(&p).unwrap());
            }
        }
    }
    let log = found.expect("--run must write a run-policy JSONL log");
    assert!(
        log.contains("\"escalate_ship\""),
        "recorded verdict must be the chosen one (escalate_ship): {log}"
    );
}
