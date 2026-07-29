// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! End-to-end tests: drive the real built `taintguard` binary's three
//! subcommands (`mark` / `gate` / `clear`) over stdin, exactly the way Claude
//! Code invokes them as PostToolUse / PreToolUse / Stop hooks, and assert on
//! stdout + stderr + exit code. Mirrors `crates/blastguard/tests/integration.rs`
//! and `crates/blastguard/tests/ask_hardening_invariant.rs`'s `run_with_env`
//! pattern (env fully cleared, then only the variables the scenario cares
//! about are set, so a stray `CLAUDECODE`/`CLAUDE_CODE_ENTRYPOINT` leaking in
//! from the process actually running this suite can't contaminate the
//! result).
//!
//! Each test gets its OWN temp dir for `cwd` (the project root) AND its own
//! `TAINTGUARD_STATE_DIR` (so marker files never collide across tests or with
//! a real `~/.taintguard`), and its own session id (so `mark`/`gate`/`clear`
//! invocations within one test never collide with another test's marker).
//!
//! # Why the observe-only tests live HERE and not in a unit test
//!
//! A fault-injection audit of this crate proved the unit tests are structurally
//! blind to three live faults, because every one of them calls
//! `observe::resolve(Some("…"))` / `decide_gate_with(_, posture)` directly:
//!
//! * `observe::posture()` — the ONLY reader of the real `TAINTGUARD_OBSERVE_ONLY`
//!   env var — had zero test callers, so a loose truthy parse inside it
//!   (`"yes"`, `"true"`, `" 1"`, even `"false"`) disabled the gate against the
//!   real binary while the whole suite stayed green.
//! * `emit_gate`'s stdout was observed by no test at all, so deleting its
//!   `println!` made a tainted-and-suppressed turn byte-identical to a clean
//!   turn.
//! * `emit_gate`'s ledger-append `Err` diagnostic (stderr) was observed by no
//!   test, so swallowing it kept the suite green.
//!
//! Only a test that spawns the real binary with the real env var can see any of
//! that, which is why these are integration tests that assert on the child
//! process's stdout AND stderr.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use taintguard::observe;

/// Run the `taintguard` binary's subcommand `sub` (`"mark"`/`"gate"`/`"clear"`)
/// with a fully-controlled environment: cleared, then `cwd`/`state_dir` pinned
/// and `extra_env` applied on top. Returns (exit_code, stdout, stderr).
///
/// stderr is returned (it used to be captured and thrown away) because a
/// diagnostic that only reaches stderr — e.g. `emit_gate`'s "the measurement
/// under-counts" line when the ledger append fails — is otherwise unobservable
/// from a test, which is exactly how that fault survived fault injection.
fn run(
    sub: &str,
    payload: &str,
    cwd: &Path,
    state_dir: &Path,
    extra_env: &[(&str, &str)],
) -> (i32, String, String) {
    let bin = env!("CARGO_BIN_EXE_taintguard");
    let mut cmd = Command::new(bin);
    cmd.arg(sub);
    cmd.env_clear();
    cmd.env("TAINTGUARD_STATE_DIR", state_dir);
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("binary spawns");
    if let Some(mut child_stdin) = child.stdin.take() {
        child_stdin
            .write_all(payload.as_bytes())
            .expect("write payload to stdin");
    }
    let out = child.wait_with_output().expect("binary runs");
    let _ = cwd; // cwd is embedded in the payload's "cwd" field, not the process cwd.
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// A fresh project root + isolated state dir + a session id unique to this
/// test, so parallel `cargo test` runs never collide.
struct Fixture {
    _project_root: tempfile::TempDir,
    _state_dir: tempfile::TempDir,
    cwd: std::path::PathBuf,
    state_dir: std::path::PathBuf,
    session: String,
}

fn fixture(name: &str) -> Fixture {
    let project_root = tempfile::Builder::new()
        .prefix(&format!("taintguard-e2e-{name}-project-"))
        .tempdir()
        .expect("tempdir");
    let state_dir = tempfile::Builder::new()
        .prefix(&format!("taintguard-e2e-{name}-state-"))
        .tempdir()
        .expect("tempdir");
    let cwd = project_root.path().to_path_buf();
    let state_dir_path = state_dir.path().to_path_buf();
    Fixture {
        _project_root: project_root,
        _state_dir: state_dir,
        cwd,
        state_dir: state_dir_path,
        session: format!("session-{name}"),
    }
}

fn mark_payload(
    tool_name: &str,
    tool_input: serde_json::Value,
    cwd: &Path,
    session: &str,
) -> String {
    serde_json::json!({
        "hook_event_name": "PostToolUse",
        "tool_name": tool_name,
        "tool_input": tool_input,
        "cwd": cwd.to_string_lossy(),
        "session_id": session,
    })
    .to_string()
}

fn gate_payload(
    tool_name: &str,
    tool_input: serde_json::Value,
    cwd: &Path,
    session: &str,
) -> String {
    serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": tool_name,
        "tool_input": tool_input,
        "cwd": cwd.to_string_lossy(),
        "session_id": session,
    })
    .to_string()
}

fn stop_payload(cwd: &Path, session: &str) -> String {
    serde_json::json!({
        "hook_event_name": "Stop",
        "cwd": cwd.to_string_lossy(),
        "session_id": session,
    })
    .to_string()
}

const INTERACTIVE_ENV: &[(&str, &str)] = &[("CLAUDECODE", "1"), ("CLAUDE_CODE_ENTRYPOINT", "cli")];
const HEADLESS_ENV: &[(&str, &str)] = &[("CLAUDECODE", "1"), ("CLAUDE_CODE_ENTRYPOINT", "sdk-cli")];

fn permission_decision(stdout: &str) -> Option<String> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(trimmed)
        .unwrap_or_else(|e| panic!("expected hook JSON, got {trimmed:?}: {e}"));
    v["hookSpecificOutput"]["permissionDecision"]
        .as_str()
        .map(|s| s.to_string())
}

// ── WebFetch / WebSearch taint the session ──────────────────────────────────

