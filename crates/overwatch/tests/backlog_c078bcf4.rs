#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! backlog c078bcf4: `record-finding --verdict refuted --probe <file>
//! --signed-off-by <who>` uses the probe and the sign-off only to ADJUDICATE
//! the verdict, then throws both away: `ReviewFinding` has no field for either,
//! so a stored `refuted` row carries no audit trail of which probe result
//! justified it or who signed it off.
//!
//! All state lives in a temp HOME + temp project; the real ledger is untouched.

use std::process::Command;

#[test]
#[ignore = "backlog c078bcf4: open defect, remove ignore when fixed"]
fn stored_refuted_row_keeps_probe_path_and_signer() {
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let probe = project.path().join("probe-result.json");
    std::fs::write(&probe, r#"{"result":"not_reproduced"}"#).unwrap();
    let probe_s = probe.to_string_lossy().into_owned();

    let out = Command::new(env!("CARGO_BIN_EXE_overwatch"))
        .args([
            "record-finding",
            "--finding-id",
            "W-REF-1",
            "--source",
            "continuous-audit",
            "--summary",
            "claimed unreachable branch",
            "--verdict",
            "refuted",
            "--probe",
            &probe_s,
            "--signed-off-by",
            "signer-yuki",
        ])
        .current_dir(project.path())
        .env("HOME", home.path())
        .output()
        .expect("spawn overwatch");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && stdout.contains("\"verdict\":\"refuted\""),
        "precondition: a probe + sign-off REFUTED is stored refuted. stdout={stdout} stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Read back the durable row exactly as stored.
    std::env::set_var("HOME", home.path());
    let path = overwatch::store::review_findings_path(project.path()).unwrap();
    let body = std::fs::read_to_string(&path).unwrap();
    assert!(
        body.contains("signer-yuki") && body.contains("probe-result.json"),
        "the stored refuted row keeps neither the signer nor the probe that \
         justified it: {body}"
    );
}
