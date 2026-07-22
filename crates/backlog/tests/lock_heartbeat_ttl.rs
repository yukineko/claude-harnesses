//! Regression test for commit 0f7edbf: lock staleness is judged by
//! `heartbeat_at` + a fixed TTL (`LOCK_STALE_TTL_SECS = 1800`), not by
//! PID-liveness. This drives the real built `backlog` binary end-to-end so it
//! reproduces the fix without needing to wait 30 real minutes: instead, after
//! a genuine `lock acquire`, the test rewrites the on-disk lock file's
//! `heartbeat_at` field directly to a timestamp older than the TTL, simulating
//! the passage of time.
//!
//! Two scenarios:
//! 1. The lock's owning session calls `lock heartbeat`, which must refresh
//!    `heartbeat_at` back to "now" and keep the lock non-stale (a competing
//!    session's acquire attempt must still fail).
//! 2. Without a heartbeat call, a lock file whose `heartbeat_at` was rewritten
//!    past the TTL must be judged stale and be reapable by a different
//!    session's `lock acquire`.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Mirrors `LOCK_STALE_TTL_SECS` in `crates/backlog/src/lock.rs`. Kept as an
/// independent literal (not `include!`d) so this test asserts against the
/// documented contract rather than silently tracking any future refactor of
/// the constant's name/location.
const LOCK_STALE_TTL_SECS: i64 = 1800;

/// A unique, isolated temp HOME so `~/.backlog` (the task store + lock file)
/// is fresh and the real queue/lock is never read or written.
fn temp_home(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "backlog-lock-ttl-{}-{}-{}",
        tag,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Locate the single per-project lock file under `~/.backlog/locks/`. The
/// lock file name is now a hash of the (canonicalized) project string rather
/// than a fixed path, so this test derives it by listing the directory
/// instead of replicating the hash — each test here only ever acquires one
/// project's lock at a time, so exactly one `.lock` file is expected.
fn lock_path(home: &Path) -> PathBuf {
    let locks_dir = home.join(".backlog").join("locks");
    let mut entries: Vec<PathBuf> = std::fs::read_dir(&locks_dir)
        .expect("locks dir readable")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "lock"))
        .collect();
    entries.sort();
    assert_eq!(
        entries.len(),
        1,
        "expected exactly one lock file, found {entries:?}"
    );
    entries.into_iter().next().unwrap()
}