#[test]
fn webfetch_then_bash_asks_when_interactive() {
    let f = fixture("webfetch-interactive");
    let (code, _, _) = run(
        "mark",
        &mark_payload(
            "WebFetch",
            serde_json::json!({"url": "https://example.com"}),
            &f.cwd,
            &f.session,
        ),
        &f.cwd,
        &f.state_dir,
        &[],
    );
    assert_eq!(code, 0, "mark must always exit 0");

    let (code, stdout, _) = run(
        "gate",
        &gate_payload(
            "Bash",
            serde_json::json!({"command": "echo hi"}),
            &f.cwd,
            &f.session,
        ),
        &f.cwd,
        &f.state_dir,
        INTERACTIVE_ENV,
    );
    assert_eq!(code, 0, "gate must always exit 0");
    assert_eq!(
        permission_decision(&stdout).as_deref(),
        Some("ask"),
        "a tainted session in an interactive env must ask, got: {stdout:?}"
    );
}

#[test]
fn webfetch_then_bash_denies_when_headless() {
    let f = fixture("webfetch-headless");
    let (code, _, _) = run(
        "mark",
        &mark_payload(
            "WebFetch",
            serde_json::json!({"url": "https://example.com"}),
            &f.cwd,
            &f.session,
        ),
        &f.cwd,
        &f.state_dir,
        &[],
    );
    assert_eq!(code, 0);

    let (code, stdout, _) = run(
        "gate",
        &gate_payload(
            "Bash",
            serde_json::json!({"command": "echo hi"}),
            &f.cwd,
            &f.session,
        ),
        &f.cwd,
        &f.state_dir,
        HEADLESS_ENV,
    );
    assert_eq!(code, 0);
    assert_eq!(
        permission_decision(&stdout).as_deref(),
        Some("deny"),
        "a tainted session in a headless env must harden to deny, got: {stdout:?}"
    );
}

#[test]
fn websearch_then_write_asks_when_interactive() {
    let f = fixture("websearch");
    let (code, _, _) = run(
        "mark",
        &mark_payload(
            "WebSearch",
            serde_json::json!({"query": "how to x"}),
            &f.cwd,
            &f.session,
        ),
        &f.cwd,
        &f.state_dir,
        &[],
    );
    assert_eq!(code, 0);

    let (code, stdout, _) = run(
        "gate",
        &gate_payload(
            "Write",
            serde_json::json!({"file_path": "out.rs", "content": "x"}),
            &f.cwd,
            &f.session,
        ),
        &f.cwd,
        &f.state_dir,
        INTERACTIVE_ENV,
    );
    assert_eq!(code, 0);
    assert_eq!(permission_decision(&stdout).as_deref(), Some("ask"));
}

// ── external Read taints the session ────────────────────────────────────────

#[test]
fn external_read_then_edit_is_blocked() {
    let f = fixture("external-read");
    let outside = tempfile::Builder::new()
        .prefix("taintguard-e2e-outside-")
        .tempdir()
        .expect("tempdir");
    let secret = outside.path().join("secret.txt");
    std::fs::write(&secret, "outside content").expect("write outside file");

    let (code, _, _) = run(
        "mark",
        &mark_payload(
            "Read",
            serde_json::json!({"file_path": secret.to_string_lossy()}),
            &f.cwd,
            &f.session,
        ),
        &f.cwd,
        &f.state_dir,
        &[],
    );
    assert_eq!(code, 0);

    let (code, stdout, _) = run(
        "gate",
        &gate_payload(
            "Edit",
            serde_json::json!({"file_path": "src/main.rs"}),
            &f.cwd,
            &f.session,
        ),
        &f.cwd,
        &f.state_dir,
        INTERACTIVE_ENV,
    );
    assert_eq!(code, 0);
    let decision = permission_decision(&stdout);
    assert!(
        decision.as_deref() == Some("ask") || decision.as_deref() == Some("deny"),
        "a Read of a path outside the project root must not be silently allowed, got: {stdout:?}"
    );
}

// ── anti-vacuity: legitimate flows keep working ─────────────────────────────

#[test]
fn clean_session_gate_allows_silently() {
    let f = fixture("clean");
    let (code, stdout, _) = run(
        "gate",
        &gate_payload(
            "Bash",
            serde_json::json!({"command": "cargo test"}),
            &f.cwd,
            &f.session,
        ),
        &f.cwd,
        &f.state_dir,
        INTERACTIVE_ENV,
    );
    assert_eq!(code, 0);
    assert!(
        stdout.trim().is_empty(),
        "a session that never consumed untrusted content must allow silently, got: {stdout:?}"
    );
}

#[test]
fn in_repo_read_does_not_taint_the_session() {
    let f = fixture("in-repo-read");
    let f_path = f.cwd.join("src.rs");
    std::fs::write(&f_path, "fn main() {}").expect("write in-repo file");

    let (code, _, _) = run(
        "mark",
        &mark_payload(
            "Read",
            serde_json::json!({"file_path": f_path.to_string_lossy()}),
            &f.cwd,
            &f.session,
        ),
        &f.cwd,
        &f.state_dir,
        &[],
    );
    assert_eq!(code, 0);

    let (code, stdout, _) = run(
        "gate",
        &gate_payload(
            "MultiEdit",
            serde_json::json!({"file_path": "src.rs"}),
            &f.cwd,
            &f.session,
        ),
        &f.cwd,
        &f.state_dir,
        INTERACTIVE_ENV,
    );
    assert_eq!(code, 0);
    assert!(
        stdout.trim().is_empty(),
        "an in-repo Read must not taint the session, got: {stdout:?}"
    );
}

