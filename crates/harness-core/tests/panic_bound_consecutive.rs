#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Contract (CLAUDE.md §1): a panicking Stop gate fails CLOSED (block). Only a
//! SECOND CONSECUTIVE panic of the SAME gate in the SAME session may fall
//! through to allow. `stop_hook_active == true` alone is not evidence of that:
//! it is set whenever ANY gate blocked the previous stop. The streak is
//! therefore tracked per gate, per session, under
//! `$HOME/.harness/state/panic-markers/`, and a clean run of the gate resets it.
//! When the streak cannot be determined (marker IO fails, no session id), the
//! first stop still blocks, and a re-entry is allowed only loudly (a
//! `systemMessage` on stdout) so a persistently broken store cannot trap the
//! session in an endless block loop and cannot be silent either.
//!
//! `run_guarded` ends in `process::exit`, so each stop is observed out of
//! process: the test re-executes this test binary into one of the child test
//! fns below (no-ops unless `PANIC_BOUND_CHILD` is set) with an isolated
//! HOME/XDG state and a controlled `CLAUDE_CODE_SESSION_ID`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const CHILD_ENV: &str = "PANIC_BOUND_CHILD";
const ACTIVE_ENV: &str = "PANIC_BOUND_ACTIVE";
const GATE: &str = "probe-gate";
const SESSION: &str = "s-panic-bound-consecutive";

fn child_active() -> bool {
    std::env::var(ACTIVE_ENV).as_deref() == Ok("1")
}

/// Re-exec target: a gate body that panics deterministically.
#[test]
fn child_panicking_body() {
    if std::env::var(CHILD_ENV).as_deref() != Ok("panic") {
        return;
    }
    harness_core::gate::run::run_guarded(GATE, false, child_active(), || -> () {
        panic!("deterministic gate panic (panic_bound_consecutive probe)")
    });
}

/// Re-exec target: a gate body that judges cleanly and ends the process with
/// `exit(0)`, exactly like real gates do (control never returns to the guard).
#[test]
fn child_clean_body() {
    if std::env::var(CHILD_ENV).as_deref() != Ok("clean") {
        return;
    }
    harness_core::gate::run::run_guarded(GATE, false, child_active(), || -> () {
        std::process::exit(0)
    });
}

/// A unique scratch root per test, with an isolated HOME and XDG dirs.
fn scratch(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let d = std::env::temp_dir().join(format!(
        "hc-panic-bound-{tag}-{}-{nanos}",
        std::process::id()
    ));
    for sub in ["home", "state", "data", "config", "cache", "cwd"] {
        std::fs::create_dir_all(d.join(sub)).unwrap();
    }
    d
}

#[derive(Clone, Copy)]
enum Body {
    Panic,
    Clean,
}

