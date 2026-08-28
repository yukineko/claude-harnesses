//! End-to-end proof that the cap holds against *real* concurrent processes.
//!
//! The unit tests in `src/model.rs` prove the arithmetic. They cannot prove the
//! thing that actually decides whether this gate works: Claude Code runs the
//! `PreToolUse` hooks of a parallel tool batch as separate OS processes at the
//! same time, so the ledger is a shared mutable file with N writers. A
//! read-modify-write without mutual exclusion admits more than the cap under
//! exactly that load, and passes every single-threaded test.
//!
//! So these tests launch the real binary N times concurrently, against one
//! ledger, and count how many were admitted.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_parallelguard");
const SESSION: &str = "concurrency-test-session";

/// One hook payload as Claude Code would send it.
fn payload(tool: &str, command: &str) -> String {
    serde_json::json!({
        "session_id": SESSION,
        "hook_event_name": "PreToolUse",
        "tool_name": tool,
        "tool_input": { "command": command },
    })
    .to_string()
}

fn spawn(state: &Path, subcommand: &str, body: String) -> std::process::Child {
    let mut child = Command::new(BIN)
        .arg(subcommand)
        .env("PARALLELGUARD_STATE_DIR", state)
        .env_remove("HARNESS_MAX_PARALLEL")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the parallelguard binary should start");
    let mut stdin = child.stdin.take().expect("stdin was piped");
    std::thread::spawn(move || {
        let _ = stdin.write_all(body.as_bytes());
    });
    child
}

