#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Repro: `LockError::NotAcquired` folds lock I/O failures (lock dir cannot be
//! created, temp lock cannot be written) into "contended". The CLI callsites
//! then print `reason: lock_contended` although NO live peer holds anything:
//! the lock dir is simply unusable. "Contended" tells the operator to retry
//! shortly; an I/O failure will never clear by retrying.
//!
//! One test per distinct callsite: `begin` (lease.rs), `resolve-merge-conflict`
//! (merge_conflict.rs resolution_result), `record-disposition`
//! (disposition_cli.rs disposition_result).
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_overwatch"))
}

fn sandbox(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
    let t = std::env::temp_dir().join(format!("ow-repro-lockio-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&t);
    let home = t.join("home");
    let cwd = t.join("proj");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&cwd).unwrap();
    assert!(Command::new("git")
        .current_dir(&cwd)
        .args(["init", "-q"])
        .status()
        .unwrap()
        .success());
    (t, home, cwd)
}

fn run(home: &Path, cwd: &Path, args: &[&str]) -> Output {
    Command::new(bin())
        .current_dir(cwd)
        .env("HOME", home)
        .env_remove("HARNESS_SANDBOX_HOME")
        .args(args)
        .output()
        .unwrap()
}

fn find(root: &Path, name: &str, out: &mut Vec<PathBuf>) {
    if let Ok(rd) = std::fs::read_dir(root) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                find(&p, name, out);
            } else if p.file_name().is_some_and(|n| n == name) {
                out.push(p);
            }
        }
    }
}

/// Materialise the store with a real `begin`, then make its directory
/// read-only so no lock file can be created there. Returns the dir.
fn make_store_dir_unwritable(home: &Path, cwd: &Path) -> PathBuf {
    let b = run(
        home,
        cwd,
        &["begin", "--key", "k0", "--title", "t", "--session", "s0"],
    );
    assert!(b.status.success(), "setup begin failed: {b:?}");
    let mut found = Vec::new();
    find(home, "leases.json", &mut found);
    assert_eq!(found.len(), 1, "expected one leases.json, got {found:?}");
    let dir = found[0].parent().unwrap().to_path_buf();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
    // Control: the dir really is unwritable (skip meaning if running as root).
    assert!(
        std::fs::File::create(dir.join("probe")).is_err(),
        "control failed: store dir is still writable (running as root?)"
    );
    dir
}

fn assert_not_contended(what: &str, out: &Output) {
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stdout.contains("lock_contended"),
        "{what} reported lock_contended for an I/O failure (no peer holds any lock; the \
         lock dir is unusable).\ncode={:?}\nstdout={stdout}\nstderr={stderr}",
        out.status.code()
    );
}

#[test]
#[ignore = "repro: begin reports lock_contended when the lock dir cannot be created (I/O failure); remove ignore when fixed"]
fn begin_with_uncreatable_lock_dir_must_not_claim_lock_contended() {
    let (t, home, cwd) = sandbox("begin");
    // HOME/.overwatch is a regular FILE, so the store dir can never be created.
    std::fs::write(home.join(".overwatch"), "not a dir").unwrap();
    let out = run(
        &home,
        &cwd,
        &["begin", "--key", "k1", "--title", "t", "--session", "s1"],
    );
    let _ = std::fs::remove_dir_all(&t);
    assert_not_contended("begin", &out);
}

#[test]
#[ignore = "repro: resolve-merge-conflict reports lock_contended when the lock file cannot be created (I/O failure); remove ignore when fixed"]
fn resolve_merge_conflict_with_unwritable_lock_dir_must_not_claim_lock_contended() {
    let (t, home, cwd) = sandbox("resolve");
    let dir = make_store_dir_unwritable(&home, &cwd);
    let out = run(
        &home,
        &cwd,
        &["resolve-merge-conflict", "--id", "c1", "--choose", "ours"],
    );
    let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755));
    let _ = std::fs::remove_dir_all(&t);
    assert_not_contended("resolve-merge-conflict", &out);
}

#[test]
#[ignore = "repro: record-disposition reports lock_contended when the lock file cannot be created (I/O failure); remove ignore when fixed"]
fn record_disposition_with_unwritable_lock_dir_must_not_claim_lock_contended() {
    let (t, home, cwd) = sandbox("disp");
    let dir = make_store_dir_unwritable(&home, &cwd);
    let out = run(
        &home,
        &cwd,
        &[
            "record-disposition",
            "--finding-id",
            "f1",
            "--verdict",
            "confirmed",
            "--reviewer",
            "r",
        ],
    );
    let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755));
    let _ = std::fs::remove_dir_all(&t);
    assert_not_contended("record-disposition", &out);
}
