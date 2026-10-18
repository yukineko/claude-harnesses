#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Repro: `overwatch reassign --key K --to X` on a key that nobody holds exits
//! 0 after printing "not held; recording intent anyway". Nothing was
//! reassigned, yet the exit code says it was. `end` on an unheld key bails
//! non-zero ("nothing was released"); reassign must do the same.
use std::path::PathBuf;
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_overwatch"))
}

#[test]
fn reassign_of_unheld_key_must_be_nonzero() {
    let t = std::env::temp_dir().join(format!("ow-repro-reassign-unheld-{}", std::process::id()));
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

    // Control: `end` of the same unheld key is already non-zero.
    let end = Command::new(bin())
        .current_dir(&cwd)
        .env("HOME", &home)
        .args(["end", "--key", "never-begun"])
        .output()
        .unwrap();
    assert!(
        !end.status.success(),
        "control: end of an unheld key must be non-zero: {end:?}"
    );

    let out = Command::new(bin())
        .current_dir(&cwd)
        .env("HOME", &home)
        .args(["reassign", "--key", "never-begun", "--to", "other"])
        .output()
        .unwrap();
    let so = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        so.contains("not held"),
        "control failed: expected the not-held note.\nstdout={so}"
    );
    assert!(
        !out.status.success(),
        "reassign of an unheld key exited 0 (nothing was reassigned).\nstdout={so}\nstderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
}
