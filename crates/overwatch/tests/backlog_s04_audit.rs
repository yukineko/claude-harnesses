// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Reproduction tests for the s04-overwatch backlog audit. Every test here
//! asserts the property that the named backlog item says is missing; each was
//! observed RED before being `#[ignore]`d with the item id. Remove the ignore
//! when the item is fixed.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::TempDir;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_overwatch")
}

fn run(home: &TempDir, project: &TempDir, args: &[&str]) -> Output {
    Command::new(bin())
        .args(args)
        .current_dir(project.path())
        .env("HOME", home.path())
        .output()
        .expect("spawn overwatch")
}

fn so(o: &Output) -> String {
    format!(
        "rc={:?} stdout=[{}] stderr=[{}]",
        o.status.code(),
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

/// Find `name` anywhere under `dir` (the per-project storage root is keyed by a
/// hash this test does not want to recompute).
fn find(dir: &Path, name: &str) -> Option<PathBuf> {
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let p = e.path();
        if p.file_name().map(|n| n == name).unwrap_or(false) {
            return Some(p);
        }
        if p.is_dir() {
            if let Some(f) = find(&p, name) {
                return Some(f);
            }
        }
    }
    None
}

/// Replace an existing ledger file by a directory of the same name, so the next
/// append to it fails with an IO error.
fn break_ledger(path: &Path) {
    std::fs::remove_file(path).unwrap();
    std::fs::create_dir(path).unwrap();
}

/// backlog d34715d8: a failed audit-round ledger write must not read as success.
#[test]
#[ignore = "backlog d34715d8: open defect, remove ignore when fixed"]
fn d34715d8_audit_round_record_store_write_failure_is_not_success() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let args = ["audit-round", "record", "--round", "r1", "--target", "x"];
    let ok = run(&home, &project, &args);
    assert!(ok.status.success(), "control must succeed: {}", so(&ok));
    let ledger = find(home.path(), "audit_rounds.jsonl").expect("ledger created by control");
    break_ledger(&ledger);
    let out = run(
        &home,
        &project,
        &["audit-round", "record", "--round", "r2", "--target", "x"],
    );
    assert!(
        !out.status.success(),
        "the round never reached the ledger yet the command exits 0: {}",
        so(&out)
    );
}

/// backlog c64191fa: record-finding that could not write must not exit 0.
#[test]
#[ignore = "backlog c64191fa: open defect, remove ignore when fixed"]
fn c64191fa_record_finding_store_write_failure_is_not_success() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let rec = |id: &str| {
        run(
            &home,
            &project,
            &[
                "record-finding",
                "--finding-id",
                id,
                "--source",
                "t",
                "--summary",
                "s",
                "--verdict",
                "confirmed",
            ],
        )
    };
    let ok = rec("F-1");
    assert!(ok.status.success(), "control must succeed: {}", so(&ok));
    let ledger = find(home.path(), "review_findings.jsonl").expect("ledger created by control");
    break_ledger(&ledger);
    let out = rec("F-2");
    assert!(
        !out.status.success(),
        "finding was not stored yet the command exits 0: {}",
        so(&out)
    );
}

/// backlog 3b056c02: ending a lease that does not exist must be distinguishable
/// from releasing a real one.
#[test]
#[ignore = "backlog 3b056c02: open defect, remove ignore when fixed"]
fn s3b056c02_end_of_missing_key_is_distinguishable_from_release() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    // control: a real lease is begun and released.
    let b = run(
        &home,
        &project,
        &["begin", "--key", "k-real", "--title", "t"],
    );
    assert!(b.status.success(), "{}", so(&b));
    let released = run(
        &home,
        &project,
        &["end", "--key", "k-real", "--status", "done"],
    );
    let missing = run(
        &home,
        &project,
        &["end", "--key", "k-never-begun", "--status", "done"],
    );
    assert!(
        released.status.code() != missing.status.code() || released.stdout != missing.stdout,
        "end of a key that never existed is byte-identical to a real release: \
         released: {} | missing: {}",
        so(&released),
        so(&missing)
    );
}

