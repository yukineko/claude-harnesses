// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (observed live 2026-10-03): `plugin_bin::cache_lookup_in` probes
//! `<entry>/bin/<name>` for EVERY entry of `<root>/<name>/`. Since
//! `scripts/rollout-plugins.sh` writes a plain file
//! `<root>/<name>/.version-history.jsonl` into each plugin cache dir, the probe
//! on `.version-history.jsonl/bin/<name>` fails with `ENOTDIR`, and the whole
//! lookup collapses to `Undetermined("could not test …: Not a directory")` —
//! deployed `overwatch status` printed exactly that for backlog/condukt/compass.
//!
//! A plain file in the cache dir is an *observation* (it is not a version dir,
//! so it cannot hold `bin/<name>`), not an inability to look. The cases below
//! pin that, plus a control that must STAY `Undetermined`: a version directory
//! whose `bin/` genuinely cannot be tested (EACCES) is still "could not look".
#![cfg(unix)]

use harness_core::plugin_bin::cache_lookup_in;
use harness_core::verdict::Determination;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

const NAME: &str = "backlog";

/// Build `<root>/<name>/<v>/bin/<name>` for each `v`.
fn plant(root: &Path, name: &str, versions: &[&str]) {
    for v in versions {
        let bin = root.join(name).join(v).join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join(name), b"#!/bin/sh\n").unwrap();
    }
}

/// Write the rollout ledger file exactly where rollout-plugins.sh puts it.
fn plant_history_file(root: &Path, name: &str) {
    let dir = root.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(".version-history.jsonl"),
        b"{\"version\":\"0.1.12\"}\n",
    )
    .unwrap();
}

fn expect_known(d: Determination<Option<PathBuf>>) -> Option<PathBuf> {
    match d {
        Determination::Known(v) => v,
        Determination::Undetermined(r) => panic!("expected Known, got undetermined: {r}"),
    }
}

#[test]
fn plain_file_beside_version_dirs_does_not_make_lookup_undetermined() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    plant(root, NAME, &["0.1.9", "0.1.12"]);
    plant_history_file(root, NAME);

    let got = expect_known(cache_lookup_in(root, NAME));
    assert_eq!(
        got,
        Some(root.join(NAME).join("0.1.12").join("bin").join(NAME)),
        "highest version dir must win; the plain .version-history.jsonl file is not a candidate"
    );
}

#[test]
fn only_a_plain_file_means_nothing_installed() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    plant_history_file(root, NAME);

    let got = expect_known(cache_lookup_in(root, NAME));
    assert_eq!(
        got, None,
        "a stray plain file is not evidence of an install: Known(None), not Undetermined"
    );
}

#[test]
fn symlinked_version_dir_is_still_a_candidate() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    // Real version dir lives outside the plugin cache dir; the cache dir holds
    // only a symlink to it (plus the ledger file, to exercise both together).
    let real = root.join("elsewhere").join("0.3.0");
    std::fs::create_dir_all(real.join("bin")).unwrap();
    std::fs::write(real.join("bin").join(NAME), b"#!/bin/sh\n").unwrap();
    std::fs::create_dir_all(root.join(NAME)).unwrap();
    std::os::unix::fs::symlink(&real, root.join(NAME).join("0.3.0")).unwrap();
    plant_history_file(root, NAME);

    let got = expect_known(cache_lookup_in(root, NAME));
    assert_eq!(
        got,
        Some(root.join(NAME).join("0.3.0").join("bin").join(NAME)),
        "a symlink to a real version dir containing bin/<name> is a candidate"
    );
}

/// Restores a directory's mode on drop so tempdir cleanup can remove it even if
/// an assertion panics.
struct RestoreMode(PathBuf);
impl Drop for RestoreMode {
    fn drop(&mut self) {
        let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
    }
}

#[test]
fn untestable_version_dir_stays_undetermined() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    plant(root, NAME, &["0.1.9", "0.2.0"]);
    let locked = root.join(NAME).join("0.2.0");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    let _guard = RestoreMode(locked.clone());

    // If chmod 000 does not actually deny access (running as root), this case
    // cannot be observed here — skip loudly rather than assert on a fixture
    // that does not exist.
    if std::fs::read_dir(&locked).is_ok() {
        eprintln!("SKIP untestable_version_dir_stays_undetermined: chmod 000 ineffective (root?)");
        return;
    }

    match cache_lookup_in(root, NAME) {
        Determination::Undetermined(_) => {}
        Determination::Known(v) => panic!(
            "a version dir whose bin/ cannot be tested must stay Undetermined, got Known({v:?})"
        ),
    }
}
