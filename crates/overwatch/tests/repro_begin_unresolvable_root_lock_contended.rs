#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Repro: `overwatch begin` in a cwd whose project root cannot be resolved
//! (a `.git` FILE that does not parse) reports `reason: lock_contended`
//! although no lock exists anywhere. "Could not resolve the lock path" is an
//! UNDETERMINED outcome; naming it `lock_contended` tells the caller a live
//! peer holds the lease lock, which is a different (and false) fact.
//!
//! Source: `lease.rs` begin() maps `LeaseLock::acquire_or_skip` -> `None`
//! (which also covers `lock_path` Err in `lock.rs`) to `lock_contended`.
use std::path::PathBuf;
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_overwatch"))
}

#[test]
#[ignore = "repro: unresolvable root is reported as lock_contended; remove ignore when fixed"]
fn begin_on_unresolvable_root_must_not_claim_lock_contended() {
    let t = std::env::temp_dir().join(format!("ow-repro-begin-root-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&t);
    let home = t.join("home");
    let cwd = t.join("proj");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&cwd).unwrap();
    std::fs::write(cwd.join(".git"), "garbage").unwrap();

    let out = Command::new(bin())
        .current_dir(&cwd)
        .env("HOME", &home)
        .args(["begin", "--key", "k1", "--title", "t", "--session", "s1"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();

    // Control: no lock file exists anywhere under the sandbox HOME.
    let mut locks = Vec::new();
    fn find(dir: &std::path::Path, out: &mut Vec<PathBuf>) {
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    find(&p, out);
                } else if p.extension().is_some_and(|x| x == "lock") {
                    out.push(p);
                }
            }
        }
    }
    find(&home, &mut locks);
    assert!(locks.is_empty(), "control failed: a lock exists: {locks:?}");

    assert!(
        !stdout.contains("lock_contended"),
        "begin reported lock_contended with NO lock present (root was merely \
         unresolvable; that is an undetermined outcome, not contention).\n\
         code={:?}\nstdout={stdout}\nstderr={stderr}",
        out.status.code()
    );
}
