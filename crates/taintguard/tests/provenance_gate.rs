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

/// EMPTY stdin stays silent on every subcommand — and that is a different case
/// from a NON-EMPTY payload that could not be parsed (see
/// [`unparseable_stdin_on_gate_fails_closed_instead_of_allowing_silently`]).
///
/// Empty means this process was handed nothing to judge at all: Claude Code
/// always writes a payload, so an empty read is "not invoked as a hook" (a bare
/// `taintguard gate` at a shell, a probe), not "a hook payload I failed to
/// understand". Same split as `blastguard::main::run` and `ctxrot`'s hooks, which
/// both return silently on empty and emit a fail-closed answer on unparseable.
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

// ── same-repository git worktrees must not taint (backlog eb39308e) ──────────

/// Run `git`, panicking with stderr on failure so a broken fixture can never
/// masquerade as the property under test holding.
fn git(args: &[&str]) {
    let out = std::process::Command::new("git")
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .expect("git must be on PATH to run this test");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A real repo with one commit inside `holder`, plus a real linked worktree
/// created as a SIBLING of it (the condukt/flow shape). Returns
/// `(main_checkout, linked_worktree)`.
fn init_repo_and_linked_worktree(holder: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
    let main = holder.join("repo");
    std::fs::create_dir_all(&main).unwrap();
    let main_s = main.to_string_lossy().into_owned();
    git(&["-C", &main_s, "init", "-q", "-b", "main"]);
    std::fs::write(main.join("a.rs"), "fn main() {}").unwrap();
    git(&["-C", &main_s, "add", "-A"]);
    git(&[
        "-C",
        &main_s,
        "-c",
        "user.email=t@example.com",
        "-c",
        "user.name=t",
        "commit",
        "-qm",
        "init",
    ]);
    let linked = holder.join("linked");
    let linked_s = linked.to_string_lossy().into_owned();
    git(&[
        "-C", &main_s, "worktree", "add", "-q", &linked_s, "-b", "topic", "main",
    ]);
    (main, linked)
}

/// THE eb39308e REGRESSION, end to end through the real binary.
///
/// Reproduces exactly what made condukt/flow subagents unable to edit: the hook
/// payload's `cwd` is the session's project root (the main checkout), the
/// subagent `Read`s an absolute path inside a linked worktree that is not under
/// that root, and then tries to `Edit` in that worktree.
///
/// RED before the fix: `mark` classified the read `Untrusted`, recorded
/// `external-read`, and this `gate` returned
/// `permissionDecision: "ask"` — in a subagent there is no human to answer an
/// `ask`, so the edit simply never happened.
///
/// The control that keeps this from being "trust everything" lives in
/// [`a_read_in_an_unrelated_git_repo_still_taints`] directly below.
#[test]
fn a_read_in_a_sibling_git_worktree_of_the_same_repo_does_not_taint() {
    let f = fixture("same-repo-worktree");
    let (main, linked) = init_repo_and_linked_worktree(&f.cwd);
    let target = linked.join("a.rs");

    let (code, _, _) = run(
        "mark",
        &mark_payload(
            "Read",
            serde_json::json!({"file_path": target.to_string_lossy()}),
            &main,
            &f.session,
        ),
        &main,
        &f.state_dir,
        &[],
    );
    assert_eq!(code, 0);

    let (code, stdout, _) = run(
        "gate",
        &gate_payload(
            "Edit",
            serde_json::json!({"file_path": target.to_string_lossy()}),
            &main,
            &f.session,
        ),
        &main,
        &f.state_dir,
        INTERACTIVE_ENV,
    );
    assert_eq!(code, 0);
    assert!(
        stdout.trim().is_empty(),
        "reading a file in a linked worktree of the SAME repository is the operator's \
         own checkout, not untrusted-provenance content — the gate must stay silent, \
         got: {stdout:?}"
    );
}

/// CONTROL for the test above: a `Read` in an UNRELATED git repository must
/// still taint. Without this, "the target is inside some git repo" would satisfy
/// the test above while classifying every other project on the machine as
/// trusted.
#[test]
fn a_read_in_an_unrelated_git_repo_still_taints() {
    let f = fixture("other-repo-worktree");
    let ours = f.cwd.join("ours");
    let theirs = f.cwd.join("theirs");
    std::fs::create_dir_all(&ours).unwrap();
    std::fs::create_dir_all(&theirs).unwrap();
    let (main, _linked) = init_repo_and_linked_worktree(&ours);
    let (other_main, other_linked) = init_repo_and_linked_worktree(&theirs);

    for target in [other_main.join("a.rs"), other_linked.join("a.rs")] {
        let session = format!("{}-{}", f.session, target.display().to_string().len());
        let (code, _, _) = run(
            "mark",
            &mark_payload(
                "Read",
                serde_json::json!({"file_path": target.to_string_lossy()}),
                &main,
                &session,
            ),
            &main,
            &f.state_dir,
            &[],
        );
        assert_eq!(code, 0);

        let (code, stdout, _) = run(
            "gate",
            &gate_payload(
                "Edit",
                serde_json::json!({"file_path": "a.rs"}),
                &main,
                &session,
            ),
            &main,
            &f.state_dir,
            INTERACTIVE_ENV,
        );
        assert_eq!(code, 0);
        let decision = permission_decision(&stdout);
        assert!(
            decision.as_deref() == Some("ask") || decision.as_deref() == Some("deny"),
            "reading {} — a DIFFERENT repository — must still taint the session, got: {stdout:?}",
            target.display()
        );
    }
}

// ── fail-closed: an unparseable hook payload (backlog 9a28b98c) ──────────────

/// `gate` carries a VERDICT, so "I was handed a payload and could not parse it"
/// must resolve to the restricted side, not to the silent allow that
/// `harness_core::hook::run_hook`'s `exit(0)` produces when nothing is printed.
///
/// RED before the fix: every one of these payloads produced exit 0 with EMPTY
/// stdout, which Claude Code reads as "this hook has no opinion" — i.e. an
/// allow. Measured directly against the real binary, which is the only place the
/// `run_hook` wrapper is in the loop.
///
/// Mirrors `blastguard::main::run`'s `UNREADABLE_PAYLOAD` arm (empty → silent,
/// non-empty-unparseable → ask, hardened to deny with no human).
#[test]
fn unparseable_stdin_on_gate_fails_closed_instead_of_allowing_silently() {
    let f = fixture("unparseable-gate");
    // Every one of these is NON-EMPTY (so it is a payload, not an absent one)
    // and cannot become a `HookInput`.
    let payloads = [
        "not json at all",
        "{ \"tool_name\": ",      // truncated object
        "[]",                     // valid JSON, wrong shape
        "\"a string\"",           // valid JSON, wrong shape
        "42",                     // valid JSON, wrong shape
        "{\"tool_name\": 12345}", // right key, wrong type
        "\u{0}\u{1}\u{2}",        // binary noise
    ];
    for payload in payloads {
        let (code, stdout, _) = run("gate", payload, &f.cwd, &f.state_dir, INTERACTIVE_ENV);
        assert_eq!(code, 0, "gate must still always exit 0 ({payload:?})");
        let decision = permission_decision(&stdout);
        assert!(
            decision.as_deref() == Some("ask") || decision.as_deref() == Some("deny"),
            "an unparseable NON-EMPTY gate payload is cannot-determine and must fail \
             closed to ask/deny; got exit {code} and stdout {stdout:?} for {payload:?}"
        );
        let reason = serde_json::from_str::<serde_json::Value>(stdout.trim()).unwrap()
            ["hookSpecificOutput"]["permissionDecisionReason"]
            .as_str()
            .expect("a fail-closed decision must carry a reason")
            .to_string();
        assert!(
            reason.contains("could not"),
            "the reason must say the payload could not be read, not invent a taint \
             finding: {reason}"
        );
    }
}

/// The same unparseable payload with NO human present hardens the `ask` to a
/// `deny` — an `ask` nobody can answer is not a pause, and this path is reached
/// most often in exactly the headless/subagent context that cannot answer one.
#[test]
fn unparseable_stdin_on_gate_hardens_to_deny_when_headless() {
    let f = fixture("unparseable-gate-headless");
    let (code, stdout, _) = run(
        "gate",
        "not json at all",
        &f.cwd,
        &f.state_dir,
        HEADLESS_ENV,
    );
    assert_eq!(code, 0);
    assert_eq!(
        permission_decision(&stdout).as_deref(),
        Some("deny"),
        "no human ⇒ the fail-closed ask must harden to deny, got: {stdout:?}"
    );
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
    //
    // The list is chosen so that EVERY plausible loosening of the exact `"1"`
    // comparison is killed by at least one row: trimming (`" 1"`, `"1 "`,
    // `"\t1"`, `"1\n"` — the shape a `TAINTGUARD_OBSERVE_ONLY=$(...)` shell
    // substitution produces), case-folding (`"TRUE"`), truthy-string parsing
    // (`"true"`, `"yes"`, `"on"`), numeric parsing (`"01"`, `"1.0"`),
    // "any non-empty value" (`"false"`, `"off"`, `"no"`), and "the var is present
    // at all" (`"0"`, `""`).
    let non_opt_in: &[Option<&str>] = &[
        Some(" 1"),
        Some("1 "),
        Some("\t1"),
        Some("1\n"),
        Some("true"),
        Some("TRUE"),
        Some("01"),
        Some("1.0"),
        Some("yes"),
        Some("on"),
        Some("false"),
        Some("off"),
        Some("no"),
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

/// THE DECISIVE TEST for "observe-only must NOT suppress a cannot-determine"
/// (CLAUDE.md §3: 判定不能は必ず制限側に解決する).
///
/// This test previously had the name
/// `undetermined_state_with_the_real_observe_only_env_set_is_reported_not_silent`
/// and asserted the OPPOSITE of what it asserts now: it pinned production's
/// then-actual behaviour, a SUPPRESSED-but-reported observe warning with **no**
/// `permissionDecision` and one `check: "undetermined"` ledger line. Its own
/// docstring flagged that as an open design question for a human ("If
/// 'cannot-determine must enforce regardless of posture' is meant to cover the
/// `Undetermined` path too … this test is the place that will go red when it
/// changes"). That decision has now been made — **yes, §3 covers the
/// `Undetermined` path, not just the panic path** — so this is that red, resolved
/// in the direction of enforcing.
///
/// ## What it pins
///
/// With the REAL `TAINTGUARD_OBSERVE_ONLY=1` in the child's environment and a
/// corrupt on-disk marker (so `state::check` returns `Check::Undetermined`), the
/// binary must still print a real `ask`/`deny` `permissionDecision`. Observe-only
/// is an affordance for measuring a *working* gate; it is not a licence to
/// swallow "I could not read my own store".
///
/// The env var is driven through the real binary on purpose. Every unit test
/// injects the posture into `decide_gate_with` and therefore never calls
/// `observe::posture()`, the only reader of the var — a loose parse there once
/// stayed green across the entire unit suite (see this file's module docs). Only
/// a child process with the var actually set makes `posture()` the code under
/// test, which is why the decisive assertion lives here.
///
/// ## What this test does NOT do — stated explicitly
///
/// It does **not** test the `catch_unwind` panic arm. That arm is unreachable
/// from outside the process: the binary exposes no fault-injection hook, and
/// every externally-plantable fault (corrupt marker, wrong-schema marker,
/// unwritable store, missing `file_path`) is *handled* — it lands on
/// `Check::Undetermined`, not on a panic. The panic barrier remains pinned only
/// by the in-process unit tests, and this test claims nothing about it.
///
/// It also does not prove observe-only still works at all — a binary with
/// observe-only deleted wholesale would pass this test. That is what
/// `observe_only_still_suppresses_a_genuinely_tainted_session_...` (the
/// anti-vacuity anchor below) is for.
///
/// HOW THIS FAILS against a wrong implementation:
///   * `Undetermined` + `ObserveOnly` routed to `Observe` (the pre-change
///     behaviour) → stdout carries `additionalContext` but no
///     `permissionDecision` → the decision assertion fails. This is the RED at
///     HEAD.
///   * `Undetermined` treated as `Clean` under observe-only → stdout empty →
///     `permission_decision` returns `None` → same assertion fails ("an
///     unreadable store is not a clean turn").
///   * `posture()` mis-reading the var such that this path silently softens →
///     caught here rather than nowhere.
#[test]
fn observe_only_must_not_suppress_a_cannot_determine_corrupt_marker() {
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

    let decision = permission_decision(&stdout);
    assert!(
        decision.as_deref() == Some("ask") || decision.as_deref() == Some("deny"),
        "with the real {}={} set, a CORRUPT marker (cannot-determine) must STILL emit an \
         ask/deny permissionDecision — observe-only must not suppress a cannot-determine \
         (CLAUDE.md §3). got decision={decision:?}, stdout={stdout:?}",
        observe::OBSERVE_ONLY_ENV,
        observe::OBSERVE_ONLY_OPT_IN,
    );
}

/// THE SECOND HALF OF THE DECISIVE CONTRACT: an enforced cannot-determine under
/// observe-only appends **NO** ledger line.
///
/// This is deliberate, not an oversight, which is why it is asserted rather than
/// left to a comment. The ledger — and `taintguard tally`'s `suppressed` counter
/// that reads it — is *defined* as the count of **suppressed enforcements**. This
/// event was not suppressed; it enforced. Recording it would inflate a counter
/// whose very name asserts suppression, i.e. it would make the measurement lie in
/// the same direction the observe-only mode exists to measure honestly.
///
/// The ledger assertion comes FIRST so the failure message names the contract
/// under test; the enforcement assertion follows as the tie-in that stops this
/// from passing against an implementation that simply stopped writing the ledger
/// while ALSO still suppressing.
///
/// ## What this test does NOT do
///
/// It says nothing about the ledger on the *tainted* path — a suppressed taint
/// must still be recorded, and that is
/// `observe_only_still_suppresses_a_genuinely_tainted_session_...` below. Without
/// that anchor, "the ledger stayed at (0, 0)" would also be satisfied by a
/// binary that never writes a ledger at all.
///
/// HOW THIS FAILS against a wrong implementation:
///   * pre-change behaviour (suppress + record) → tally is `(1, 0)` → the first
///     assertion fails. This is the RED at HEAD.
///   * enforce-but-still-record → tally is `(1, 0)` → same assertion fails.
///   * enforce, record nothing, but also stop enforcing → the tie-in decision
///     assertion fails.
#[test]
fn observe_only_enforced_cannot_determine_appends_no_ledger_line() {
    let f = fixture("observe-undet-no-ledger");
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

    // THE CONTRACT: nothing suppressed ⇒ nothing on the suppression ledger.
    assert_eq!(
        ledger_tally(&f),
        (0, 0),
        "an ENFORCED cannot-determine under observe-only must append NO ledger line — the \
         ledger and `taintguard tally`'s `suppressed` counter are defined as suppressed \
         enforcements, and this event was not suppressed. stdout was: {stdout:?}"
    );
    // …and the file should not even exist, since nothing was ever appended.
    let path = ledger_path(&f);
    assert!(
        !path.exists(),
        "no ledger file should have been created at all, but {} exists with: {:?}",
        path.display(),
        std::fs::read_to_string(&path).ok()
    );

    // TIE-IN (stops a vacuous pass): the event really was enforced.
    let decision = permission_decision(&stdout);
    assert!(
        decision.as_deref() == Some("ask") || decision.as_deref() == Some("deny"),
        "the un-recorded event must be un-recorded BECAUSE it enforced, not because \
         enforcement vanished; got decision={decision:?}, stdout={stdout:?}"
    );
}

/// A SECOND FAULT ROUTE to the same contract: a valid-JSON but WRONG-SCHEMA
/// marker (`{"foo":123}`), which `wrong_schema_marker_fails_closed_to_ask_or_deny`
/// already pins under the enforce posture, must also enforce under observe-only.
///
/// Worth its own test because the corrupt-marker route and the wrong-schema route
/// reach `Check::Undetermined` through different code in `state::read_state`
/// (a serde parse error vs. a missing required field). An implementation that
/// fixed only one of them — e.g. by special-casing unparseable bytes — would pass
/// the corrupt-marker test and fail here.
///
/// ## What this test does NOT do
///
/// It does not assert on the ledger; that is the previous test's job, and
/// duplicating it here would only add a second place to update.
#[test]
fn observe_only_must_not_suppress_a_cannot_determine_wrong_schema_marker() {
    let f = fixture("observe-undet-wrong-schema");
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
    let marker =
        find_marker_file(&f.state_dir).expect("mark must have written exactly one marker file");
    // Valid JSON, but missing the required `tainted` field entirely.
    std::fs::write(&marker, br#"{"foo":123}"#).expect("overwrite with wrong-schema marker");

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
        &env,
    );
    assert_eq!(code, 0, "gate must always exit 0");

    let decision = permission_decision(&stdout);
    assert!(
        decision.as_deref() == Some("ask") || decision.as_deref() == Some("deny"),
        "with the real {}={} set, a WRONG-SCHEMA marker (cannot-determine) must STILL emit an \
         ask/deny permissionDecision — a serde default to clean is a fail-open in either \
         posture. got decision={decision:?}, stdout={stdout:?}",
        observe::OBSERVE_ONLY_ENV,
        observe::OBSERVE_ONLY_OPT_IN,
    );
}

/// ANTI-VACUITY ANCHOR — PASSES BOTH BEFORE AND AFTER the change, by design.
///
/// The tests above would all be satisfied by a binary with observe-only ripped
/// out entirely (always enforce). This one proves it was not: with the same env
/// var set, a GENUINELY `Tainted` session is still SUPPRESSED — no
/// `permissionDecision` anywhere in the output — and the suppression is still
/// recorded as exactly ONE ledger line.
///
/// Both halves are asserted in ONE run so the pair cannot drift apart: "no
/// decision" and "one recorded suppression" are the same event seen from two
/// sides, and an implementation that emitted the warning but stopped recording it
/// (or recorded it but stopped warning) must fail rather than half-pass.
///
/// The `permissionDecision` absence is checked twice on purpose: once on the
/// parsed object (so a payload whose `hookSpecificOutput` is missing entirely
/// cannot satisfy it vacuously — `hook_specific` panics in that case) and once as
/// a raw substring over the whole of stdout (so a decision smuggled in under some
/// other key or nesting is still caught).
///
/// ## What this test does NOT do
///
/// It does not check the exact warning wording — that is
/// `observe_only_suppression_is_visible_on_stdout_with_a_top_level_system_message`.
#[test]
fn observe_only_still_suppresses_a_genuinely_tainted_session_and_records_exactly_one_line() {
    let f = fixture("observe-tainted-anchor");
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
    assert!(
        hso.get("permissionDecision").is_none(),
        "a genuinely TAINTED session under observe-only must still be SUPPRESSED (no \
         permissionDecision) — otherwise observe-only was deleted wholesale rather than \
         narrowed to exclude cannot-determine, got {v}"
    );
    assert!(
        !stdout.contains("permissionDecision"),
        "no `permissionDecision` may appear ANYWHERE in the output for a suppressed taint, \
         got: {stdout:?}"
    );
    assert!(
        hso.get("additionalContext")
            .and_then(|c| c.as_str())
            .is_some_and(|c| !c.trim().is_empty()),
        "the suppressed taint must still be reported, got {v}"
    );

    let records = ledger_records(&f);
    assert_eq!(
        records.len(),
        1,
        "exactly ONE suppressed enforcement must be recorded, got {records:?}"
    );
    assert_eq!(
        records[0].check, "tainted",
        "a suppressed genuine taint must be recorded as `tainted`, got {:?}",
        records[0]
    );
    assert_eq!(
        ledger_tally(&f),
        (1, 0),
        "the tally must agree with the records read back"
    );
}

/// ANTI-VACUITY ANCHOR #2 — PASSES BOTH BEFORE AND AFTER the change.
///
/// `Check::Clean` → `Silent`, in the observe-only posture too. Cheap, and it
/// pins the one thing a "make cannot-determine always enforce" change could
/// plausibly over-reach into: turning the empty/clean store into a warning or a
/// decision. A gate that starts speaking on every clean turn is not fail-closed,
/// it is broken.
///
/// ## What this test does NOT do
///
/// A clean store here is an *empty* store (no `mark` ever ran). It does not cover
/// the mark-then-clear route to clean; `clear_after_stop_restores_a_clean_gate`
/// does that (under the enforce posture).
#[test]
fn observe_only_with_a_clean_session_is_still_silent() {
    let f = fixture("observe-clean-anchor");
    let env = observe_only_env();

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
        &env,
    );
    assert_eq!(code, 0, "gate must always exit 0");
    assert!(
        stdout.trim().is_empty(),
        "a clean session must allow SILENTLY even under observe-only, got: {stdout:?}"
    );
    assert_eq!(
        ledger_tally(&f),
        (0, 0),
        "a clean turn must not append a ledger line"
    );
}

/// KILLS FAULT 38b14814 AT ITS OWN WORDING: "tainted+suppressed is byte-for-byte
/// identical to clean".
///
/// The two existing tests each pin one side — `observe_only_with_a_clean_session_is_still_silent`
/// asserts the clean side is empty, `observe_only_suppression_is_visible_on_stdout_with_a_top_level_system_message`
/// asserts the suppressed side has the right shape — but nothing put the two
/// **byte strings** next to each other, which is the property the backlog item
/// actually names. A reader checking "can these two be confused downstream?" had
/// to reason across two files. This asserts it directly, in ONE fixture, with ONE
/// posture and ONE binary: same env, same cwd, two sessions, and the raw stdout
/// of each compared.
///
/// HOW THIS FAILS against a wrong implementation: delete or condition away
/// `emit_gate`'s `Observe`-arm `println!` and the two byte strings become equal
/// (both empty), so `assert_ne!` fires with both sides shown.
#[test]
fn a_suppressed_taint_is_not_byte_identical_to_a_clean_turn_on_stdout() {
    let f = fixture("observe-byte-differential");
    let env = observe_only_env();
    let clean_session = format!("{}-clean", f.session);
    let tainted_session = format!("{}-tainted", f.session);

    // (a) clean.
    let (_, clean_stdout, _) = run(
        "gate",
        &gate_payload(
            "Bash",
            serde_json::json!({"command": "echo hi"}),
            &f.cwd,
            &clean_session,
        ),
        &f.cwd,
        &f.state_dir,
        &env,
    );

    // (b) tainted, and therefore SUPPRESSED under this posture.
    let (code, _, _) = run(
        "mark",
        &mark_payload(
            "WebFetch",
            serde_json::json!({"url": "https://example.com"}),
            &f.cwd,
            &tainted_session,
        ),
        &f.cwd,
        &f.state_dir,
        &env,
    );
    assert_eq!(code, 0);
    let (_, tainted_stdout, _) = run(
        "gate",
        &gate_payload(
            "Bash",
            serde_json::json!({"command": "echo hi"}),
            &f.cwd,
            &tainted_session,
        ),
        &f.cwd,
        &f.state_dir,
        &env,
    );

    assert_ne!(
        clean_stdout.as_bytes(),
        tainted_stdout.as_bytes(),
        "a SUPPRESSED taint must not be byte-identical to a clean turn on stdout — \
         otherwise observe-only is indistinguishable from finding nothing. \
         clean={clean_stdout:?} tainted={tainted_stdout:?}"
    );
    // Directional, so the assertion above cannot be satisfied by making the
    // CLEAN turn start printing something.
    assert!(
        clean_stdout.is_empty(),
        "the clean turn must print NOTHING AT ALL (not even whitespace), got: {clean_stdout:?}"
    );
    assert!(
        !tainted_stdout.trim().is_empty(),
        "the suppressed turn must print the observe-only warning, got: {tainted_stdout:?}"
    );
    assert_eq!(
        ledger_tally(&f),
        (1, 0),
        "exactly the suppressed half must have appended a ledger line"
    );
}

/// THE DIFFERENTIAL, in ONE project and therefore ONE ledger: the same binary,
/// the same env var, the same `cwd` — and two sessions that must be treated
/// DIFFERENTLY.
///
/// This is the test that cannot be satisfied by moving a single global switch. A
/// build that always enforces fails on the tainted half; a build that always
/// suppresses fails on the undetermined half; the pre-change build fails on the
/// undetermined half (both its decision and its ledger line). Only "suppress a
/// recorded taint, enforce a cannot-determine" passes.
///
/// Sharing one `cwd` is the point: the ledger is project-scoped
/// (`observe::ledger_path` takes only `cwd`), so the tally after the undetermined
/// half and the tally after the tainted half are readings of the SAME counter.
/// That makes "the undetermined event did not inflate the suppressed count"
/// directly observable rather than inferred from two separate stores.
///
/// The undetermined session is driven FIRST on purpose: `find_marker_file`
/// returns whichever marker it walks into first, so it is only unambiguous while
/// exactly one session has marked.
///
/// ## What this test does NOT do
///
/// It does not cover the headless (`deny`) hardening — `observe_only_env()` is
/// deliberately interactive, so a wrongly-softening implementation still has to
/// produce the *softer* `ask` and is caught by the same assertion.
#[test]
fn one_ledger_records_the_suppressed_taint_but_not_the_enforced_cannot_determine() {
    let f = fixture("observe-differential");
    let env = observe_only_env();

    // ── half 1: cannot-determine ⇒ ENFORCE, and record NOTHING ──────────────
    let undet_session = format!("{}-undetermined", f.session);
    let (code, _, _) = run(
        "mark",
        &mark_payload(
            "WebFetch",
            serde_json::json!({"url": "https://example.com"}),
            &f.cwd,
            &undet_session,
        ),
        &f.cwd,
        &f.state_dir,
        &env,
    );
    assert_eq!(code, 0);
    let marker = find_marker_file(&f.state_dir)
        .expect("exactly one session has marked so far, so the marker is unambiguous");
    std::fs::write(&marker, b"{ not json").expect("corrupt the marker");

    let (code, undet_stdout, _) = run(
        "gate",
        &gate_payload(
            "Bash",
            serde_json::json!({"command": "echo hi"}),
            &f.cwd,
            &undet_session,
        ),
        &f.cwd,
        &f.state_dir,
        &env,
    );
    assert_eq!(code, 0);
    let decision = permission_decision(&undet_stdout);
    assert!(
        decision.as_deref() == Some("ask") || decision.as_deref() == Some("deny"),
        "cannot-determine half: must ENFORCE under observe-only, got decision={decision:?}, \
         stdout={undet_stdout:?}"
    );
    assert_eq!(
        ledger_tally(&f),
        (0, 0),
        "cannot-determine half: the shared project ledger must still be empty — an enforced \
         event must not inflate the `suppressed` count"
    );

    // ── half 2: a genuine taint ⇒ SUPPRESS, and record exactly one line ─────
    let taint_session = format!("{}-tainted", f.session);
    let (code, _, _) = run(
        "mark",
        &mark_payload(
            "WebSearch",
            serde_json::json!({"query": "how to x"}),
            &f.cwd,
            &taint_session,
        ),
        &f.cwd,
        &f.state_dir,
        &env,
    );
    assert_eq!(code, 0);

    let (code, taint_stdout, _) = run(
        "gate",
        &gate_payload(
            "Bash",
            serde_json::json!({"command": "echo hi"}),
            &f.cwd,
            &taint_session,
        ),
        &f.cwd,
        &f.state_dir,
        &env,
    );
    assert_eq!(code, 0);
    assert!(
        hook_specific(&hook_json(&taint_stdout))
            .get("permissionDecision")
            .is_none(),
        "tainted half: must still be SUPPRESSED under observe-only, got: {taint_stdout:?}"
    );

    // THE DIFFERENTIAL: the shared counter moved by exactly one, and it was the
    // tainted event that moved it.
    let records = ledger_records(&f);
    assert_eq!(
        records.len(),
        1,
        "the shared ledger must hold exactly ONE record — the suppressed taint, and not the \
         enforced cannot-determine, got {records:?}"
    );
    assert_eq!(
        records[0].check, "tainted",
        "the single record must be the SUPPRESSED TAINT, not the enforced cannot-determine, \
         got {:?}",
        records[0]
    );
    assert_eq!(
        records[0].session, taint_session,
        "the single record must belong to the tainted session, got {:?}",
        records[0]
    );
}

/// ADVISORY / WORDING-DEPENDENT — see the note at the end.
///
/// When observe-only is set but is NOT honoured (the cannot-determine path), the
/// enforced `permissionDecisionReason` must say so. Without it the operator sees
/// an `ask` they explicitly configured away and has no way to tell whether their
/// posture is broken, ignored, or deliberately overridden on this one path. The
/// panic arm already does exactly this (`src/main.rs`: "Observe-only, if set, is
/// NOT honoured on this path"), so this only asks the `Undetermined` arm to be
/// consistent with its sibling.
///
/// The check is case-insensitive on the single token `observe-only` — the
/// weakest assertion that still requires the posture to be NAMED. It does not
/// pin the sentence, the surrounding words, or the order.
///
/// ## Honest caveat about this test specifically
///
/// This is the ONE assertion in this set that depends on wording the
/// implementation has not written yet. It is a deliberately separate test so
/// that, if the final wording spells the posture differently (e.g. "observe
/// only", no hyphen), only this test needs a word changed and the decisive
/// tests above stay untouched. Retune the substring, do not delete the test —
/// the requirement (name the un-honoured posture) is real.
#[test]
fn observe_only_enforced_cannot_determine_reason_names_the_unhonoured_posture() {
    let f = fixture("observe-undet-reason");
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
    assert_eq!(code, 0);

    let v = hook_json(&stdout);
    let hso = hook_specific(&v);
    let reason = hso
        .get("permissionDecisionReason")
        .and_then(|r| r.as_str())
        .unwrap_or_else(|| {
            panic!("an enforced decision must carry a permissionDecisionReason, got {v}")
        });
    assert!(
        reason.to_lowercase().contains("observe-only"),
        "the enforced reason must NAME the posture it is not honouring, so the operator can \
         tell an ignored posture from a deliberately-overridden one: {reason}"
    );
    assert!(
        reason.to_lowercase().contains("taintguard"),
        "the reason must name the gate that produced it: {reason}"
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