#[test]
fn clear_after_stop_restores_a_clean_gate() {
    let f = fixture("clear-restores");
    let (code, _, _) = run(
        "mark",
        &mark_payload(
            "WebFetch",
            serde_json::json!({"url": "https://example.com"}),
            &f.cwd,
            &f.session,
        ),
        &f.cwd,
        &f.state_dir,
        &[],
    );
    assert_eq!(code, 0);

    // Confirm it actually was tainted before clearing (else this test would
    // pass vacuously regardless of whether `clear` does anything).
    let (_, stdout, _) = run(
        "gate",
        &gate_payload(
            "Bash",
            serde_json::json!({"command": "echo hi"}),
            &f.cwd,
            &f.session,
        ),
        &f.cwd,
        &f.state_dir,
        INTERACTIVE_ENV,
    );
    assert_eq!(permission_decision(&stdout).as_deref(), Some("ask"));

    let (code, stdout, _) = run(
        "clear",
        &stop_payload(&f.cwd, &f.session),
        &f.cwd,
        &f.state_dir,
        &[],
    );
    assert_eq!(code, 0, "clear must always exit 0");
    assert!(
        stdout.trim().is_empty(),
        "Stop hook must not inject anything"
    );

    let (code, stdout, _) = run(
        "gate",
        &gate_payload(
            "Bash",
            serde_json::json!({"command": "echo hi"}),
            &f.cwd,
            &f.session,
        ),
        &f.cwd,
        &f.state_dir,
        INTERACTIVE_ENV,
    );
    assert_eq!(code, 0);
    assert!(
        stdout.trim().is_empty(),
        "a clean Stop must restore normal (silent-allow) access, got: {stdout:?}"
    );
}

#[test]
fn empty_stdin_is_silent_on_every_subcommand() {
    let f = fixture("empty-stdin");
    for sub in ["mark", "gate", "clear"] {
        let (code, stdout, _) = run(sub, "", &f.cwd, &f.state_dir, &[]);
        assert_eq!(code, 0, "{sub} must always exit 0");
        assert!(
            stdout.trim().is_empty(),
            "{sub} on empty stdin must stay silent, got: {stdout:?}"
        );
    }
}

// ── fail-closed: indeterminate state / indeterminate path ───────────────────

#[test]
fn corrupt_marker_fails_closed_to_ask_or_deny() {
    let f = fixture("corrupt-marker");
    // A Read that never happened but establishes the project dir/session first
    // isn't needed — directly corrupt the marker path that `gate` will look up.
    // We derive it the same way `mark` would have (any real taint first, then
    // clobber the file with invalid JSON), so the path is guaranteed correct
    // without reimplementing the crate's private path derivation here.
    let (code, _, _) = run(
        "mark",
        &mark_payload(
            "WebFetch",
            serde_json::json!({"url": "https://example.com"}),
            &f.cwd,
            &f.session,
        ),
        &f.cwd,
        &f.state_dir,
        &[],
    );
    assert_eq!(code, 0);

    // Find the marker file `mark` just wrote and corrupt it in place.
    let marker =
        find_marker_file(&f.state_dir).expect("mark must have written exactly one marker file");
    std::fs::write(&marker, b"{ not json").expect("corrupt the marker");

    let (code, stdout, _) = run(
        "gate",
        &gate_payload(
            "Bash",
            serde_json::json!({"command": "echo hi"}),
            &f.cwd,
            &f.session,
        ),
        &f.cwd,
        &f.state_dir,
        INTERACTIVE_ENV,
    );
    assert_eq!(code, 0);
    let decision = permission_decision(&stdout);
    assert!(
        decision.as_deref() == Some("ask") || decision.as_deref() == Some("deny"),
        "a corrupt/unreadable taint marker must fail closed, not allow silently, got: {stdout:?}"
    );
}

#[test]
fn read_with_no_file_path_fails_closed() {
    // A Read payload missing `file_path` entirely: the target is
    // unextractable (indeterminate), which must mark the session tainted
    // (fail-closed), never silently pass through as clean.
    let f = fixture("read-no-path");
    let (code, _, _) = run(
        "mark",
        &mark_payload("Read", serde_json::json!({}), &f.cwd, &f.session),
        &f.cwd,
        &f.state_dir,
        &[],
    );
    assert_eq!(code, 0);

    let (code, stdout, _) = run(
        "gate",
        &gate_payload(
            "Bash",
            serde_json::json!({"command": "echo hi"}),
            &f.cwd,
            &f.session,
        ),
        &f.cwd,
        &f.state_dir,
        INTERACTIVE_ENV,
    );
    assert_eq!(code, 0);
    let decision = permission_decision(&stdout);
    assert!(
        decision.as_deref() == Some("ask") || decision.as_deref() == Some("deny"),
        "an indeterminate Read target must fail closed, got: {stdout:?}"
    );
}

// ── FIX #1 (primary): an unwritable state store must fail closed, a healthy
// one must not over-block ──────────────────────────────────────────────────

/// CONTROL for FIX #1: a genuinely healthy, writable, empty store (no `mark`
/// ever ran) must still allow silently. Guards against the writability probe
/// itself becoming an over-block on the ordinary happy path.
#[test]
fn healthy_writable_empty_store_allows_silently() {
    let f = fixture("writable-control");
    let (code, stdout, _) = run(
        "gate",
        &gate_payload(
            "Bash",
            serde_json::json!({"command": "echo hi"}),
            &f.cwd,
            &f.session,
        ),
        &f.cwd,
        &f.state_dir,
        INTERACTIVE_ENV,
    );
    assert_eq!(code, 0);
    assert!(
        stdout.trim().is_empty(),
        "a healthy, writable, empty store must allow silently (no over-block), got: {stdout:?}"
    );
}

