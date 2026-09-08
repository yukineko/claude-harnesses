// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! MIRROR of `crates/donegate/tests/session_scoped_skip.rs`, for propguard.
//!
//! This file exists because of the failure mode this repo keeps repeating: a
//! fix lands on ONE of several copies of the same path and the audit reads as
//! converged. Four Stop gates (donegate, propguard, reviewgate, tdd) consumed
//! four project-root markers through one shared primitive, so a change that
//! only rewires donegate leaves three live shared hatches while donegate's own
//! end-to-end suite goes green.
//!
//! The defect is identical: `consume_skip(&root, ".propguard-skip")` reads an
//! unattributed file from the SHARED project root, so whichever session's Stop
//! hook fires next consumes it — that session's legitimate gate waved through
//! on an exception it never asked for (CLAUDE.md §5).
//!
//! Written by a DISINTERESTED author (CLAUDE.md §2(a)).
//!
//! ## Isolation
//!
//! Each test gets its own directory under `std::env::temp_dir()`, passed to the
//! child as both `HOME` and `PROPGUARD_STATE_DIR` (the belt-and-suspenders
//! idiom already used by `tests/integration.rs`: `dirs::home_dir()` ignores a
//! `HOME` override on Windows, so the state dir is pinned explicitly too).
//! Nothing here touches live `~/.propguard` or `~/.overwatch` state, and no
//! process-global environment variable is mutated.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

const CRITERIA: &str = "idempotent; never panic; stable output schema";

fn scratch(tag: &str) -> PathBuf {
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("propguard-skip-{tag}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch");
    dir
}

/// A git repo with one changed source file — the state in which propguard has
/// something to verify and therefore something to block on.
fn project(tag: &str) -> PathBuf {
    let dir = scratch(tag);
    for args in [
        vec!["init", "-q"],
        vec!["config", "user.email", "t@t"],
        vec!["config", "user.name", "t"],
    ] {
        let _ = Command::new("git").current_dir(&dir).args(&args).output();
    }
    std::fs::write(dir.join("a.rs"), "fn f() { panic!() }\n").expect("write source");
    dir
}

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

impl Run {
    fn blocked(&self) -> bool {
        self.stdout.contains("\"decision\": \"block\"")
            || self.stdout.contains("\"decision\":\"block\"")
    }
}

fn run(dir: &Path, args: &[&str], session: Option<&str>, stdin: &str) -> Run {
    let bin = env!("CARGO_BIN_EXE_propguard");
    let mut cmd = Command::new(bin);
    cmd.args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .env("PROPGUARD_STATE_DIR", dir.join(".propguard-state"))
        .env("PROPGUARD_CRITERIA", CRITERIA)
        .env_remove("PROPGUARD_DISABLE");
    match session {
        Some(s) => {
            cmd.env("CLAUDE_CODE_SESSION_ID", s);
        }
        None => {
            cmd.env_remove("CLAUDE_CODE_SESSION_ID");
        }
    }
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("propguard spawns");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(stdin.as_bytes())
        .expect("write stdin");
    let out = child.wait_with_output().expect("propguard runs");
    Run {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

fn stop(dir: &Path, session: &str) -> Run {
    let payload = serde_json::json!({
        "session_id": session,
        "cwd": dir.to_string_lossy(),
        "hook_event_name": "Stop",
    })
    .to_string();
    run(dir, &["check"], Some(session), &payload)
}

fn issue_skip(dir: &Path, session: &str, reason: &str) -> Run {
    run(dir, &["skip", "--reason", reason], Some(session), "")
}

/// ANTI-VACUITY CONTROL: with no skip of any kind, propguard blocks. Every
/// "must block" assertion below is meaningless without this.
#[test]
fn with_no_skip_at_all_propguard_still_blocks() {
    let dir = project("control");
    let r = stop(&dir, "sess-control");
    assert_eq!(r.code, 0, "a Stop hook always exits 0 toward Claude");
    assert!(
        r.blocked(),
        "apparatus: propguard must block here, else this whole file proves nothing; stdout={:?} \
         stderr={:?}",
        r.stdout,
        r.stderr
    );
}

/// THE MIRROR-GAP ASSERTION: an unattributed marker in the shared project root
/// must not wave propguard's stop through either.
#[test]
fn an_unattributed_project_root_marker_no_longer_waves_a_propguard_stop_through() {
    let dir = project("shared-marker");
    std::fs::write(
        dir.join(".propguard-skip"),
        "left here by some other session\n",
    )
    .expect("write the legacy shared marker");

    let r = stop(&dir, "sess-victim");
    assert_eq!(r.code, 0);
    assert!(
        r.blocked(),
        "a file in the SHARED project root waved this session's propguard stop through. The fix \
         landed on donegate but not here — this is the mirror gap. stdout={:?} stderr={:?}",
        r.stdout,
        r.stderr
    );
}

/// The two-session form, through the supported CLI: A's skip must be invisible
/// to B, and must still be waiting for A.
#[test]
fn a_skip_issued_by_another_session_does_not_wave_my_propguard_gate_through() {
    let dir = project("cross-session");

    let issued = issue_skip(&dir, "sess-A", "landing a doc-only fix");
    assert_eq!(
        issued.code, 0,
        "`propguard skip --reason …` must succeed for the issuing session; stdout={:?} \
         stderr={:?}",
        issued.stdout, issued.stderr
    );

    let b = stop(&dir, "sess-B");
    assert!(
        b.blocked(),
        "session B's legitimate propguard gate was waved through by a skip session A asked for. \
         stdout={:?} stderr={:?}",
        b.stdout,
        b.stderr
    );

    let a = stop(&dir, "sess-A");
    assert!(
        !a.blocked(),
        "session A's own skip did not survive session B's stop; stdout={:?} stderr={:?}",
        a.stdout,
        a.stderr
    );
}
