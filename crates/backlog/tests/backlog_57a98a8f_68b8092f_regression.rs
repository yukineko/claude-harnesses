//! Shared-failure pin for backlog 57a98a8f (closed as DUPLICATE) and 68b8092f.
//!
//! Both tickets: `backlog merge-driver --install` writes the absolute path of
//! the running executable (`std::env::current_exe()`) into
//! `merge.backlog.driver`. When that executable lives in a versioned plugin
//! cache dir (`.../backlog/<ver>/bin/backlog-<plat>`), pruning that version dir
//! leaves git pointing at a file that no longer exists and every later merge of
//! the task store falls back to a conflict.
//!
//! The test runs a copy of the binary from a fake versioned cache dir, installs
//! the driver, prunes the version dir, and requires that the configured driver
//! still resolves (it must not name the pruned path). `#[ignore]`d and RED while
//! 68b8092f is open. The non-ignored control proves --install configured the
//! key at all, so the ignored test cannot pass on an empty config.
//!
//! Written by an independent closure verifier, not an implementer.

use std::path::{Path, PathBuf};
use std::process::Command;

fn unique_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "backlog-57a98a8f-{}-{}-{}",
        tag,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&d).unwrap();
    d.canonicalize().unwrap()
}

fn git(repo: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Install from a binary copied into `<root>/cache/backlog/9.9.9/bin/`, then
/// return (configured driver, the version dir).
fn install_from_versioned_cache(tag: &str) -> (String, PathBuf) {
    let root = unique_dir(tag);
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    let verdir = root.join("cache").join("backlog").join("9.9.9");
    std::fs::create_dir_all(verdir.join("bin")).unwrap();
    let exe = verdir.join("bin").join("backlog-test");
    std::fs::copy(env!("CARGO_BIN_EXE_backlog"), &exe).unwrap();
    let out = Command::new(&exe)
        .args(["merge-driver", "--install"])
        .current_dir(&repo)
        .output()
        .expect("installed copy runs");
    assert!(
        out.status.success(),
        "--install: {} {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let driver = git(&repo, &["config", "--get", "merge.backlog.driver"]);
    (driver, verdir)
}

#[test]
fn control_install_configures_the_driver() {
    let (driver, _) = install_from_versioned_cache("control");
    assert!(
        driver.contains("merge-driver %O %A %B"),
        "driver = {driver:?}"
    );
}

#[test]
#[ignore = "68b8092f open: --install bakes the versioned cache path (57a98a8f duplicate)"]
fn installed_driver_survives_pruning_the_version_dir() {
    let (driver, verdir) = install_from_versioned_cache("prune");
    std::fs::remove_dir_all(&verdir).unwrap();
    let pruned = verdir.to_string_lossy().into_owned();
    assert!(
        !driver.contains(&pruned),
        "merge.backlog.driver names the pruned version dir {pruned:?}: {driver:?}"
    );
}
