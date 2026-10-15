// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! End-to-end proof of the TRI-STATE adversarial verdict
//! (CONFIRMED / REFUTED / UNVERIFIED) through the real CLI.
//!
//! Why this exists: the Continuous-Audit verifier used to answer a BINARY
//! question with "default REFUTED", which collapses *"I could not trace a
//! permissive path"* into *"there is no permissive path"* — the same fail-open
//! shape the loop audits other crates for, and it demonstrably discarded a
//! real finding (specguard `forge/gather.rs`, 2026-07-21). An UNVERIFIED
//! finding must therefore be:
//!
//! * NOT dropped — it stays in the store and on the `review-queue` surface,
//! * NOT treated as an established finding — it is labelled `[UNVERIFIED]`,
//! * and an UNPARSEABLE `--verdict` value must land on `unverified` (the
//!   restrictive side), never silently on `confirmed`.
use std::fs;
use std::process::Command;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_overwatch")
}

#[test]
fn unverified_findings_stay_pending_and_visible() {
    let root = std::env::temp_dir().join(format!("overwatch-verdict-{}", std::process::id()));
    let home = root.join("home");
    let project = root.join("project");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&project).unwrap();
    // The overwatch lib resolves its storage root from the *process* HOME, so
    // the final read-back below must agree with the children's HOME. This test
    // binary holds a single test, so no sibling test races on the process env.
    std::env::set_var("HOME", &home);

    let record = |id: &str, summary: &str, verdict: Option<&str>| {
        let mut cmd = Command::new(bin());
        cmd.arg("record-finding")
            .arg("--finding-id")
            .arg(id)
            .arg("--source")
            .arg("continuous-audit")
            .arg("--severity")
            .arg("high")
            .arg("--summary")
            .arg(summary);
        if let Some(v) = verdict {
            cmd.arg("--verdict").arg(v);
        }
        let out = cmd
            .current_dir(&project)
            .env("HOME", &home)
            .output()
            .expect("spawn overwatch record-finding");
        assert!(out.status.success(), "record-finding must exit 0");
        String::from_utf8_lossy(&out.stdout).into_owned()
    };

    // 1. A CONFIRMED finding, an UNVERIFIED one, and one with an UNPARSEABLE
    //    verdict value (which must resolve to unverified, not confirmed).
    let stdout = record("CA-x-001", "confirmed claim", Some("confirmed"));
    assert!(
        stdout.contains("\"verdict\":\"confirmed\""),
        "stdout must echo the recorded verdict: {stdout}"
    );
    let stdout = record("CA-x-002", "undetermined claim", Some("unverified"));
    assert!(
        stdout.contains("\"verdict\":\"unverified\""),
        "stdout must echo the recorded verdict: {stdout}"
    );
    let stdout = record("CA-x-003", "garbage verdict claim", Some("probably-fine"));
    assert!(
        stdout.contains("\"verdict\":\"unverified\""),
        "an unparseable verdict must resolve to unverified (restrictive side), \
         never silently to confirmed: {stdout}"
    );

    // 2. review-queue: ALL THREE are visible (nothing is silently discarded),
    //    and the two undetermined ones are labelled so they can never be read
    //    as established findings.
    let out = Command::new(bin())
        .arg("review-queue")
        .current_dir(&project)
        .env("HOME", &home)
        .output()
        .expect("spawn overwatch review-queue");
    let queue = String::from_utf8_lossy(&out.stdout);
    for id in ["CA-x-001", "CA-x-002", "CA-x-003"] {
        assert!(
            queue.contains(id),
            "{id} must remain visible on the review surface: {queue}"
        );
    }
    assert!(
        queue.contains("[UNVERIFIED] high") || queue.matches("[UNVERIFIED]").count() == 2,
        "both undetermined findings must be marked [UNVERIFIED]: {queue}"
    );
    assert_eq!(
        queue.matches("[UNVERIFIED]").count(),
        2,
        "exactly the two undetermined findings carry the marker: {queue}"
    );

    // 3. The undetermined findings are still IN the store afterwards (pending).
    let findings = overwatch::store::read_review_findings(&project).unwrap();
    let unverified: Vec<_> = findings
        .iter()
        .filter(|f| f.verdict == overwatch::review_finding::AuditVerdict::Unverified)
        .map(|f| f.finding_id.as_str())
        .collect();
    assert_eq!(
        unverified,
        vec!["CA-x-002", "CA-x-003"],
        "undetermined findings must remain pending in the store"
    );

    fs::remove_dir_all(&root).ok();
}
