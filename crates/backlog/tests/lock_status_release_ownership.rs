//! Observed live 2026-10-20 (main 9b188d57):
//! F1. `lock status` printed kind=driver-presence and hid an exclusive lock
//!     that `next --claim` honours (refuses).
//! F2. `lock release --project P` deleted another session's lock file; `release`
//!     takes no session id, so it cannot prove ownership.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn temp_home(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "backlog-lockown-{}-{}-{}",
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

fn run(args: &[&str], home: &Path, session: Option<&str>) -> (i32, String, String) {
    let mut c = Command::new(env!("CARGO_BIN_EXE_backlog"));
    harness_core::git_env::scrub(&mut c);
    c.args(args)
        .env("HOME", home)
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(s) = session {
        c.env("CLAUDE_CODE_SESSION_ID", s);
    }
    let out = c.output().expect("binary runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn lock_files(home: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(home.join(".backlog").join("locks"))
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|x| x == "lock"))
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn status_reports_exclusive_lock_held_by_another_session() {
    let home = temp_home("f1");
    let (c, o, e) = run(
        &[
            "lock",
            "acquire",
            "--session-id",
            "holderA",
            "--project",
            "/p",
        ],
        &home,
        None,
    );
    assert_eq!(c, 0, "acquire A: {o} {e}");
    let (c, o, e) = run(
        &[
            "driver",
            "register",
            "--session-id",
            "driverB",
            "--project",
            "/p",
        ],
        &home,
        None,
    );
    assert_eq!(c, 0, "register B: {o} {e}");
    // The live case: holder's heartbeat is stale (TTL 1800s) and its pid is dead.
    let lf = lock_files(&home).remove(0);
    let mut v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&lf).unwrap()).unwrap();
    v["heartbeat_at"] = serde_json::Value::from(1_700_000_000_i64);
    v["pid"] = serde_json::Value::from(999_999_i64);
    std::fs::write(&lf, serde_json::to_string_pretty(&v).unwrap()).unwrap();
    // Control: the lock is real and excludes B (claim_permitted semantics are
    // exercised elsewhere; here the lock file existing is the precondition).
    assert_eq!(lock_files(&home).len(), 1, "exclusive lock file exists");

    let (c, o, e) = run(
        &["lock", "status", "--project", "/p"],
        &home,
        Some("driverB"),
    );
    assert_eq!(c, 0, "status: {o} {e}");
    assert!(
        o.contains("holderA"),
        "lock status must surface the exclusive lock holder (session holderA) that next --claim \
         honours; got:\n{o}"
    );
}

#[test]
fn release_by_non_holder_does_not_delete_the_lock() {
    let home = temp_home("f2");
    let (c, o, e) = run(
        &[
            "lock",
            "acquire",
            "--session-id",
            "holderA",
            "--project",
            "/p",
        ],
        &home,
        None,
    );
    assert_eq!(c, 0, "acquire A: {o} {e}");
    let (c, _, _) = run(
        &[
            "lock",
            "acquire",
            "--session-id",
            "otherB",
            "--project",
            "/p",
        ],
        &home,
        None,
    );
    assert_ne!(c, 0, "B's acquire must be refused while A holds the lock");

    // B (identified via CLAUDE_CODE_SESSION_ID, the caller identity `next --claim`
    // uses) runs release, which has no session-id flag.
    let _ = run(
        &["lock", "release", "--project", "/p"],
        &home,
        Some("otherB"),
    );
    assert_eq!(
        lock_files(&home).len(),
        1,
        "a non-holder's `lock release` must not delete session A's lock file"
    );
}
