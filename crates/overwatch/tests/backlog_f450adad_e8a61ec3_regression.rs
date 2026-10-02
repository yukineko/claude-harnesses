#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Shared-failure pin for backlog f450adad (closed as DUPLICATE) and e8a61ec3.
//!
//! Both tickets: `store::read_jsonl_best_effort` maps an UNREADABLE ledger
//! (`Determination::Undetermined`) to `Vec::new()`, so every public best-effort
//! wrapper (`read_review_findings`, `read_dispositions`, ...) returns
//! `Ok(vec![])` -- the same value as "no findings were ever recorded".
//!
//! Fixture: review_findings.jsonl holds non-UTF-8 bytes (unreadable as text).
//! The non-ignored control proves the tri-state reader sees it as
//! Undetermined, so the fixture really is unreadable; the ignored test is RED
//! while e8a61ec3 is open: the two-valued wrapper must not answer `Ok(empty)`.
//!
//! One test binary, one test each touching HOME: both run sequentially under a
//! mutex because HOME is process-global.

use std::path::PathBuf;
use std::sync::Mutex;

static HOME_LOCK: Mutex<()> = Mutex::new(());

fn fixture(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "overwatch-f450adad-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let home = root.join("home");
    let repo = root.join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    let st = std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(&repo)
        .status()
        .unwrap();
    assert!(st.success());
    std::env::set_var("HOME", &home);
    let path = overwatch::store::review_findings_path(&repo).unwrap();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, [0xff_u8, 0xfe, 0x00, b'\n']).unwrap();
    repo
}

#[test]
fn control_tri_state_reader_sees_the_fixture_as_undetermined() {
    let _g = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let repo = fixture("control");
    assert!(
        matches!(
            overwatch::store::scan_review_findings(&repo),
            overwatch::store::ReviewFindingScan::Undetermined(_)
        ),
        "fixture must be unreadable to the tri-state reader"
    );
}

#[test]
fn best_effort_reader_does_not_report_an_unreadable_ledger_as_empty() {
    let _g = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let repo = fixture("pin");
    match overwatch::store::read_review_findings(&repo) {
        Ok(v) if v.is_empty() => panic!(
            "an unreadable review_findings.jsonl came back as Ok(empty) -- \
             indistinguishable from 'no findings recorded'"
        ),
        _ => {}
    }
}