/// PRIMARY FIX #1: when the state base is unwritable, a `mark` write is lost
/// (fail-soft, exits 0) and the marker stays absent. `gate` must not read
/// that absence as trustworthy `Clean` — it must probe the store's
/// writability and fail closed when the probe itself fails.
#[cfg(unix)]
#[test]
fn unwritable_state_store_fails_closed_despite_a_lost_mark() {
    use std::os::unix::fs::PermissionsExt;

    let f = fixture("unwritable-store");

    // Make the state base unwritable BEFORE the mark attempt, mirroring a
    // read-only mount / chmod 555 / disk-full session state dir.
    std::fs::set_permissions(&f.state_dir, std::fs::Permissions::from_mode(0o555))
        .expect("chmod state dir read-only");

    let restore_perms = || {
        std::fs::set_permissions(&f.state_dir, std::fs::Permissions::from_mode(0o755))
            .expect("restore state dir permissions for cleanup");
    };

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // This mark attempt cannot actually persist (the dir is read-only),
        // but per the crate's fail-soft write contract it still exits 0.
        let (mark_code, _, _) = run(
            "mark",
            &mark_payload(
                "WebFetch",
                serde_json::json!({"url": "https://example.com"}),
                &f.cwd,
                &f.session,
            ),
            &f.cwd,
            &f.state_dir,
            &[],
        );
        assert_eq!(
            mark_code, 0,
            "mark must always exit 0 even when it cannot persist"
        );

        // The marker never landed (store unwritable) — confirm the gate does
        // NOT trust that absence as Clean.
        let (gate_code, stdout, _) = run(
            "gate",
            &gate_payload(
                "Bash",
                serde_json::json!({"command": "echo hi"}),
                &f.cwd,
                &f.session,
            ),
            &f.cwd,
            &f.state_dir,
            INTERACTIVE_ENV,
        );
        assert_eq!(gate_code, 0, "gate must always exit 0");
        let decision = permission_decision(&stdout);
        assert!(
            decision.as_deref() == Some("ask") || decision.as_deref() == Some("deny"),
            "an unwritable state store with a lost mark must fail closed, not silently \
             allow, got: {stdout:?}"
        );
    }));

    // Restore permissions unconditionally so the TempDir can clean up on drop
    // regardless of the assertions' outcome above.
    restore_perms();
    result.unwrap();
}

// ── FIX #3: a valid-JSON, wrong-schema marker must fail closed ──────────────

#[test]
fn wrong_schema_marker_fails_closed_to_ask_or_deny() {
    let f = fixture("wrong-schema-marker");
    // Establish the marker path the same way the corrupt-marker test does
    // (real mark first, then clobber), so the derived path is guaranteed
    // correct without reimplementing the crate's private path logic here.
    let (code, _, _) = run(
        "mark",
        &mark_payload(
            "WebFetch",
            serde_json::json!({"url": "https://example.com"}),
            &f.cwd,
            &f.session,
        ),
        &f.cwd,
        &f.state_dir,
        &[],
    );
    assert_eq!(code, 0);

    let marker =
        find_marker_file(&f.state_dir).expect("mark must have written exactly one marker file");
    // Valid JSON, but missing the required `tainted` field entirely.
    std::fs::write(&marker, br#"{"foo":123}"#).expect("overwrite with wrong-schema marker");

    let (code, stdout, _) = run(
        "gate",
        &gate_payload(
            "Bash",
            serde_json::json!({"command": "echo hi"}),
            &f.cwd,
            &f.session,
        ),
        &f.cwd,
        &f.state_dir,
        INTERACTIVE_ENV,
    );
    assert_eq!(code, 0);
    let decision = permission_decision(&stdout);
    assert!(
        decision.as_deref() == Some("ask") || decision.as_deref() == Some("deny"),
        "a valid-JSON but wrong-schema marker must fail closed, not serde-default to clean, \
         got: {stdout:?}"
    );
}

// ── FIX #4: an unexpanded leading `~` must never be trusted ─────────────────

#[test]
fn unexpanded_tilde_read_taints_and_gate_fails_closed() {
    let f = fixture("tilde-read");
    let (code, _, _) = run(
        "mark",
        &mark_payload(
            "Read",
            serde_json::json!({"file_path": "~/secret"}),
            &f.cwd,
            &f.session,
        ),
        &f.cwd,
        &f.state_dir,
        &[],
    );
    assert_eq!(code, 0);

    let (code, stdout, _) = run(
        "gate",
        &gate_payload(
            "Edit",
            serde_json::json!({"file_path": "src/main.rs"}),
            &f.cwd,
            &f.session,
        ),
        &f.cwd,
        &f.state_dir,
        INTERACTIVE_ENV,
    );
    assert_eq!(code, 0);
    let decision = permission_decision(&stdout);
    assert!(
        decision.as_deref() == Some("ask") || decision.as_deref() == Some("deny"),
        "a Read of an unexpanded `~` path must not be trusted, got: {stdout:?}"
    );
}

/// Locate the single `taint.json` marker file under `state_dir` (recursively) —
/// a test helper, not a reimplementation of the crate's path derivation.
fn find_marker_file(state_dir: &Path) -> Option<std::path::PathBuf> {
    fn walk(dir: &Path, found: &mut Vec<std::path::PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, found);
            } else if path.file_name().and_then(|n| n.to_str()) == Some("taint.json") {
                found.push(path);
            }
        }
    }
    let mut found = Vec::new();
    walk(state_dir, &mut found);
    found.into_iter().next()
}

// ═══════════════════════════════════════════════════════════════════════════
// observe-only, END-TO-END through the real binary and the real env var
// ═══════════════════════════════════════════════════════════════════════════

/// Serializes the in-process `TAINTGUARD_STATE_DIR` writes done by
/// [`with_state_dir`]. `std::env::set_var` is process-global and this test
/// binary runs its tests on parallel threads, so two tests resolving
/// `observe::ledger_path` for two different temp stores would otherwise race.
/// Only [`with_state_dir`] touches this var in-process; the child processes get
/// theirs explicitly via [`run`], so they are unaffected either way.
static STATE_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Point the *library's* state-dir resolution at `state_dir` for the duration of
/// `f`, so in-process calls (`observe::ledger_path`, `observe::tally`) read the
/// same store the child process wrote. Uses the crate's real path derivation
/// rather than reimplementing `<base>/<project_key>/observe-only.jsonl` here.
fn with_state_dir<T>(state_dir: &Path, f: impl FnOnce() -> T) -> T {
    let guard = STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("TAINTGUARD_STATE_DIR", state_dir);
    let out = f();
    drop(guard);
    out
}

/// `(parseable records, unparseable lines)` in the observe-only ledger for this
/// fixture's project, via the crate's own [`observe::tally`].
fn ledger_tally(f: &Fixture) -> (usize, usize) {
    with_state_dir(&f.state_dir, || observe::tally(&f.cwd))
        .expect("the observe-only ledger must be readable")
}

