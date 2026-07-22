// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! FAULT INJECTION — C3: `overwatch::store::read_violations` returns
//! `Ok(vec![])` on an I/O error, so "the violation ledger could not be read"
//! and "no violation has ever been recorded" are the same value.
//!
//! `crates/overwatch/src/store.rs:233-247` (verbatim):
//! ```ignore
//! pub fn read_violations(cwd: &Path) -> Result<Vec<ViolationEvent>> {
//!     let path = violations_path(cwd)?;
//!     match std::fs::read_to_string(&path) {
//!         Ok(txt) => {
//!             let mut events = Vec::new();
//!             for line in txt.lines() {
//!                 if !line.is_empty() {
//!                     if let Ok(event) = serde_json::from_str::<ViolationEvent>(line) {
//!                         events.push(event);
//!                     }
//!                 }
//!             }
//!             Ok(events)
//!         }
//!         Err(_) => Ok(Vec::new()),
//!     }
//! }
//! ```
//! Note the `Ok(events)` arm silently DROPS any line that fails to parse, too:
//! a schema-drifted real violation vanishes from a result the caller reads as
//! authoritative.
//!
//! Its fail-CLOSED twin `scan_violations` (`store.rs:258`) answers the same
//! question with three values — `Absent` / `Undetermined` / `Events` — so this
//! test drives BOTH under the SAME injected faults and puts the two answers
//! side by side. The type, not the caller's discipline, is what differs.
//!
//! Single `#[test]` on purpose: it sets the process-global `HOME` to sandbox
//! the storage root, and integration tests in one binary run in parallel
//! threads. Do not add a second `#[test]` to this file — it would race the
//! `set_var` window.

use overwatch::store::{self, ViolationScan};
use overwatch::violation::{self, RawViolation, ViolationEvent, ViolationSource};
use std::path::{Path, PathBuf};

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("overwatch-fi-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn record(cwd: &Path, rule: &'static str, ts: i64) {
    let ev = violation::build_event(
        ViolationSource::Blastguard,
        &RawViolation {
            rule_id: Some(rule),
            ..Default::default()
        },
        format!("task-{ts}"),
        format!("session-{ts}"),
        ts,
        Some(format!("denied: {rule}")),
    )
    .unwrap();
    store::append_violation(cwd, &ev).unwrap();
}

fn sigs(events: &[ViolationEvent]) -> Vec<String> {
    events.iter().map(|e| e.signature.clone()).collect()
}

