#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Repro for backlog 5b75ea9d: `overwatch lease --session <id> --json` exits 1
//! both for "this session holds no lease" (documented silent fail-soft) and for
//! an internal error (here: a corrupt leases.json), so a caller cannot tell a
//! legitimate NoLease from an undetermined lookup.  Callers (stuckguard
//! anchor.rs `if output.code() == 1 { return AnchorLookup::NoLease; }`) read
//! exit 1 as NoLease.
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

fn lease(cwd: &Path, home: &Path) -> std::process::Output {
    Command::new(bin())
        .current_dir(cwd)
        .env("HOME", home)
        .args(["lease", "--session", "sess-probe", "--json"])
        .output()
        .unwrap()
}

#[test]
#[ignore = "backlog 5b75ea9d: open defect, remove ignore when fixed"]
fn no_lease_and_internal_error_have_distinct_exit_codes() {
    let t = std::env::temp_dir().join(format!("ow-5b75ea9d-{}", std::process::id()));
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

    // Materialise a real registry via `begin` for a DIFFERENT session, so the
    // probe session legitimately has no lease.
    let b = Command::new(bin())
        .current_dir(&cwd)
        .env("HOME", &home)
        .args(["begin", "--key", "k1", "--title", "t", "--session", "other"])
        .output()
        .unwrap();
    assert!(b.status.success(), "fixture begin failed: {b:?}");

    let no_lease = lease(&cwd, &home);
    let mut found = Vec::new();
    find(&home, "leases.json", &mut found);
    assert_eq!(
        found.len(),
        1,
        "fixture: expected one leases.json, got {found:?}"
    );

    // Now make the lookup itself fail (corrupt registry).
    std::fs::write(&found[0], "{ this is not json").unwrap();
    let internal = lease(&cwd, &home);
    let _ = std::fs::remove_dir_all(&t);

    let a = no_lease.status.code();
    let b = internal.status.code();
    assert_eq!(a, Some(1), "no-lease is documented as exit 1; got {a:?}");
    assert_ne!(
        a,
        b,
        "NoLease (exit {a:?}) and a corrupt-registry internal error (exit {b:?}, stderr={:?}) are indistinguishable",
        String::from_utf8_lossy(&internal.stderr)
    );
}
