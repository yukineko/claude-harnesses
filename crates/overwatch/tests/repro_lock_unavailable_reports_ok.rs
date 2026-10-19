#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Repros: `store::record_changeset_and_detect` and `store::append_merge_conflict`
//! return `Ok` when the lease lock was NOT acquired, although nothing was
//! recorded / written.
//!
//! * `record_changeset_and_detect` -> `Ok(Vec::new())` ("no overlap"); condukt's
//!   `runtime_conflict` then sets no merge-hold, so the merge proceeds
//!   unchecked. An unchecked changeset is UNDETERMINED, not "no overlap".
//! * `append_merge_conflict` -> `Ok(())` with no row written; callers only warn
//!   on `Err`, so a blocked merge is silently never recorded.
//!
//! The lock is held by THIS test process (a live pid), so the call waits the
//! full 10s deadline and then takes the skip arm.
use overwatch::changeset::ActualChangeset;
use overwatch::merge_conflict::{ConflictOrigin, MergeConflictEntry};
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
    assert!(Command::new("git")
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

#[test]
#[ignore = "repro: record_changeset_and_detect returns Ok(empty)=no overlap when lock not acquired; remove ignore when fixed"]
fn record_changeset_and_detect_with_unacquired_lock_must_not_report_no_overlap() {
    let cwd = git_repo("rcd");
    let lock = hold_lock(&cwd);
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
    let r = store::record_changeset_and_detect(&cwd, &cs);
    let _ = std::fs::remove_file(&lock);
    assert!(
        !matches!(&r, Ok(v) if v.is_empty()),
        "record_changeset_and_detect returned Ok(empty) (\"no overlap\") although the lock \
         was NOT acquired and the changeset was never recorded or cross-checked: {r:?}"
    );
}

#[test]
#[ignore = "repro: append_merge_conflict returns Ok(()) with nothing written when lock not acquired; remove ignore when fixed"]
fn append_merge_conflict_with_unacquired_lock_must_not_return_ok_unwritten() {
    let cwd = git_repo("amc");
    let lock = hold_lock(&cwd);
    let entry = MergeConflictEntry {
        conflict_id: "run/b1/1".into(),
        origin: ConflictOrigin::RuntimeOverlap,
        run_id: "run".into(),
        branch: "b1".into(),
        default_branch: "main".into(),
        base_ref: "base".into(),
        conflicted_files: vec!["a.rs".into()],
        diff_ours: String::new(),
        diff_theirs: String::new(),
        ts: 1,
    };
    let r = store::append_merge_conflict(&cwd, &entry);
    let _ = std::fs::remove_file(&lock);
    let recorded = store::read_merge_conflicts(&cwd)
        .map(|v| v.iter().any(|e| e.conflict_id == entry.conflict_id))
        .unwrap_or(false);
    assert!(
        !(r.is_ok() && !recorded),
        "append_merge_conflict returned Ok(()) but nothing was written (lock not acquired): {r:?}"
    );
}
