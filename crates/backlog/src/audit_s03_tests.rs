// Audit repro/regression tests added by shard s03-backlog (2026-10-02).
// include!d from a child module of store.rs so it can reach private items.

use super::*;

/// backlog ba8789e0: the doc comments claim the acquire budget is a
/// "comfortable margin BELOW TASKS_LOCK_STALE_SECS". The item measured
/// 9.95-10.0s under the old attempts x sleep arithmetic. This measures the
/// REAL wait against a holder that is provably alive (our own pid), so the
/// lock can never be reaped and the whole budget is spent.
#[test]
fn acquire_budget_is_measured_below_the_stale_window() {
    let dir = tempfile::tempdir().expect("tmp dir");
    let path = dir.path().join("tasks.toml");
    let lock_path = tasks_lock_path(&path);
    std::fs::write(&lock_path, std::process::id().to_string()).expect("write lockfile");

    let started = std::time::Instant::now();
    let got = try_acquire_tasks_lock(&path);
    let elapsed = started.elapsed();
    assert!(
        got.is_none(),
        "a lock held by a live pid must not be acquired"
    );
    assert!(
        elapsed + Duration::from_secs(1) < Duration::from_secs(TASKS_LOCK_STALE_SECS),
        "measured acquire budget {elapsed:?} leaves no margin below the {TASKS_LOCK_STALE_SECS}s stale window"
    );
    assert!(
        elapsed >= TASKS_LOCK_BUDGET,
        "the budget was cut short: {elapsed:?} < {TASKS_LOCK_BUDGET:?}"
    );
}

/// backlog b14d433c: when the lockfile cannot even be CREATED (EACCES, here
/// a read-only store dir) the refusal blames CONTENTION ("could not be
/// acquired within 8s") although it returned instantly, and never names the
/// OS error. "The lock mechanism is broken" is not "someone else holds it".
#[test]
#[ignore = "backlog b14d433c: open defect, remove ignore when fixed"]
fn lock_io_error_is_not_reported_as_contention() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().expect("tmp dir");
    let store_dir = dir.path().join(".backlog");
    std::fs::create_dir_all(&store_dir).unwrap();
    let path = store_dir.join("tasks.toml");
    std::fs::write(&path, "").unwrap();
    std::fs::set_permissions(&store_dir, std::fs::Permissions::from_mode(0o555)).unwrap();
    // If this environment can still create files there (root), the probe is void.
    let probe = std::fs::File::create(store_dir.join("probe"));
    if probe.is_ok() {
        let _ = std::fs::set_permissions(&store_dir, std::fs::Permissions::from_mode(0o755));
        panic!("fixture void: could create a file in a mode-555 dir");
    }

    let started = std::time::Instant::now();
    let res = with_tasks_lock_required(&path, "done", || Ok(()));
    let elapsed = started.elapsed();
    let _ = std::fs::set_permissions(&store_dir, std::fs::Permissions::from_mode(0o755));

    let msg = res.expect_err("must refuse (fail-closed)").to_string();
    assert!(
        elapsed < Duration::from_secs(2),
        "precondition: an EACCES refusal returns immediately, took {elapsed:?}"
    );
    assert!(
        msg.to_lowercase().contains("permission denied") || !msg.contains("within 8s"),
        "the refusal claims a {}s contention timeout for an instant IO error and hides the OS cause: {msg}",
        TASKS_LOCK_BUDGET.as_secs()
    );
}
