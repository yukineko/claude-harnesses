#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro: the stale-binary check only recognises `bin/<name>-linux-x86_64`.
//!
//! `stale.rs::check_stale_crate` builds the path `bin/{name}-linux-x86_64` and
//! returns `None` ("could not check") when it is absent. This repo's committed
//! binaries on a Mac are `bin/<name>-darwin-arm64`, so a crate whose darwin
//! binary is OLDER than its `src/` is never reported, and the checklist says
//! "[x] no stale plugin binaries".
//!
//! Asserts the correct behaviour: a stale darwin-arm64 binary must be reported
//! (or, at minimum, the check must say it could not run), never `[x]`.

use std::fs::{self, File};
use std::process::Command;
use std::time::{Duration, SystemTime};

#[test]
fn stale_darwin_arm64_binary_is_not_reported_clean() {
    let repo = tempfile::TempDir::new().unwrap();
    let p = repo.path();
    let o = Command::new("git")
        .args(["init", "-q"])
        .current_dir(p)
        .output()
        .unwrap();
    assert!(o.status.success(), "git init failed");

    let krate = p.join("crates/foo");
    fs::create_dir_all(krate.join("src")).unwrap();
    fs::create_dir_all(krate.join("bin")).unwrap();
    let bin = krate.join("bin/foo-darwin-arm64");
    let src = krate.join("src/lib.rs");
    fs::write(&bin, "binary").unwrap();
    fs::write(&src, "code").unwrap();
    let now = SystemTime::now();
    File::options()
        .write(true)
        .open(&bin)
        .unwrap()
        .set_modified(now - Duration::from_secs(3600))
        .unwrap();
    File::options()
        .write(true)
        .open(&src)
        .unwrap()
        .set_modified(now)
        .unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_ship"))
        .arg("check")
        .current_dir(p)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.contains("[x] no stale plugin binaries"),
        "darwin-arm64 binary older than src/ was reported clean:\n{stdout}"
    );
}