fn run_child(root: &Path, body: Body, active: bool, with_session: bool) -> Output {
    let (kind, test) = match body {
        Body::Panic => ("panic", "child_panicking_body"),
        Body::Clean => ("clean", "child_clean_body"),
    };
    let mut cmd = Command::new(std::env::current_exe().unwrap());
    cmd.args(["--exact", test, "--nocapture", "--test-threads=1"])
        .env(CHILD_ENV, kind)
        .env(ACTIVE_ENV, if active { "1" } else { "0" })
        .env("HOME", root.join("home"))
        .env("XDG_STATE_HOME", root.join("state"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_CACHE_HOME", root.join("cache"))
        .current_dir(root.join("cwd"));
    if with_session {
        cmd.env("CLAUDE_CODE_SESSION_ID", SESSION);
    } else {
        cmd.env_remove("CLAUDE_CODE_SESSION_ID");
    }
    cmd.output().unwrap()
}

fn stdout_of(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).to_string()
}

fn is_block(o: &Output) -> bool {
    o.status.code() == Some(0) && stdout_of(o).contains("\"decision\":\"block\"")
}

fn is_allow(o: &Output) -> bool {
    o.status.code() == Some(0) && !stdout_of(o).contains("\"decision\":\"block\"")
}

/// True when some stdout line, parsed from its first `{`, is a JSON object
/// carrying a non-empty string `systemMessage`. Parsing starts at the first `{`
/// because libtest under `--nocapture` prints `test <name> ... ` without a
/// newline, so the child's JSON lands mid-line.
fn has_system_message(o: &Output) -> bool {
    stdout_of(o).lines().any(|l| {
        let Some(start) = l.find('{') else {
            return false;
        };
        serde_json::from_str::<serde_json::Value>(l[start..].trim())
            .ok()
            .and_then(|v| {
                v.get("systemMessage")
                    .and_then(|m| m.as_str())
                    .map(|s| !s.trim().is_empty())
            })
            .unwrap_or(false)
    })
}

fn dbg(o: &Output) -> String {
    format!(
        "exit={:?}\nstdout={}\nstderr={}",
        o.status.code(),
        stdout_of(o),
        String::from_utf8_lossy(&o.stderr)
    )
}

#[test]
fn first_panic_on_continuation_blocks() {
    let root = scratch("first-on-continuation");
    // The turn is continuing because ANOTHER gate blocked; this gate never
    // panicked before in this session. stop_hook_active alone must not buy an
    // allow.
    let o = run_child(&root, Body::Panic, true, true);
    let _ = std::fs::remove_dir_all(&root);
    assert!(
        is_block(&o),
        "contract: the FIRST panic of a gate must block even when stop_hook_active=true \
         (another gate's block set it; this gate has no prior panic) — only a second \
         consecutive panic of the same gate may allow\n{}",
        dbg(&o)
    );
}

#[test]
fn second_consecutive_panic_allows() {
    let root = scratch("second-consecutive");
    let first = run_child(&root, Body::Panic, false, true);
    let second = run_child(&root, Body::Panic, true, true);
    let _ = std::fs::remove_dir_all(&root);
    assert!(
        is_block(&first),
        "contract: the first panic (stop_hook_active=false) must block\n{}",
        dbg(&first)
    );
    assert!(
        is_allow(&second),
        "contract: a second consecutive panic of the same gate in the same session \
         (stop_hook_active=true) is the one case that may fall through to allow\n{}",
        dbg(&second)
    );
}

#[test]
fn clean_run_resets_the_streak() {
    let root = scratch("clean-resets");
    let first = run_child(&root, Body::Panic, false, true);
    let clean = run_child(&root, Body::Clean, true, true);
    let third = run_child(&root, Body::Panic, true, true);
    let _ = std::fs::remove_dir_all(&root);
    assert!(
        is_block(&first),
        "contract: the first panic must block\n{}",
        dbg(&first)
    );
    assert_eq!(
        clean.status.code(),
        Some(0),
        "control: the clean body exits 0\n{}",
        dbg(&clean)
    );
    assert!(
        is_block(&third),
        "contract: a clean run (body ended in exit(0)) breaks the panic streak, so the \
         next panic is a FIRST panic again and must block even with stop_hook_active=true\n{}",
        dbg(&third)
    );
}

/// Make the marker store unusable: `panic-markers` exists as a regular file.
fn break_marker_store(root: &Path) {
    let state = root.join("home").join(".harness").join("state");
    std::fs::create_dir_all(&state).unwrap();
    std::fs::write(state.join("panic-markers"), b"not a directory").unwrap();
}

#[test]
fn marker_io_failure_first_stop_blocks() {
    let root = scratch("io-fail-first");
    break_marker_store(&root);
    let o = run_child(&root, Body::Panic, false, true);
    let _ = std::fs::remove_dir_all(&root);
    assert!(
        is_block(&o),
        "contract: with the marker store broken (undetermined streak), a panic on the \
         first stop (stop_hook_active=false) must still block\n{}",
        dbg(&o)
    );
}

#[test]
fn marker_io_failure_reentry_is_bounded_and_loud() {
    let root = scratch("io-fail-reentry");
    break_marker_store(&root);
    let mut outs = Vec::new();
    for _ in 0..3 {
        outs.push(run_child(&root, Body::Panic, true, true));
    }
    let _ = std::fs::remove_dir_all(&root);
    for (i, o) in outs.iter().enumerate() {
        assert!(
            !is_block(o),
            "contract: re-entry #{} with a persistently broken marker store must NOT block \
             (a deterministic crash plus broken IO must not loop forever)\n{}",
            i + 1,
            dbg(o)
        );
        assert_eq!(
            o.status.code(),
            Some(0),
            "contract: re-entry #{} with a broken marker store exits 0\n{}",
            i + 1,
            dbg(o)
        );
        assert!(
            has_system_message(o),
            "contract: re-entry #{} allowed under an undetermined streak (marker IO failed) \
             must not be silent — stdout must carry a JSON line with a non-empty \
             `systemMessage`\n{}",
            i + 1,
            dbg(o)
        );
    }
}

#[test]
fn missing_session_id_is_undetermined() {
    let root = scratch("no-session");
    let first = run_child(&root, Body::Panic, false, false);
    let second = run_child(&root, Body::Panic, true, false);
    let _ = std::fs::remove_dir_all(&root);
    assert!(
        is_block(&first),
        "contract: without CLAUDE_CODE_SESSION_ID, a panic on the first stop must block\n{}",
        dbg(&first)
    );
    assert!(
        !is_block(&second),
        "contract: without a session id the streak is undetermined; the re-entry must not \
         block (bounded)\n{}",
        dbg(&second)
    );
    assert_eq!(
        second.status.code(),
        Some(0),
        "contract: the undetermined re-entry exits 0\n{}",
        dbg(&second)
    );
    assert!(
        has_system_message(&second),
        "contract: an allow under an undetermined streak (no session id) must not be silent \
         — stdout must carry a JSON line with a non-empty `systemMessage`\n{}",
        dbg(&second)
    );
}
