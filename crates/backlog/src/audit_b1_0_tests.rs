//! Closure regression tests written by an independent verifier for the
//! backlog-closure audit (batch b1_0). Each test names the backlog id whose
//! closure it proves, asserts the exact property that item describes, and was
//! observed RED with the fix construct removed before being kept GREEN.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "backlog-audit-b1-0-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Hold the tasks-file lock as a LIVE holder (this very test process), so the
/// lock is never reapable and every acquire must exhaust its budget.
fn hold_lock_as_live_holder(tasks: &Path) -> PathBuf {
    let mut s = tasks.as_os_str().to_os_string();
    s.push(".lock");
    let lock = PathBuf::from(s);
    std::fs::write(&lock, format!("{}", std::process::id())).unwrap();
    lock
}

/// backlog 87eda070 + ba8789e0.
///
/// 87eda070: `edit`, `mark_done`, `mark_failed` and `requeue_expired` must NOT
/// run an unprotected read-modify-write when the tasks-file lock cannot be
/// acquired: each returns `Err` naming the refusal and leaves the file
/// byte-identical.
///
/// ba8789e0: the measured acquire budget must sit BELOW the 10s stale-reap
/// window with a real margin (it was measured at ~9.95-10.0s when the budget
/// was `1600 attempts x 5ms`), and a live holder's lock must not be reaped.
#[test]
fn backlog_87eda070_ba8789e0_mutators_refuse_within_budget_when_lock_is_held() {
    let dir = scratch("lock");
    let tasks = dir.join("tasks.toml");
    let now = 1_000;
    let id = crate::store::add(&tasks, "t", "proj", vec![], "n", now).unwrap();
    // A deferred row so requeue_expired would have something to change.
    let _ = crate::store::add(&tasks, "t2", "proj", vec![], "n", now).unwrap();
    let before = std::fs::read(&tasks).unwrap();
    let lock = hold_lock_as_live_holder(&tasks);

    type Op = Box<dyn FnOnce() -> anyhow::Result<()> + Send>;
    let mk = |name: &'static str, f: Op| (name, f);
    let (t1, t2, t3, t4) = (tasks.clone(), tasks.clone(), tasks.clone(), tasks.clone());
    let (i1, i2, i3) = (id.clone(), id.clone(), id.clone());
    let ops: Vec<(&str, Op)> = vec![
        mk(
            "edit",
            Box::new(move || crate::store::edit(&t1, &i1, Some("changed"), None, None, None)),
        ),
        mk(
            "mark_done",
            Box::new(move || crate::store::mark_done(&t2, &i2)),
        ),
        mk(
            "mark_failed",
            Box::new(move || crate::store::mark_failed(&t3, &i3, Some("x"))),
        ),
        mk(
            "requeue_expired",
            Box::new(move || crate::store::requeue_expired(&t4, i64::MAX / 2).map(|_| ())),
        ),
    ];

    let handles: Vec<_> = ops
        .into_iter()
        .map(|(name, f)| {
            std::thread::spawn(move || {
                let started = Instant::now();
                let r = f();
                (name, r, started.elapsed())
            })
        })
        .collect();

    for h in handles {
        let (name, r, elapsed) = h.join().unwrap();
        let err = match r {
            Ok(()) => panic!(
                "87eda070: {name} returned Ok while the tasks lock was held by a live holder — \
                 an unprotected read-modify-write ran"
            ),
            Err(e) => e.to_string(),
        };
        assert!(
            err.contains("refused"),
            "87eda070: {name} must refuse by name, got: {err}"
        );
        assert!(
            elapsed >= Duration::from_secs(7),
            "{name}: refused after only {elapsed:?}; the budget was not spent waiting"
        );
        assert!(
            elapsed < Duration::from_millis(9_500),
            "ba8789e0: {name} waited {elapsed:?} before giving up — no margin below the 10s \
             stale-reap window (TASKS_LOCK_STALE_SECS)"
        );
    }

    assert_eq!(
        std::fs::read(&tasks).unwrap(),
        before,
        "87eda070: a refused mutator must leave the tasks file byte-identical"
    );
    assert_eq!(
        std::fs::read_to_string(&lock).unwrap(),
        format!("{}", std::process::id()),
        "ba8789e0: the LIVE holder's lock must not have been reaped"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// backlog e110b1e8: the PermissionDenied/IO arm of
/// `canonicalize_project_with_marker` must mark the label `unresolved: true`.
/// Flipping it to `false` must turn this test RED (the item reported a zero
/// kill rate for exactly that flip).
#[test]
fn backlog_e110b1e8_permission_denied_project_is_marked_unresolved() {
    use std::os::unix::fs::PermissionsExt;
    let base = scratch("eacces");
    let outer = base.join("outer");
    let inner = outer.join("proj");
    std::fs::create_dir_all(&inner).unwrap();
    std::fs::set_permissions(&outer, std::fs::Permissions::from_mode(0o000)).unwrap();
    let degraded = std::fs::symlink_metadata(&inner)
        .err()
        .is_some_and(|e| e.kind() != std::io::ErrorKind::NotFound);
    let got = crate::store::canonicalize_project_with_marker(&inner.to_string_lossy());
    let _ = std::fs::set_permissions(&outer, std::fs::Permissions::from_mode(0o700));
    let _ = std::fs::remove_dir_all(&base);
    assert!(
        degraded,
        "fixture did not degrade (running as root?) — this test cannot observe the arm"
    );
    assert!(
        got.unresolved,
        "e110b1e8: a project whose existence cannot be checked (EACCES) must be marked \
         unresolved, got label {:?} unresolved=false",
        got.label
    );
}
