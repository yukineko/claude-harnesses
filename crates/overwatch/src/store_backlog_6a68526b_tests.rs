//! `store::backlog_6a68526b` tests (see the module doc in `store.rs`), moved
//! verbatim out of `store.rs` (file-growth ratchet).

use super::*;
use crate::merge_conflict::ConflictOrigin;

const BRANCH: &str = "condukt/t-6a68526b";

struct Sandbox {
    prev_home: Option<std::ffi::OsString>,
    _home: tempfile::TempDir,
    cwd: tempfile::TempDir,
    _g: std::sync::MutexGuard<'static, ()>,
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        match self.prev_home.take() {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
    }
}

/// Sandboxed HOME + cwd with one OPEN RuntimeOverlap hold on `BRANCH`.
fn sandbox_with_open_hold() -> Sandbox {
    let g = HOME_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let prev_home = std::env::var_os("HOME");
    let home = tempfile::tempdir().unwrap();
    std::env::set_var("HOME", home.path());
    let cwd = tempfile::tempdir().unwrap();
    let entry = MergeConflictEntry {
        conflict_id: "c-6a68526b".to_string(),
        origin: ConflictOrigin::RuntimeOverlap,
        run_id: "runA".to_string(),
        branch: BRANCH.to_string(),
        default_branch: "main".to_string(),
        base_ref: "base".to_string(),
        conflicted_files: vec!["shared.rs".to_string()],
        diff_ours: "ours".to_string(),
        diff_theirs: "theirs".to_string(),
        ts: 1,
        task_named: None,
    };
    append_merge_conflict(cwd.path(), &entry).unwrap();
    assert_eq!(open_merge_conflicts(cwd.path()).unwrap().len(), 1);
    Sandbox {
        prev_home,
        _home: home,
        cwd,
        _g: g,
    }
}

/// Control: with nothing in the way the hold is cleared and counted.
#[test]
fn clean_store_clears_and_counts_the_hold() {
    let sb = sandbox_with_open_hold();
    assert_eq!(
        clear_runtime_overlap_holds(sb.cwd.path(), BRANCH, 10).unwrap(),
        1
    );
    assert!(open_merge_conflicts(sb.cwd.path()).unwrap().is_empty());
}

/// A corrupt resolution ledger makes the dedup undeterminable: the call
/// must ERROR, not report "cleared 0" (or 1) as if it had looked.
#[test]
fn undeterminable_resolution_ledger_is_an_error_not_a_count() {
    let sb = sandbox_with_open_hold();
    let p = merge_conflict_resolutions_path(sb.cwd.path()).unwrap();
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, "{not-valid-json\n").unwrap();
    let r = clear_runtime_overlap_holds(sb.cwd.path(), BRANCH, 10);
    assert!(
        r.is_err(),
        "SkippedUndetermined must surface as Err, got {r:?}"
    );
}

/// A contended store lock skips the append: nothing was persisted, so the
/// hold must NOT be counted as cleared.
#[test]
fn contended_lock_is_not_counted_as_cleared() {
    let sb = sandbox_with_open_hold();
    let held = LeaseLock::acquire_or_skip(sb.cwd.path()).expect("hold the store lock");
    let r = clear_runtime_overlap_holds(sb.cwd.path(), BRANCH, 10).unwrap();
    drop(held);
    assert_eq!(r, 0, "a contended (unpersisted) resolution was counted");
    assert_eq!(
        open_merge_conflicts(sb.cwd.path()).unwrap().len(),
        1,
        "precondition: the hold really is still open"
    );
}
