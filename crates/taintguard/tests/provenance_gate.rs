// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! End-to-end tests: drive the real built `taintguard` binary's three
//! subcommands (`mark` / `gate` / `clear`) over stdin, exactly the way Claude
//! Code invokes them as PostToolUse / PreToolUse / Stop hooks, and assert on
//! stdout + exit code. Mirrors `crates/blastguard/tests/integration.rs` and
//! `crates/blastguard/tests/ask_hardening_invariant.rs`'s `run_with_env`
//! pattern (env fully cleared, then only the variables the scenario cares
//! about are set, so a stray `CLAUDECODE`/`CLAUDE_CODE_ENTRYPOINT` leaking in
//! from the process actually running this suite can't contaminate the
//! result).
//!
//! Each test gets its OWN temp dir for `cwd` (the project root) AND its own
//! `TAINTGUARD_STATE_DIR` (so marker files never collide across tests or with
//! a real `~/.taintguard`), and its own session id (so `mark`/`gate`/`clear`
//! invocations within one test never collide with another test's marker).

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

/// Run the `taintguard` binary's subcommand `sub` (`"mark"`/`"gate"`/`"clear"`)
/// with a fully-controlled environment: cleared, then `cwd`/`state_dir` pinned
/// and `extra_env` applied on top. Returns (exit_code, stdout).
fn run(
    sub: &str,
    payload: &str,
    cwd: &Path,
    state_dir: &Path,
    extra_env: &[(&str, &str)],
) -> (i32, String) {
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
    let (code, _) = run(
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

    let (code, stdout) = run(
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
    let (code, _) = run(
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

    let (code, stdout) = run(
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
    let (code, _) = run(
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

    let (code, stdout) = run(
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

    let (code, _) = run(
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

    let (code, stdout) = run(
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
    let (code, stdout) = run(
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

    let (code, _) = run(
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

    let (code, stdout) = run(
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
    let (code, _) = run(
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
    let (_, stdout) = run(
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

    let (code, stdout) = run(
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

    let (code, stdout) = run(
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
        let (code, stdout) = run(sub, "", &f.cwd, &f.state_dir, &[]);
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
    let (code, _) = run(
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

    let (code, stdout) = run(
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
    let (code, _) = run(
        "mark",
        &mark_payload("Read", serde_json::json!({}), &f.cwd, &f.session),
        &f.cwd,
        &f.state_dir,
        &[],
    );
    assert_eq!(code, 0);

    let (code, stdout) = run(
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
    let (code, stdout) = run(
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
        let (mark_code, _) = run(
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
        let (gate_code, stdout) = run(
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
    let (code, _) = run(
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

    let (code, stdout) = run(
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
    let (code, _) = run(
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

    let (code, stdout) = run(
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
