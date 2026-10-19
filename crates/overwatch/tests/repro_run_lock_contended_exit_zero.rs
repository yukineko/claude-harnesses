#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Repro: `overwatch run --key K` under REAL lease-lock contention exits 0.
//! `lease.rs` run() maps `acquire_or_skip -> None` to `return Ok(())`, so the
//! Running event is never recorded and the heartbeat is never refreshed, yet
//! the caller sees success. `begin` (exit 1) and `end` (bail) both report the
//! same contention as a failure.
//!
//! The lock is held by THIS test process (live pid), so the binary waits the
//! full 10s deadline and then takes the skip arm.
use std::path::{Path, PathBuf};
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_overwatch"))
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

#[test]
#[ignore = "repro: run exits 0 under real lock contention; remove ignore when fixed"]
fn run_under_real_lock_contention_must_be_nonzero() {
    let t = std::env::temp_dir().join(format!("ow-repro-run-contend-{}", std::process::id()));
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

    // Materialise the store with a real begin.
    let b = Command::new(bin())
        .current_dir(&cwd)
        .env("HOME", &home)
        .args(["begin", "--key", "k1", "--title", "t", "--session", "s1"])
        .output()
        .unwrap();
    assert!(b.status.success(), "setup begin failed: {b:?}");

    let mut found = Vec::new();
    find(&home, "leases.json", &mut found);
    assert_eq!(found.len(), 1, "expected one leases.json, got {found:?}");
    let lock = found[0].with_extension("lock");
    // A live holder: this very test process.
    std::fs::write(
        &lock,
        format!(
            "{{\"pid\":{},\"acquired_at\":{}}}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs()
        ),
    )
    .unwrap();

    // Control: `end` reports the same contention as a failure.
    let end = Command::new(bin())
        .current_dir(&cwd)
        .env("HOME", &home)
        .args(["end", "--key", "k1"])
        .output()
        .unwrap();
    assert!(
        !end.status.success(),
        "control: end must be non-zero under contention: {end:?}"
    );

    let out = Command::new(bin())
        .current_dir(&cwd)
        .env("HOME", &home)
        .args(["run", "--key", "k1", "--note", "n"])
        .output()
        .unwrap();
    let _ = std::fs::remove_file(&lock);
    assert!(
        !out.status.success(),
        "run exited 0 although the lease lock was held by a live peer and \
         nothing was recorded.\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