/// backlog 56eb3f9a: a peer lease with NO declared scope must be distinguishable
/// from a peer whose declared scope does not overlap.
#[test]
#[ignore = "backlog 56eb3f9a: open defect, remove ignore when fixed"]
fn s56eb3f9a_undeclared_scope_is_distinguishable_from_declared_disjoint() {
    let begin_b = |peer_scope: Option<&str>| {
        let home = TempDir::new().unwrap();
        let project = TempDir::new().unwrap();
        let mut a = vec!["begin", "--key", "A", "--title", "a", "--session", "s-a"];
        if let Some(s) = peer_scope {
            a.push("--scope");
            a.push(s);
        }
        let ra = run(&home, &project, &a);
        assert!(ra.status.success(), "{}", so(&ra));
        let rb = run(
            &home,
            &project,
            &[
                "begin",
                "--key",
                "B",
                "--title",
                "b",
                "--session",
                "s-b",
                "--scope",
                "src/x.rs",
            ],
        );
        assert!(rb.status.success(), "{}", so(&rb));
        String::from_utf8_lossy(&rb.stdout).to_string()
    };
    let undeclared = begin_b(None);
    let disjoint = begin_b(Some("docs/only.md"));
    assert_ne!(
        undeclared, disjoint,
        "a peer with no declared scope and a peer with a declared non-overlapping scope \
         produce the same summary: {undeclared}"
    );
}

/// backlog afdcfd4d: `auto-approved` over an UNREADABLE gate-decisions journal
/// must not print the same thing as over a never-written one.
#[test]
#[ignore = "backlog afdcfd4d: open defect, remove ignore when fixed"]
fn safdcfd4d_auto_approved_unreadable_journal_is_not_a_zero() {
    let project = TempDir::new().unwrap();
    // control: no journal at all (condukt never ran).
    let home_absent = TempDir::new().unwrap();
    let absent = run(&home_absent, &project, &["auto-approved"]);
    assert!(absent.status.success(), "{}", so(&absent));
    // fault: the journal path exists but is a directory (unreadable as a file).
    let home_bad = TempDir::new().unwrap();
    std::fs::create_dir_all(
        home_bad
            .path()
            .join(".condukt")
            .join("state")
            .join("gate-decisions.jsonl"),
    )
    .unwrap();
    let bad = run(&home_bad, &project, &["auto-approved"]);
    assert!(
        bad.status.code() != absent.status.code() || bad.stdout != absent.stdout,
        "an unreadable journal is reported exactly like a never-written one: {}",
        so(&bad)
    );
}

/// backlog e8a61ec3 (and f450adad): the best-effort `read_dispositions` /
/// `read_review_findings` readers answer an UNREADABLE ledger exactly as a
/// never-written one (`Ok(vec![])`), so `is_empty()` asserted on them cannot
/// falsify anything and `compact`'s resolved-id join consumes the collapse.
#[test]
fn se8a61ec3_best_effort_reader_distinguishes_unreadable_from_absent() {
    let home = TempDir::new().unwrap();
    std::env::set_var("HOME", home.path());
    let project = TempDir::new().unwrap();
    let cwd = project.path();
    // control: never written.
    let absent = overwatch::store::read_dispositions(cwd).map(|v| v.len());
    assert_eq!(
        format!("{absent:?}"),
        "Ok(0)",
        "control: absent ledger reads as an empty history"
    );
    // fault: path exists but is not a readable file.
    let p = overwatch::store::dispositions_path(cwd).unwrap();
    std::fs::create_dir_all(&p).unwrap();
    let unreadable = overwatch::store::read_dispositions(cwd).map(|v| v.len());
    assert_ne!(
        format!("{absent:?}"),
        format!("{unreadable:?}"),
        "an unreadable dispositions ledger reads identically to a never-written one"
    );
    // the tri-state sibling DOES distinguish (so the collapse is the reader's, not the store's).
    let scan = overwatch::store::scan_dispositions(cwd).unwrap();
    assert!(
        matches!(scan, harness_core::verdict::Determination::Undetermined(_)),
        "sibling scan_dispositions must report Undetermined"
    );
}