/// The observe-only ledger path for this fixture's project.
fn ledger_path(f: &Fixture) -> std::path::PathBuf {
    with_state_dir(&f.state_dir, || observe::ledger_path(&f.cwd))
}

/// Every record currently in the ledger, parsed with the crate's own
/// [`observe::Record`] type (a shape change in production shows up here).
fn ledger_records(f: &Fixture) -> Vec<observe::Record> {
    let path = ledger_path(f);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read observe-only ledger {}: {e}", path.display()));
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            serde_json::from_str(l)
                .unwrap_or_else(|e| panic!("ledger line {l:?} must parse as a Record: {e}"))
        })
        .collect()
}

/// Parse the hook line the binary printed, asserting it is present at all.
fn hook_json(stdout: &str) -> serde_json::Value {
    let trimmed = stdout.trim();
    assert!(
        !trimmed.is_empty(),
        "expected a hook JSON line on stdout, got NOTHING — a suppressed enforcement that \
         prints nothing is byte-identical to a clean turn"
    );
    serde_json::from_str(trimmed)
        .unwrap_or_else(|e| panic!("expected hook JSON, got {trimmed:?}: {e}"))
}

/// The `hookSpecificOutput` object, asserting it exists and is an object (so the
/// key-absence assertions below cannot pass vacuously against a payload that
/// simply has no `hookSpecificOutput` at all).
fn hook_specific(v: &serde_json::Value) -> &serde_json::Map<String, serde_json::Value> {
    v.get("hookSpecificOutput")
        .and_then(|h| h.as_object())
        .unwrap_or_else(|| panic!("hookSpecificOutput must be present and an object, got {v}"))
}

/// Env for a genuinely observe-only child: interactive (so a wrongly-enforcing
/// implementation would emit the *softer* `ask`, and the assertions below would
/// still catch it) plus the REAL opt-in value of the REAL env var.
fn observe_only_env() -> Vec<(&'static str, &'static str)> {
    let mut env = INTERACTIVE_ENV.to_vec();
    env.push((observe::OBSERVE_ONLY_ENV, observe::OBSERVE_ONLY_OPT_IN));
    env
}

/// KILLS FAULT F-C (deleting the `println!` in `emit_gate`'s `Observe` arm —
/// which the audit injected while the entire 66-test suite stayed GREEN, because
/// no test observed `emit_gate`'s stdout at all). Also the RED for the new
/// top-level `systemMessage` requirement.
///
/// A suppressed enforcement must be visible on stdout in TWO places:
///   * `hookSpecificOutput.additionalContext` — the model-facing warning, and
///   * a **top-level** `systemMessage` — the user-facing one, a sibling of
///     `hookSpecificOutput`, because a warning buried in `additionalContext`
///     alone is only seen by the model, never by the human who set the posture.
///
/// It must carry NO `permissionDecision` (an explicit `allow` would override
/// other gates and the user's own rules — see `hookio::observe_json`'s docs).
///
/// HOW THIS FAILS against a wrong implementation:
///   * `println!` deleted / made conditional → stdout empty → `hook_json` panics.
///   * `permissionDecision: "allow"` added → the absence assertion fails.
///   * `systemMessage` missing → the presence assertion fails (this is the
///     currently-EXPECTED failure, i.e. the RED).
///   * `systemMessage` nested inside `hookSpecificOutput` instead of at the root
///     → the top-level assertion fails AND the explicit nesting assertion fails.
///   * A `systemMessage` that is present but empty, or that does not name the
///     suppression, → the content assertions fail.
#[test]
fn observe_only_suppression_is_visible_on_stdout_with_a_top_level_system_message() {
    let f = fixture("observe-visible");
    let env = observe_only_env();

    let (code, _, _) = run(
        "mark",
        &mark_payload(
            "WebFetch",
            serde_json::json!({"url": "https://example.com"}),
            &f.cwd,
            &f.session,
        ),
        &f.cwd,
        &f.state_dir,
        &env,
    );
    assert_eq!(code, 0, "mark must always exit 0");

    let (code, stdout, _) = run(
        "gate",
        &gate_payload(
            "Bash",
            serde_json::json!({"command": "echo hi"}),
            &f.cwd,
            &f.session,
        ),
        &f.cwd,
        &f.state_dir,
        &env,
    );
    assert_eq!(code, 0, "gate must always exit 0");

    let v = hook_json(&stdout);
    let hso = hook_specific(&v);

    // (a) the model-facing warning
    let ctx = hso
        .get("additionalContext")
        .and_then(|c| c.as_str())
        .unwrap_or_else(|| panic!("additionalContext must be present and a string, got {v}"));
    assert!(
        ctx.contains("OBSERVE-ONLY"),
        "additionalContext must announce the posture: {ctx}"
    );
    assert!(
        ctx.contains("SUPPRESSED"),
        "additionalContext must say enforcement was suppressed: {ctx}"
    );

    // (b) NO permissionDecision at all — not even `allow`.
    assert!(
        hso.get("permissionDecision").is_none(),
        "observe-only must emit NO permissionDecision key (an explicit allow would override \
         other gates and the user's own permission rules), got {v}"
    );

    // (c) a TOP-LEVEL systemMessage, sibling of hookSpecificOutput.
    let sys = v
        .get("systemMessage")
        .unwrap_or_else(|| panic!("a TOP-LEVEL `systemMessage` must be present, got {v}"));
    let sys = sys
        .as_str()
        .unwrap_or_else(|| panic!("`systemMessage` must be a string, got {sys}"));
    assert!(
        !sys.trim().is_empty(),
        "`systemMessage` must be a non-empty string (an empty one reads as no warning)"
    );
    assert!(
        sys.contains("OBSERVE-ONLY"),
        "`systemMessage` must name the posture: {sys}"
    );
    assert!(
        sys.contains("SUPPRESSED"),
        "`systemMessage` must name the suppression: {sys}"
    );

    // (d) and it must NOT be nested — a `systemMessage` inside
    // `hookSpecificOutput` is not surfaced to the user, so an implementation
    // that puts it there must FAIL this test rather than half-pass it.
    assert!(
        hso.get("systemMessage").is_none(),
        "`systemMessage` must live at the JSON ROOT, not inside hookSpecificOutput, got {v}"
    );
}

