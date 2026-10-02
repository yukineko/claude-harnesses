#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro for backlog 1c66598a: a Stop gate that panics DETERMINISTICALLY on
//! every stop gets one fail-closed block (first stop), and from then on every
//! post-block re-entry (`stop_hook_active == true`) is a `BoundedAllow`: exit 0,
//! no decision, a stderr line, and NO persistent trace. Repeated forever, the
//! block silently turns into "continue", and nothing durable tells anyone that
//! the gate has not actually judged a stop for N rounds.
//!
//! The panic path ends in `process::exit`, so it is observed out of process:
//! the test re-executes this test binary into [`child_panicking_gate`] (gated by
//! an env var so it is a no-op in a normal run) with an isolated HOME/XDG
//! state, three times on re-entry, and asserts that the repeated BoundedAllow
//! is either not an allow every time or leaves SOME persistent record.

use std::path::{Path, PathBuf};
use std::process::Command;

const CHILD_ENV: &str = "BACKLOG_1C66598A_CHILD";

/// The re-exec target. Outside the parent's re-exec it returns immediately.
#[test]
fn child_panicking_gate() {
    if std::env::var_os(CHILD_ENV).is_none() {
        return;
    }
    let active = std::env::var("BACKLOG_1C66598A_ACTIVE").as_deref() == Ok("1");
    harness_core::gate::run::run_guarded("probe-gate", false, active, || -> () {
        panic!("deterministic gate panic (backlog 1c66598a probe)")
    });
}

fn scratch() -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let d = std::env::temp_dir().join(format!(
        "hc-backlog-1c66598a-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn run_child(root: &Path, active: bool) -> std::process::Output {
    Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "child_panicking_gate",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD_ENV, "1")
        .env("BACKLOG_1C66598A_ACTIVE", if active { "1" } else { "0" })
        .env("HOME", root.join("home"))
        .env("XDG_STATE_HOME", root.join("state"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("CLAUDE_CODE_SESSION_ID", "s-1c66598a")
        .current_dir(root.join("cwd"))
        .output()
        .unwrap()
}

fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(files_under(&p));
        } else {
            out.push(p);
        }
    }
    out
}

#[test]
#[ignore = "backlog 1c66598a: open defect, remove ignore when fixed"]
fn backlog_1c66598a_repeated_bounded_allow_is_not_silent() {
    let root = scratch();
    for d in ["home", "state", "data", "config", "cwd"] {
        std::fs::create_dir_all(root.join(d)).unwrap();
    }
    // Control: the first stop fails CLOSED (block decision on stdout).
    let first = run_child(&root, false);
    let first_out = String::from_utf8_lossy(&first.stdout).to_string();
    assert!(
        first_out.contains("\"decision\":\"block\""),
        "control: first-stop panic must block: {first:?}"
    );
    // The same deterministic panic on three consecutive re-entries.
    let mut allowed_silently = 0;
    for _ in 0..3 {
        let o = run_child(&root, true);
        let out = String::from_utf8_lossy(&o.stdout).to_string();
        if o.status.code() == Some(0) && !out.contains("\"decision\":\"block\"") {
            allowed_silently += 1;
        }
    }
    let trace: Vec<PathBuf> = ["home", "state", "data", "config", "cwd"]
        .iter()
        .flat_map(|d| files_under(&root.join(d)))
        .collect();
    let _ = std::fs::remove_dir_all(&root);
    assert!(
        allowed_silently < 3 || !trace.is_empty(),
        "a gate panicking on every stop was allowed on {allowed_silently}/3 re-entries \
         and left no persistent record anywhere (files={trace:?})"
    );
}