/// Run `backlog <args>` under an isolated HOME. Returns (exit_code, stdout).
fn run(args: &[&str], home: &Path) -> (i32, String) {
    let bin = env!("CARGO_BIN_EXE_backlog");
    let child = Command::new(bin)
        .args(args)
        .env("HOME", home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("binary spawns");
    let out = child.wait_with_output().expect("binary runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Read the on-disk lock file's JSON and return the parsed value.
fn read_lock_json(home: &Path) -> serde_json::Value {
    let txt = std::fs::read_to_string(lock_path(home)).expect("lock file readable");
    serde_json::from_str(&txt).expect("lock file is valid JSON")
}

/// Overwrite the on-disk lock file's `heartbeat_at` field, keeping every
/// other field as-is, so the TTL-exceeded state is simulated without waiting
/// real time.
fn backdate_heartbeat(home: &Path, seconds_ago: i64) {
    let mut v = read_lock_json(home);
    let backdated = now_unix() - seconds_ago;
    v["heartbeat_at"] = serde_json::Value::from(backdated);
    let json = serde_json::to_string_pretty(&v).unwrap();
    let mut f = std::fs::File::create(lock_path(home)).expect("overwrite lock file");
    f.write_all(json.as_bytes()).expect("write lock file");
}

#[test]
fn heartbeat_refreshes_a_ttl_exceeded_lock_and_keeps_it_from_being_stolen() {
    let home = temp_home("refresh");

    // 1. Acquire the lock for "owner".
    let (code, stdout) = run(
        &[
            "lock",
            "acquire",
            "--session-id",
            "owner",
            "--project",
            "/p",
        ],
        &home,
    );
    assert_eq!(code, 0, "initial acquire must succeed, got: {stdout}");

    // 2. Simulate 30+ minutes passing without a heartbeat: rewrite
    //    heartbeat_at on disk to be older than LOCK_STALE_TTL_SECS.
    backdate_heartbeat(&home, LOCK_STALE_TTL_SECS + 60);

    // Sanity: status must now report the lock as stale before we heartbeat it.
    let (code, stdout) = run(&["lock", "status", "--project", "/p"], &home);
    assert_eq!(code, 0);
    assert!(
        stdout.contains("\"stale\": true") || stdout.contains("\"stale\":true"),
        "lock with a TTL-exceeded heartbeat must report stale before refresh, got: {stdout}"
    );

    // 3. The owning session heartbeats — this must refresh heartbeat_at back
    //    to "now" without waiting any real time.
    let (code, stdout) = run(
        &[
            "lock",
            "heartbeat",
            "--session-id",
            "owner",
            "--project",
            "/p",
        ],
        &home,
    );
    assert_eq!(code, 0, "heartbeat must succeed, got: {stdout}");

    // 4. Status must now report Active again (heartbeat_at moved forward).
    let (code, stdout) = run(&["lock", "status", "--project", "/p"], &home);
    assert_eq!(code, 0);
    assert!(
        !stdout.contains("stale"),
        "lock must no longer be stale after heartbeat refresh, got: {stdout}"
    );
    assert!(
        stdout.contains("\"session_id\": \"owner\""),
        "status must still show 'owner' as the holder, got: {stdout}"
    );

    let refreshed = read_lock_json(&home);
    let refreshed_heartbeat = refreshed["heartbeat_at"]
        .as_i64()
        .expect("heartbeat_at is i64");
    assert!(
        now_unix() - refreshed_heartbeat < LOCK_STALE_TTL_SECS,
        "heartbeat_at must have been moved forward within the TTL window, got {refreshed_heartbeat}"
    );

    // 5. A competing session's acquire attempt for the SAME project must
    //    still fail: the lock is active again thanks to the heartbeat, not
    //    stealable. (A different project would trivially succeed now that
    //    locks are per-project — that's not what this is testing.)
    let (code, stdout) = run(
        &[
            "lock",
            "acquire",
            "--session-id",
            "competitor",
            "--project",
            "/p",
        ],
        &home,
    );
    assert_ne!(
        code, 0,
        "a freshly-heartbeated lock must not be stealable by another session, got: {stdout}"
    );

    // The lock must still be held by "owner", unchanged.
    let still_owner = read_lock_json(&home);
    assert_eq!(still_owner["session_id"], "owner");
}

#[test]
fn without_a_heartbeat_a_ttl_exceeded_lock_is_reaped_by_a_different_session() {
    let home = temp_home("reap");

    // 1. Acquire the lock for "owner".
    let (code, stdout) = run(
        &[
            "lock",
            "acquire",
            "--session-id",
            "owner",
            "--project",
            "/p",
        ],
        &home,
    );
    assert_eq!(code, 0, "initial acquire must succeed, got: {stdout}");

    // 2. Simulate 30+ minutes passing without any heartbeat call: rewrite
    //    heartbeat_at on disk to be older than LOCK_STALE_TTL_SECS.
    backdate_heartbeat(&home, LOCK_STALE_TTL_SECS + 60);

    // Sanity: status must report stale.
    let (code, stdout) = run(&["lock", "status", "--project", "/p"], &home);
    assert_eq!(code, 0);
    assert!(
        stdout.contains("\"stale\": true") || stdout.contains("\"stale\":true"),
        "lock with a TTL-exceeded heartbeat must report stale, got: {stdout}"
    );

    // 3. No heartbeat call is made here (contrast with the other test). A
    //    different session's acquire for the SAME project must reap the
    //    stale lock and win it. (A different project would trivially
    //    succeed now that locks are per-project, regardless of staleness —
    //    that's not what this is testing.)
    let (code, stdout) = run(
        &[
            "lock",
            "acquire",
            "--session-id",
            "rescuer",
            "--project",
            "/p",
        ],
        &home,
    );
    assert_eq!(
        code, 0,
        "acquire by a different session must succeed over a TTL-exceeded (stale) lock, got: {stdout}"
    );

    let (code, stdout) = run(&["lock", "status", "--project", "/p"], &home);
    assert_eq!(code, 0);
    assert!(
        !stdout.contains("stale"),
        "status must not report stale immediately after a fresh acquire, got: {stdout}"
    );
    assert!(
        stdout.contains("\"session_id\": \"rescuer\""),
        "the reaping session must now hold the lock, got: {stdout}"
    );
}