/// backlog 6a9eb1ed (same family as 3b056c02): when the lease lock cannot be
/// taken, `end` skips the release but exits 0 — the lease is still held.
#[cfg(unix)]
#[test]
#[ignore = "backlog 6a9eb1ed: open defect, remove ignore when fixed"]
fn s6a9eb1ed_end_that_could_not_lock_is_not_reported_as_release() {
    use std::os::unix::fs::PermissionsExt;
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let b = run(&home, &project, &["begin", "--key", "k1", "--title", "t"]);
    assert!(b.status.success(), "{}", so(&b));
    let leases = find(home.path(), "leases.json").unwrap();
    let dir = leases.parent().unwrap().to_path_buf();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
    let denied = std::fs::File::create(dir.join("probe")).is_err();
    let out = run(&home, &project, &["end", "--key", "k1", "--status", "done"]);
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(denied, "precondition: chmod 555 must deny this uid (root?)");
    let still_held = std::fs::read_to_string(&leases).unwrap().contains("\"k1\"");
    assert!(
        !(still_held && out.status.success()),
        "the lease is still held but `end` exited 0: {}",
        so(&out)
    );
}

/// backlog 4b6f4aba: a TTL-expired lease must not render identically to "no
/// lease at all" in the Sessions section.
#[test]
#[ignore = "backlog 4b6f4aba: open defect, remove ignore when fixed"]
fn s4b6f4aba_ttl_expired_lease_is_distinguishable_from_no_lease() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let b = run(
        &home,
        &project,
        &["begin", "--key", "k", "--title", "t", "--session", "s-live"],
    );
    assert!(b.status.success(), "{}", so(&b));
    let leases = find(home.path(), "leases.json").expect("leases.json");
    // control: with a FRESH heartbeat the session is listed.
    let fresh = run(&home, &project, &["sessions"]);
    let fresh_txt = String::from_utf8_lossy(&fresh.stdout).to_string();
    assert!(
        fresh_txt.contains("s-live"),
        "control: a fresh lease must be listed: {}",
        so(&fresh)
    );
    let raw = std::fs::read_to_string(&leases).unwrap();
    let mut v: serde_json::Value = serde_json::from_str(&raw).unwrap();
    v["k"]["heartbeat_at"] = serde_json::json!(1000);
    std::fs::write(&leases, serde_json::to_string(&v).unwrap()).unwrap();
    let stale = run(&home, &project, &["sessions"]);
    // no-lease baseline
    let home2 = TempDir::new().unwrap();
    let project2 = TempDir::new().unwrap();
    let none = run(&home2, &project2, &["sessions"]);
    assert_ne!(
        String::from_utf8_lossy(&stale.stdout),
        String::from_utf8_lossy(&none.stdout),
        "a lease whose heartbeat stopped renders exactly like an empty ledger: {}",
        so(&stale)
    );
}

/// backlog 27cf0081: `reconcile-fixed --help` prose vs actual behaviour on an
/// unreadable store.
#[test]
#[ignore = "backlog 27cf0081: open defect, remove ignore when fixed"]
fn s27cf0081_reconcile_fixed_help_matches_unreadable_store_behaviour() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let rec = run(
        &home,
        &project,
        &[
            "record-finding",
            "--finding-id",
            "F-1",
            "--source",
            "t",
            "--summary",
            "s",
            "--verdict",
            "confirmed",
        ],
    );
    assert!(rec.status.success(), "{}", so(&rec));
    let ledger = find(home.path(), "review_findings.jsonl").unwrap();
    break_ledger(&ledger);
    let out = run(&home, &project, &["reconcile-fixed", "--dry-run"]);
    let help = run(&home, &project, &["reconcile-fixed", "--help"]);
    let help_txt = String::from_utf8_lossy(&help.stdout).to_string();
    let degrades_to_zero = help_txt.contains("0 processed");
    // Observed behaviour: non-zero exit (not a clean "0 processed").
    if !out.status.success() {
        assert!(
            !degrades_to_zero,
            "help claims an unreadable store degrades to \"0 processed\" but the command \
             fails: {}\nhelp: {help_txt}",
            so(&out)
        );
    }
}