/// KILLS FAULT F-B2 — the important one. `observe::posture()` (the ONLY reader
/// of `TAINTGUARD_OBSERVE_ONLY`) had ZERO test callers: every existing test
/// called `observe::resolve(Some("…"))` instead. The audit injected a loose
/// truthy parse into `posture()` while leaving `resolve()` exact, and the whole
/// suite stayed GREEN — yet against the real binary `TAINTGUARD_OBSERVE_ONLY=false`
/// (and `" 1"`, `"yes"`, `"observe"`, `"true"`) then DISABLED the gate.
///
/// So this test deliberately does NOT call `observe::resolve` — that call is the
/// bypass that let the fault hide. It drives the REAL env var through the REAL
/// binary, which is the only way `posture()` is the code under test.
///
/// HOW THIS FAILS against a wrong implementation: any loosening of the exact
/// `"1"` comparison in `posture()`/`resolve()` (trim, case-fold, truthy parse,
/// "any non-empty value", "any value at all", `unwrap_or(ObserveOnly)`) turns at
/// least one of these values into a suppression, so the `ask`/`deny` assertion
/// fails for it. The positive control at the end fails in the opposite
/// direction: if `posture()` ignored the env var entirely (always Enforce) the
/// loop would pass vacuously, so the control proves the test is sensitive to the
/// value rather than always seeing ask/deny.
#[test]
fn the_real_observe_only_env_var_fails_closed_for_every_non_opt_in_value() {
    let f = fixture("observe-env-failclosed");

    // `None` = the var absent entirely; `Some("")` = present but empty.
    let non_opt_in: &[Option<&str>] = &[
        Some(" 1"),
        Some("true"),
        Some("01"),
        Some("yes"),
        Some("false"),
        Some("0"),
        Some(""),
        None,
    ];

    for (i, raw) in non_opt_in.iter().enumerate() {
        // A fresh session per value so one value's marker cannot satisfy
        // another's `gate`.
        let session = format!("{}-{i}", f.session);
        let mut env = INTERACTIVE_ENV.to_vec();
        if let Some(value) = raw {
            env.push((observe::OBSERVE_ONLY_ENV, value));
        }

        let (code, _, _) = run(
            "mark",
            &mark_payload(
                "WebFetch",
                serde_json::json!({"url": "https://example.com"}),
                &f.cwd,
                &session,
            ),
            &f.cwd,
            &f.state_dir,
            &env,
        );
        assert_eq!(code, 0, "mark must always exit 0 ({raw:?})");

        // A write-class tool in a tainted session.
        let (code, stdout, _) = run(
            "gate",
            &gate_payload(
                "Write",
                serde_json::json!({"file_path": "out.rs", "content": "x"}),
                &f.cwd,
                &session,
            ),
            &f.cwd,
            &f.state_dir,
            &env,
        );
        assert_eq!(code, 0, "gate must always exit 0 ({raw:?})");
        let decision = permission_decision(&stdout);
        assert!(
            decision.as_deref() == Some("ask") || decision.as_deref() == Some("deny"),
            "{}={raw:?} must NOT opt into observe-only — the gate must still emit ask/deny, \
             got: {stdout:?}",
            observe::OBSERVE_ONLY_ENV
        );
    }

    // Enforcing writes no ledger lines, so the ledger is still empty here. This
    // also kills an implementation that records a "suppression" while enforcing.
    assert_eq!(
        ledger_tally(&f),
        (0, 0),
        "enforcing must not append observe-only ledger lines"
    );

    // ── POSITIVE CONTROL: the exact opt-in value DOES suppress ──────────────
    // Without this the loop above could pass against a binary that ignores the
    // env var completely (always enforce), which would prove nothing about the
    // parse being *exact*.
    let session = format!("{}-optin", f.session);
    let env = observe_only_env();
    let (code, _, _) = run(
        "mark",
        &mark_payload(
            "WebFetch",
            serde_json::json!({"url": "https://example.com"}),
            &f.cwd,
            &session,
        ),
        &f.cwd,
        &f.state_dir,
        &env,
    );
    assert_eq!(code, 0);
    let (code, stdout, _) = run(
        "gate",
        &gate_payload(
            "Write",
            serde_json::json!({"file_path": "out.rs", "content": "x"}),
            &f.cwd,
            &session,
        ),
        &f.cwd,
        &f.state_dir,
        &env,
    );
    assert_eq!(code, 0);
    let v = hook_json(&stdout);
    let hso = hook_specific(&v);
    assert!(
        hso.get("permissionDecision").is_none(),
        "{}={} MUST suppress the enforcement (positive control), got {v}",
        observe::OBSERVE_ONLY_ENV,
        observe::OBSERVE_ONLY_OPT_IN
    );
    assert!(
        hso.get("additionalContext")
            .and_then(|c| c.as_str())
            .is_some_and(|c| c.contains("OBSERVE-ONLY")),
        "the suppressed enforcement must still be reported (positive control), got {v}"
    );
    assert_eq!(
        ledger_tally(&f),
        (1, 0),
        "exactly the one suppressed enforcement must be recorded"
    );
}

