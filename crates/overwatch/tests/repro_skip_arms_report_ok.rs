#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Repro: the three `store` mutators that still use `LeaseLock::acquire_or_skip`
//! return `Ok(())` / `Ok(0)` when the lease lock was NOT acquired, although
//! nothing was written. (Their two siblings, `record_changeset_and_detect` and
//! `append_merge_conflict`, were moved to `acquire_strict` and now return `Err`.)
//! The only caller (`condukt::worktree::finalize_landed_branch`) logs on `Err`
//! and says nothing on `Ok(0)`, so a skipped write is invisible; a landed
//! branch's changeset stays `merged=false` and keeps holding later tasks.
//! The lock is held by THIS process (a live pid), so each call waits the full
//! deadline and then takes the skip arm.
use overwatch::changeset::ActualChangeset;
use overwatch::store;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Once;

static INIT: Once = Once::new();

fn sandbox_home() -> PathBuf {
    let home = std::env::temp_dir().join(format!("ow-repro-lockok-home-{}", std::process::id()));
    INIT.call_once(|| {
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        std::env::set_var("HOME", &home);
        std::env::remove_var("HARNESS_SANDBOX_HOME");
    });
    home
}

fn git_repo(name: &str) -> PathBuf {
    let home = sandbox_home();
    let cwd = home.join("proj").join(name);
    std::fs::create_dir_all(&cwd).unwrap();
    assert!(harness_core::git_env::scrub(&mut Command::new("git"))
        .current_dir(&cwd)
        .args(["init", "-q"])
        .status()
        .unwrap()
        .success());
    cwd
}

/// Hold the project's lease lock with a live pid (this process).
fn hold_lock(cwd: &Path) -> PathBuf {
    let lock = store::leases_path(cwd).unwrap().with_extension("lock");
    std::fs::create_dir_all(lock.parent().unwrap()).unwrap();
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
    lock
}

fn seed(cwd: &Path) {
    let cs = ActualChangeset::new(
        "run/t1".into(),
        "run".into(),
        "s1".into(),
        "b1".into(),
        "base".into(),
        "head".into(),
        &["a.rs".to_string()],
        1,
    );
    store::record_changeset_and_detect(cwd, &cs).unwrap();
}

#[test]
fn mark_changeset_merged_unacquired_lock_must_not_return_ok() {
    let cwd = git_repo("mcm");
    seed(&cwd);
    let lock = hold_lock(&cwd);
    let r = store::mark_changeset_merged(&cwd, "run/t1");
    let _ = std::fs::remove_file(&lock);
    assert!(
        r.is_err(),
        "mark_changeset_merged returned {r:?} but the lock was NOT acquired and nothing was marked"
    );
}

#[test]
fn mark_branch_merged_unacquired_lock_must_not_return_ok() {
    let cwd = git_repo("mbm");
    seed(&cwd);
    let lock = hold_lock(&cwd);
    let r = store::mark_branch_merged(&cwd, "b1");
    let _ = std::fs::remove_file(&lock);
    assert!(
        r.is_err(),
        "mark_branch_merged returned {r:?} but the lock was NOT acquired and nothing was marked"
    );
}

#[test]
fn prune_stale_changesets_unacquired_lock_must_not_return_ok() {
    let cwd = git_repo("psc");
    seed(&cwd);
    let lock = hold_lock(&cwd);
    let r = store::prune_stale_changesets(&cwd, i64::MAX / 2);
    let _ = std::fs::remove_file(&lock);
    assert!(r.is_err(), "prune_stale_changesets returned {r:?} but the lock was NOT acquired and nothing was pruned");
}
