//! Regression test for commit 0f7edbf: lock staleness is judged by
//! `heartbeat_at` + a fixed TTL (`LOCK_STALE_TTL_SECS = 1800`), not by
//! PID-liveness. This drives the real built `backlog` binary end-to-end so it
//! reproduces the fix without needing to wait 30 real minutes: instead, after
//! a genuine `lock acquire`, the test rewrites the on-disk lock file's
//! `heartbeat_at` field directly to a timestamp older than the TTL, simulating
//! the passage of time.
//!
//! Scenarios:
//! 1. The lock's owning session calls `lock heartbeat`, which must refresh
//!    `heartbeat_at` back to "now" and keep the lock non-stale (a competing
//!    session's acquire attempt must still fail).
//! 2. NEW CONTRACT (progress-gated reap): a TTL-exceeded heartbeat is no longer
//!    sufficient to reap. When the holder's progress cannot be confirmed stalled
//!    (signals unreadable ⇒ Undetermined), a plain acquire must REFUSE; only
//!    `--force` reaps. This is the protective fix — a stale heartbeat can
//!    accompany a live-but-quiet holder.
//! 3. The genuine reap path: with a real git-repo project + a discoverable
//!    transcript both FROZEN, and the multi-sample window collapsed via
//!    `HARNESS_PROGRESS_WINDOW_SECS=0`, the second acquire attempt confirms the
//!    stall and reaps.

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

// NEW CONTRACT (progress-gated reap): a TTL-exceeded heartbeat is NO LONGER
// sufficient to reap. When the holder's PROGRESS cannot be confirmed stalled —
// here `/p` is not a git repo and the holder has no discoverable transcript, so
// both progress signals are Undetermined — a plain acquire by a different
// session must REFUSE (protecting a possibly-live holder whose heartbeat merely
// lapsed). Only `--force` (the human override) reaps it. This is the E2E form of
// the memory scar this change closes.
#[test]
fn ttl_exceeded_lock_with_unconfirmable_progress_is_not_reaped_but_force_wins() {
    let home = temp_home("noreap");

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

    // Simulate 30+ minutes passing without any heartbeat.
    backdate_heartbeat(&home, LOCK_STALE_TTL_SECS + 60);

    let (code, stdout) = run(&["lock", "status", "--project", "/p"], &home);
    assert_eq!(code, 0);
    assert!(
        stdout.contains("\"stale\": true") || stdout.contains("\"stale\":true"),
        "lock with a TTL-exceeded heartbeat must report stale, got: {stdout}"
    );

    // A plain acquire by a different session must FAIL: progress is
    // Undetermined (signals unreadable), which is protective, not a reap.
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
    assert_ne!(
        code, 0,
        "a stale-heartbeat holder with unconfirmable progress must NOT be reaped, got: {stdout}"
    );
    let held = read_lock_json(&home);
    assert_eq!(held["session_id"], "owner", "holder must be unchanged");

    // --force is the escape hatch and must take it over.
    let (code, stdout) = run(
        &[
            "lock",
            "acquire",
            "--session-id",
            "rescuer",
            "--project",
            "/p",
            "--force",
        ],
        &home,
    );
    assert_eq!(
        code, 0,
        "--force must reap regardless of progress, got: {stdout}"
    );
    let now_held = read_lock_json(&home);
    assert_eq!(now_held["session_id"], "rescuer");
}

/// Set up a fake Claude transcript for `session` under `home/.claude/projects`
/// so `session_transcript_signal` finds a (frozen) transcript. Returns the file.
fn seed_transcript(home: &Path, session: &str) -> PathBuf {
    let dir = home.join(".claude").join("projects").join("-fake-cwd");
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join(format!("{session}.jsonl"));
    std::fs::write(&f, "{\"type\":\"user\"}\n").unwrap();
    f
}

/// `git init` a project dir with one commit, so `git_head_signal` reads a real
/// (and, absent further commits, frozen) HEAD.
fn git_repo(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "backlog-progress-repo-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let git = |args: &[&str]| {
        let ok = Command::new("git")
            .arg("-C")
            .arg(&dir)
            .args(args)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {args:?} failed");
    };
    git(&["init", "-q"]);
    std::fs::write(dir.join("a.txt"), "1").unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "one"]);
    dir
}

// The GENUINE reap path E2E: a real git-repo project + a discoverable transcript
// that both stay FROZEN. With the multi-sample window collapsed to 0 (via
// HARNESS_PROGRESS_WINDOW_SECS, the configurable knob), the first acquire attempt
// anchors the fingerprint (Undetermined ⇒ no reap) and the second — still frozen,
// window elapsed ⇒ confirmed Stalled — reaps and wins.
#[test]
fn frozen_signals_across_the_window_are_confirmed_stalled_and_reaped() {
    let home = temp_home("stalled-reap");
    let repo = git_repo("stalled");
    let project = repo.to_string_lossy().to_string();
    seed_transcript(&home, "owner");

    let acquire = |session: &str| -> (i32, String) {
        let bin = env!("CARGO_BIN_EXE_backlog");
        let out = Command::new(bin)
            .args([
                "lock",
                "acquire",
                "--session-id",
                session,
                "--project",
                &project,
            ])
            .env("HOME", &home)
            .env("HARNESS_PROGRESS_WINDOW_SECS", "0") // collapse the window
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap()
            .wait_with_output()
            .unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
        )
    };

    assert_eq!(acquire("owner").0, 0, "owner acquires");
    backdate_heartbeat(&home, LOCK_STALE_TTL_SECS + 60);

    // Sample 1: first observation of the frozen fingerprint ⇒ Undetermined ⇒
    // the rescuer must NOT reap yet (protective on the first sample).
    let (code1, out1) = acquire("rescuer");
    assert_ne!(
        code1, 0,
        "first attempt must not reap (single sample), got: {out1}"
    );
    assert_eq!(read_lock_json(&home)["session_id"], "owner");

    // Sample 2: still frozen, window (0s) elapsed ⇒ confirmed Stalled ⇒ reap.
    let (code2, out2) = acquire("rescuer");
    assert_eq!(
        code2, 0,
        "second attempt over a confirmed-stalled holder must reap, got: {out2}"
    );
    assert_eq!(read_lock_json(&home)["session_id"], "rescuer");

    let _ = std::fs::remove_dir_all(&repo);
}