/// PINS THE CLAIM the crate documents in three places but no test asserted:
/// "the Stop hook's `clear` cannot wipe the project-scoped ledger"
/// (`observe.rs:159-166`). The existing `ledger_is_project_scoped_across_sessions`
/// unit test is MISNAMED — it only appends twice with two session strings and
/// counts 2, which would pass identically if the ledger were session-scoped,
/// because `ledger_path` takes only `cwd`. It never runs `clear` at all.
///
/// This test runs the real Stop hook against a real ledger.
///
/// HOW THIS FAILS against a wrong implementation: move the ledger under the
/// session-scoped dir (or make `clear` remove the session directory / the
/// project dir rather than just the marker file) and the post-`clear` tally
/// drops to 0 — the measurement would be silently wiped by every turn ending.
/// The CONTROL below is what stops this from passing against a `clear` that does
/// nothing whatsoever.
#[test]
fn stop_hook_clear_cannot_wipe_the_project_scoped_ledger() {
    let f = fixture("clear-vs-ledger");
    let env = observe_only_env();

    let (code, _, _) = run(
        "mark",
        &mark_payload(
            "WebFetch",
            serde_json::json!({"url": "https://example.com"}),
            &f.cwd,
            &f.session,
        ),
        &f.cwd,
        &f.state_dir,
        &env,
    );
    assert_eq!(code, 0);

    let (code, stdout, _) = run(
        "gate",
        &gate_payload(
            "Bash",
            serde_json::json!({"command": "echo hi"}),
            &f.cwd,
            &f.session,
        ),
        &f.cwd,
        &f.state_dir,
        &env,
    );
    assert_eq!(code, 0);
    // The suppression happened (so there IS something for `clear` to destroy).
    assert!(
        hook_specific(&hook_json(&stdout))
            .get("additionalContext")
            .is_some(),
        "expected a suppressed-enforcement warning, got: {stdout:?}"
    );
    assert_eq!(
        ledger_tally(&f),
        (1, 0),
        "one suppressed enforcement must be on the ledger before `clear` runs"
    );
    let before = std::fs::read_to_string(ledger_path(&f)).expect("read ledger before clear");

    // The Stop hook.
    let (code, stdout, _) = run(
        "clear",
        &stop_payload(&f.cwd, &f.session),
        &f.cwd,
        &f.state_dir,
        &env,
    );
    assert_eq!(code, 0, "clear must always exit 0");
    assert!(
        stdout.trim().is_empty(),
        "Stop hook must not inject anything, got: {stdout:?}"
    );

    // THE CLAIM: the measurement survives the turn ending.
    assert_eq!(
        ledger_tally(&f),
        (1, 0),
        "`clear` must NOT be able to wipe the project-scoped observe-only ledger"
    );
    assert_eq!(
        std::fs::read_to_string(ledger_path(&f)).expect("read ledger after clear"),
        before,
        "the ledger bytes must be untouched by `clear`"
    );

    // ── CONTROL: `clear` really did clear the session marker ────────────────
    // Without this the test would pass trivially against a `clear` that is a
    // no-op (or that failed and left everything alone) — "the ledger survived"
    // is only meaningful if `clear` actually removed something.
    let (code, stdout, _) = run(
        "gate",
        &gate_payload(
            "Bash",
            serde_json::json!({"command": "echo hi"}),
            &f.cwd,
            &f.session,
        ),
        &f.cwd,
        &f.state_dir,
        &env,
    );
    assert_eq!(code, 0);
    assert!(
        stdout.trim().is_empty(),
        "after `clear` the same session must be clean → silent (proving `clear` did its job \
         and this test is not vacuous), got: {stdout:?}"
    );
    // …and the now-clean gate appended nothing, so the count is still exactly 1.
    assert_eq!(
        ledger_tally(&f),
        (1, 0),
        "a clean turn must not append a ledger line"
    );
}

/// SUPPLIES THE OBSERVE-ONLY COVERAGE a former unit test only claimed to have.
/// That test used to be called `panic_enforces_even_under_observe_only` and was
/// vacuous on this axis: its `let _ = observe::Posture::ObserveOnly;` line had no
/// effect, `analyse_gate_barrier` never consults a posture, and it set no env
/// var — so its verdict was decided by the ambient environment of whoever ran
/// `cargo test`. It has since been renamed to
/// `panic_enforces_regardless_of_posture_because_the_barrier_never_reads_it`
/// (`src/main.rs`), the dead line is gone, and its docstring now claims only the
/// posture-INDEPENDENCE of the barrier, which is the property it actually pins.
/// This test is what covers the real env var end-to-end.
///
/// ## What this test does NOT do — stated explicitly, per the brief
///
/// It does **not** test the `catch_unwind` panic arm. I could NOT reach that arm
/// from outside the process: the binary exposes no fault-injection hook, and
/// every externally-plantable fault (corrupt marker, wrong-schema marker,
/// unwritable store, missing `file_path`) is *handled* — it reaches
/// `Check::Undetermined`, not a panic. So the panic barrier remains pinned only
/// by the in-process unit tests, and I am not claiming otherwise.
///
/// ## What it DOES pin, and a documentation↔behaviour conflict it exposes
///
/// It pins `Check::Undetermined` **with the real `TAINTGUARD_OBSERVE_ONLY=1` set
/// in the child's environment**: production's actual behaviour is a SUPPRESSED
/// BUT REPORTED observe warning recorded as `check: "undetermined"` — *not*
/// `ask`/`deny`. The brief asked me to assert `ask`/`deny` here; that assertion
/// would FAIL against current production, and `decide_gate_with`'s
/// `Undetermined` + `ObserveOnly` arm (`src/main.rs:230-237`) shows why: only a
/// *panic* bypasses observe-only, an unreadable store does not. I am pinning the
/// real behaviour rather than writing an assertion I know to be false about it,
/// and reporting the conflict. If "cannot-determine must enforce regardless of
/// posture" (CLAUDE.md §3) is meant to cover the `Undetermined` path too — not
/// just the panic path — then this is a design decision for a human, and this
/// test is the place that will go red when it changes.
///
/// HOW THIS FAILS against a wrong implementation:
///   * `posture()` ignoring the env var → the child enforces → a
///     `permissionDecision` appears → the absence assertion fails.
///   * `Undetermined` treated as `Clean` under observe-only → stdout empty →
///     `hook_json` panics ("an unreadable store is not a clean turn").
///   * `Undetermined` folded into `"tainted"` in the ledger → the
///     `record.check` assertion fails (a store-health problem must not hide
///     inside the friction statistic).
#[test]
fn undetermined_state_with_the_real_observe_only_env_set_is_reported_not_silent() {
    let f = fixture("observe-undetermined");
    let env = observe_only_env();

    // Real mark first so the marker path is the crate's own, then corrupt it in
    // place — the same fault injection `corrupt_marker_fails_closed_to_ask_or_deny`
    // above uses. This reaches `Check::Undetermined`.
    let (code, _, _) = run(
        "mark",
        &mark_payload(
            "WebFetch",
            serde_json::json!({"url": "https://example.com"}),
            &f.cwd,
            &f.session,
        ),
        &f.cwd,
        &f.state_dir,
        &env,
    );
    assert_eq!(code, 0);
    let marker =
        find_marker_file(&f.state_dir).expect("mark must have written exactly one marker file");
    std::fs::write(&marker, b"{ not json").expect("corrupt the marker");

    let (code, stdout, _) = run(
        "gate",
        &gate_payload(
            "Bash",
            serde_json::json!({"command": "echo hi"}),
            &f.cwd,
            &f.session,
        ),
        &f.cwd,
        &f.state_dir,
        &env,
    );
    assert_eq!(code, 0, "gate must always exit 0");

    let v = hook_json(&stdout);
    let hso = hook_specific(&v);
    let ctx = hso
        .get("additionalContext")
        .and_then(|c| c.as_str())
        .unwrap_or_else(|| panic!("additionalContext must be present and a string, got {v}"));
    assert!(
        ctx.contains("OBSERVE-ONLY"),
        "must announce the posture: {ctx}"
    );
    assert!(
        ctx.contains("SUPPRESSED"),
        "must say enforcement was suppressed: {ctx}"
    );
    assert!(
        ctx.contains("could not verify"),
        "must say the taint state could NOT be read (a store-health signal, distinct from a \
         recorded taint): {ctx}"
    );
    assert!(
        hso.get("permissionDecision").is_none(),
        "with the real {}={} set, this path emits no permissionDecision, got {v}",
        observe::OBSERVE_ONLY_ENV,
        observe::OBSERVE_ONLY_OPT_IN
    );

    // And the ledger keeps `undetermined` distinct from `tainted`.
    let records = ledger_records(&f);
    assert_eq!(records.len(), 1, "exactly one record, got {records:?}");
    assert_eq!(
        records[0].check, "undetermined",
        "an unreadable store must be recorded as `undetermined`, never folded into `tainted`"
    );
    assert!(
        records[0].sources.is_empty(),
        "an unreadable marker cannot name what tainted it, so sources must stay empty, got {:?}",
        records[0].sources
    );
}