fn run(state: &Path, subcommand: &str, body: String) -> String {
    let out = spawn(state, subcommand, body)
        .wait_with_output()
        .expect("the parallelguard binary should finish");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn is_deny(stdout: &str) -> bool {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(stdout.trim()) else {
        return false;
    };
    v["hookSpecificOutput"]["permissionDecision"] == "deny"
}

/// Launch `n` acquires at once and report (admitted, denied).
fn race(state: &Path, tool: &str, n: usize) -> (usize, usize) {
    let children: Vec<_> = (0..n)
        .map(|i| spawn(state, "acquire", payload(tool, &format!("cmd-{i}"))))
        .collect();
    let mut admitted = 0;
    let mut denied = 0;
    for c in children {
        let out = c.wait_with_output().expect("child should finish");
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        assert_eq!(
            out.status.code(),
            Some(0),
            "a hook must always exit 0; stderr was: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        if is_deny(&stdout) {
            denied += 1;
        } else {
            assert!(
                stdout.trim().is_empty(),
                "an admitted call must print nothing (silence IS the allow), got: {stdout}"
            );
            admitted += 1;
        }
    }
    (admitted, denied)
}

#[test]
fn eight_simultaneous_shells_admit_exactly_three() {
    let dir = tempfile::tempdir().unwrap();
    let (admitted, denied) = race(dir.path(), "Bash", 8);
    assert_eq!(
        admitted, 3,
        "the cap is 3; {admitted} concurrent shells were admitted (denied: {denied})"
    );
    assert_eq!(denied, 5);
}

#[test]
fn eight_simultaneous_subagents_admit_exactly_three() {
    let dir = tempfile::tempdir().unwrap();
    let (admitted, denied) = race(dir.path(), "Task", 8);
    assert_eq!(admitted, 3, "denied: {denied}");
}

#[test]
fn a_full_shell_pool_never_blocks_a_subagent() {
    // The deadlock guard, end to end: three live shells must not stop a Task,
    // because a subagent that cannot run its own Bash can never release the
    // slot it is holding.
    let dir = tempfile::tempdir().unwrap();
    let (admitted, _) = race(dir.path(), "Bash", 3);
    assert_eq!(admitted, 3);
    let out = run(dir.path(), "acquire", payload("Task", "spawn an agent"));
    assert!(
        !is_deny(&out),
        "a subagent was refused because shells were busy: {out}"
    );
}

#[test]
fn releasing_returns_the_slot() {
    let dir = tempfile::tempdir().unwrap();
    for i in 0..3 {
        let out = run(dir.path(), "acquire", payload("Bash", &format!("c{i}")));
        assert!(!is_deny(&out), "call {i} should have been admitted: {out}");
    }
    assert!(is_deny(&run(dir.path(), "acquire", payload("Bash", "c3"))));
    run(dir.path(), "release", payload("Bash", "c0"));
    let out = run(dir.path(), "acquire", payload("Bash", "c3"));
    assert!(
        !is_deny(&out),
        "after a release there should be room again: {out}"
    );
}

#[test]
fn reset_clears_a_leaked_ledger() {
    // The recovery path every fail-closed deny depends on: whatever leaked is
    // gone at the next turn boundary, with no human involved.
    let dir = tempfile::tempdir().unwrap();
    let (admitted, _) = race(dir.path(), "Bash", 3);
    assert_eq!(admitted, 3);
    assert!(is_deny(&run(dir.path(), "acquire", payload("Bash", "x"))));
    run(dir.path(), "reset", payload("Bash", "irrelevant"));
    assert!(
        !is_deny(&run(dir.path(), "acquire", payload("Bash", "x"))),
        "reset must clear the ledger"
    );
}

#[test]
fn an_unreadable_ledger_denies_rather_than_reading_as_empty() {
    let dir = tempfile::tempdir().unwrap();
    let ledger = dir.path().join("sessions").join(format!("{SESSION}.json"));
    std::fs::create_dir_all(ledger.parent().unwrap()).unwrap();
    std::fs::write(&ledger, b"{ truncated").unwrap();
    let out = run(dir.path(), "acquire", payload("Bash", "anything"));
    assert!(
        is_deny(&out),
        "a corrupt ledger must deny, not admit: {out}"
    );
    assert!(out.contains("could not determine"), "{out}");
}

#[test]
fn an_unparseable_payload_denies() {
    let dir = tempfile::tempdir().unwrap();
    let out = run(dir.path(), "acquire", "not json at all".to_string());
    assert!(is_deny(&out), "{out}");
}

#[test]
fn an_unmetered_tool_passes_through() {
    let dir = tempfile::tempdir().unwrap();
    let body = serde_json::json!({
        "session_id": SESSION,
        "tool_name": "Read",
        "tool_input": { "file_path": "/etc/hosts" },
    })
    .to_string();
    for _ in 0..10 {
        let out = run(dir.path(), "acquire", body.clone());
        assert!(out.trim().is_empty(), "Read must not be metered: {out}");
    }
}

#[test]
fn the_cap_can_be_lowered_but_not_raised() {
    let dir = tempfile::tempdir().unwrap();
    let mut child = Command::new(BIN)
        .arg("acquire")
        .env("PARALLELGUARD_STATE_DIR", dir.path())
        .env("HARNESS_MAX_PARALLEL", "99")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    {
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(payload("Bash", "c0").as_bytes()).unwrap();
    }
    child.wait().unwrap();
    // With the ceiling honored, one slot is taken and the cap is still 3, so
    // two more fit and the fourth does not.
    let (admitted, _) = race(dir.path(), "Bash", 8);
    assert_eq!(
        admitted, 2,
        "HARNESS_MAX_PARALLEL=99 must not raise the ceiling above 3"
    );
}

#[test]
fn status_is_readable_and_never_silently_empty() {
    let dir = tempfile::tempdir().unwrap();
    let out = Command::new(BIN)
        .arg("status")
        .env("PARALLELGUARD_STATE_DIR", dir.path())
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("parallelguard"), "{text}");
    assert!(
        text.contains("the gate is NOT running"),
        "an empty store must be reported as unproven, not as idle: {text}"
    );
}

#[test]
fn the_launcher_denies_when_no_binary_is_bundled() {
    // The dark-failure path: a launcher that exits 0 with no stdout is an
    // allow. Mirrors crates/stuckguard/tests/launcher_missing_binary.rs.
    let launcher = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("bin")
        .join("parallelguard");
    let dir = tempfile::tempdir().unwrap();
    // Copy the launcher somewhere with no sibling platform binary.
    let alone = dir.path().join("parallelguard");
    std::fs::copy(&launcher, &alone).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&alone, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let out = Command::new("sh")
        .arg(&alone)
        .arg("acquire")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        is_deny(&stdout),
        "a missing platform binary must deny, not fall through silently: {stdout}"
    );
    assert_eq!(out.status.code(), Some(0));

    for sub in ["release", "reset"] {
        let out = Command::new("sh").arg(&alone).arg(sub).output().unwrap();
        assert_eq!(out.status.code(), Some(0), "{sub} must exit 0");
        assert!(
            String::from_utf8_lossy(&out.stdout).trim().is_empty(),
            "{sub} has no permission channel and must print nothing"
        );
    }

    let out = Command::new("sh")
        .arg(&alone)
        .arg("status")
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(1),
        "an operator command that printed nothing must not report success"
    );
}