#[cfg(unix)]
#[test]
fn unreadable_and_corrupt_violation_ledgers_read_as_clean() {
    use std::os::unix::fs::PermissionsExt;

    let home = temp_dir("readviol-home");
    std::env::set_var("HOME", &home);

    // ================= PHASE 0 — the two reference answers ==================
    // (a) A project with NO ledger at all.
    let empty_cwd = temp_dir("readviol-empty");
    let absent_read = store::read_violations(&empty_cwd).unwrap();
    let absent_scan = store::scan_violations(&empty_cwd);
    assert!(
        absent_read.is_empty(),
        "reference: an absent ledger reads as an empty vec"
    );
    assert!(
        matches!(absent_scan, ViolationScan::Absent),
        "reference: the fail-closed twin says Absent, got {absent_scan:?}"
    );

    // (b) A project with ONE clean, fully-parseable violation.
    let clean_cwd = temp_dir("readviol-clean");
    record(&clean_cwd, "rm-rf", 1_000);
    let clean_read = store::read_violations(&clean_cwd).unwrap();
    assert_eq!(clean_read.len(), 1, "reference: one clean violation");
    assert!(
        matches!(store::scan_violations(&clean_cwd), ViolationScan::Events(ref e) if e.len() == 1),
        "reference: the twin reports a trustworthy 1-event list"
    );

    // ============ PHASE 1 — FAULT A: the ledger cannot be read =============
    let unreadable_cwd = temp_dir("readviol-unreadable");
    record(&unreadable_cwd, "rm-rf", 2_000);
    record(&unreadable_cwd, "force-push", 2_001);
    let path = store::violations_path(&unreadable_cwd).unwrap();
    assert_eq!(
        store::read_violations(&unreadable_cwd).unwrap().len(),
        2,
        "precondition: both violations are on disk and readable"
    );

    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o000);
    std::fs::set_permissions(&path, perms).unwrap();

    let faulted_read = store::read_violations(&unreadable_cwd);
    let faulted_scan = store::scan_violations(&unreadable_cwd);

    // Restore BEFORE asserting (mirrors specguard/src/decision.rs:183-203) so a
    // failing assert never leaves an undeletable temp tree.
    let mut restore = std::fs::metadata(&path).unwrap().permissions();
    restore.set_mode(0o600);
    let _ = std::fs::set_permissions(&path, restore);

    // CONTROL (expected to PASS): the fail-closed twin, under the exact same
    // fault, keeps the distinction. This proves the fault reached the read and
    // that the distinction is representable.
    assert!(
        matches!(faulted_scan, ViolationScan::Undetermined),
        "control: scan_violations must report Undetermined under EACCES, got {faulted_scan:?}"
    );

    // Both phases must be OBSERVED, so a phase-1 failure is collected rather
    // than panicking and hiding phase 2.
    let mut failures: Vec<String> = Vec::new();

    let faulted_read = faulted_read.expect("read_violations never returns Err on I/O failure");
    if sigs(&faulted_read) == sigs(&absent_read) {
        failures.push(format!(
            "PHASE 1 FAIL-OPEN: an UNREADABLE ledger holding 2 real violations returned \
         Ok([]) — byte-identical to the 'no violation has ever been recorded' \
         answer from PHASE 0(a). scan_violations answered Undetermined under \
         the very same fault, so the distinction IS representable; \
         `Err(_) => Ok(Vec::new())` throws it away. Three call sites then \
         collapse it further with `.unwrap_or_default()`: \
             overwatch/src/bridge.rs:255, overwatch/src/review_queue.rs:460, \
             condukt/src/main.rs:2261. (read={:?}, absent={:?})",
            sigs(&faulted_read),
            sigs(&absent_read)
        ));
    }

    // ====== PHASE 2 — FAULT B: one line is present but unparseable =========
    let corrupt_cwd = temp_dir("readviol-corrupt");
    record(&corrupt_cwd, "rm-rf", 3_000);
    let cpath = store::violations_path(&corrupt_cwd).unwrap();
    let mut txt = std::fs::read_to_string(&cpath).unwrap();
    // A line an older/newer schema wrote that this build cannot decode. It may
    // be a REAL violation we can no longer see.
    txt.push_str("{\"schema\":\"v9\",\"unknown_shape\":true}\n");
    std::fs::write(&cpath, txt).unwrap();

    let partial_read = store::read_violations(&corrupt_cwd).unwrap();
    let partial_scan = store::scan_violations(&corrupt_cwd);

    // CONTROL (expected to PASS).
    assert!(
        matches!(partial_scan, ViolationScan::Undetermined),
        "control: scan_violations must report Undetermined when a line fails to \
         parse, got {partial_scan:?}"
    );

    if sigs(&partial_read) == sigs(&clean_read) {
        failures.push(format!(
            "PHASE 2 FAIL-OPEN: a 2-line ledger whose second line could NOT be decoded \
             returned exactly the same list as the fully-clean 1-line ledger from \
             PHASE 0(b). The undecodable line — possibly a real violation — was \
             dropped with no signal, and the caller reads the remainder as complete. \
             (partial={:?}, clean={:?})",
            sigs(&partial_read),
            sigs(&clean_read)
        ));
    }

    assert!(
        failures.is_empty(),
        "read_violations collapses 'cannot determine' into 'clean':\n- {}",
        failures.join("\n- ")
    );
}