/// KILLS FAULT F-D2 (swallowing `observe::append`'s `Err` in `emit_gate` by
/// removing the `eprintln!` at `src/main.rs:294-299` — injected by the audit
/// while the whole suite stayed GREEN, because no test read the child's stderr).
///
/// A lost ledger line means the measurement UNDER-COUNTS. That cannot be
/// silent: a suppression that was never recorded, reported as nothing at all,
/// makes the tally read lower than the real fire-rate — the statistic version of
/// "I could not check" rendered as "nothing to report".
///
/// The fault is planted by making the ledger path a DIRECTORY, so
/// `OpenOptions::new().create(true).append(true).open(path)` cannot succeed
/// (`EISDIR`). Deliberately not a `chmod`: a directory is unopenable-for-append
/// for **root too**, so this cannot go vacuous under a root test run the way a
/// permission fault would. The explicit plant-check below still reports a loud
/// SKIPPED if the fault somehow does not take, rather than passing silently.
///
/// HOW THIS FAILS against a wrong implementation: drop the `eprintln!` (or
/// downgrade it to a comment / a `let _ =`) and stderr no longer mentions the
/// under-count → the stderr assertion fails. Turn the append failure into an
/// early return that skips the `println!` and the stdout assertion fails
/// instead (the suppression must STILL be visible even when it cannot be
/// recorded).
#[test]
fn ledger_append_failure_is_reported_on_stderr_and_still_shows_the_suppression() {
    let f = fixture("observe-append-fails");
    let env = observe_only_env();

    // Plant the fault: a directory where the ledger file must be written.
    let ledger = ledger_path(&f);
    let planted = std::fs::create_dir_all(&ledger).is_ok() && ledger.is_dir();
    if !planted {
        // Cannot-determine resolves to the restricted side (CLAUDE.md §3). The
        // previous shape printed this note and `return`ed, which cargo reports
        // as a PASS while hiding the note — "proves NOTHING" was being rendered
        // as green. A test that could not inject its fault must fail.
        panic!(
            "CANNOT VERIFY: could not plant the append fault — a directory at {} could not be \
             created, so this run proves NOTHING about the append-failure diagnostic. Failing \
             rather than reporting a vacuous green.",
            ledger.display()
        );
    }

    let (code, _, _) = run(
        "mark",
        &mark_payload(
            "WebFetch",
            serde_json::json!({"url": "https://example.com"}),
            &f.cwd,
            &f.session,
        ),
        &f.cwd,
        &f.state_dir,
        &env,
    );
    assert_eq!(code, 0);

    let (code, stdout, stderr) = run(
        "gate",
        &gate_payload(
            "Edit",
            serde_json::json!({"file_path": "src/main.rs"}),
            &f.cwd,
            &f.session,
        ),
        &f.cwd,
        &f.state_dir,
        &env,
    );
    assert_eq!(code, 0, "gate must always exit 0");

    // The fault really did take: nothing could have been appended.
    assert!(
        ledger.is_dir(),
        "the planted fault must still be in place at the end of the run"
    );

    // (a) the suppression is still visible on stdout even though it could not
    //     be recorded.
    assert!(
        hook_specific(&hook_json(&stdout))
            .get("additionalContext")
            .is_some(),
        "an unrecordable suppression must STILL be reported on stdout, got: {stdout:?}"
    );

    // (b) and the lost record is announced on stderr as an under-count.
    assert!(
        stderr.contains("under-count"),
        "a failed ledger append must say on stderr that the measurement UNDER-COUNTS \
         (swallowing this Err makes a lost suppression invisible), got stderr: {stderr:?}"
    );
    assert!(
        stderr.contains("observe-only"),
        "the diagnostic must name the observe-only ledger it lost, got stderr: {stderr:?}"
    );
}
