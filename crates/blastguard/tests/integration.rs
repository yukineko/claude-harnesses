// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! End-to-end tests: feed a PreToolUse payload to the real built binary over
//! stdin and assert on stdout + exit code.

use std::io::Write;
use std::process::{Command, Stdio};

/// Run the binary with `payload` on stdin. Returns (exit_code, stdout).
fn run(payload: &str) -> (i32, String) {
    let bin = env!("CARGO_BIN_EXE_blastguard");
    let mut child = Command::new(bin)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("binary spawns");
    if let Some(mut child_stdin) = child.stdin.take() {
        let _ = child_stdin.write_all(payload.as_bytes());
    }
    let out = child.wait_with_output().expect("binary runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

#[allow(dead_code)]
fn run_with_env(payload: &str, envs: &[(&str, &str)]) -> (i32, String) {
    let bin = env!("CARGO_BIN_EXE_blastguard");
    let mut cmd = Command::new(bin);
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().expect("binary spawns");
    if let Some(mut child_stdin) = child.stdin.take() {
        let _ = child_stdin.write_all(payload.as_bytes());
    }
    let out = child.wait_with_output().expect("binary runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

#[test]
fn bash_rm_rf_is_denied() {
    let payload = r#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"rm -rf build"}}"#;
    let (code, stdout) = run(payload);
    assert_eq!(code, 0, "hook must always exit 0");
    assert!(
        stdout.contains(r#""permissionDecision":"deny""#),
        "expected deny, got stdout: {stdout}"
    );
}

#[test]
fn write_empty_overwrite_is_denied() {
    let payload = r#"{"hook_event_name":"PreToolUse","tool_name":"Write","tool_input":{"file_path":"src/main.rs","content":""}}"#;
    let (code, stdout) = run(payload);
    assert_eq!(code, 0);
    assert!(
        stdout.contains(r#""permissionDecision":"deny""#),
        "expected deny, got stdout: {stdout}"
    );
}

#[test]
fn config_file_edit_to_protected_path_is_denied() {
    // Renamed from `config_file_edit_is_allowed_silently`: this was the
    // end-to-end twin of the fail-open flipped in the unit tests (detect.rs
    // `write_with_content_or_to_config_is_allowed`) — an Edit to
    // `.claude/settings.json` decides which hooks/gates run at all, so it
    // must now Deny and be surfaced to the agent, not pass through silently.
    let payload = r#"{"hook_event_name":"PreToolUse","tool_name":"Edit","tool_input":{"file_path":".claude/settings.json"}}"#;
    let (code, stdout) = run(payload);
    assert_eq!(code, 0, "hook must always exit 0");
    assert!(
        stdout.contains(r#""permissionDecision":"deny""#),
        "expected deny, got stdout: {stdout}"
    );
}

#[test]
fn benign_command_is_allowed_silently() {
    let payload = r#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"cargo test"}}"#;
    let (code, stdout) = run(payload);
    assert_eq!(code, 0);
    assert!(
        stdout.trim().is_empty(),
        "allow must be silent, got: {stdout}"
    );
}

#[test]
fn empty_stdin_is_silent() {
    let (code, stdout) = run("");
    assert_eq!(code, 0);
    assert!(
        stdout.trim().is_empty(),
        "empty stdin must be silent, got: {stdout}"
    );
}

#[test]
fn config_file_delete_via_bash_is_allowed() {
    let payload = r#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"rm package.json"}}"#;
    let (code, stdout) = run(payload);
    assert_eq!(code, 0);
    assert!(
        stdout.trim().is_empty(),
        "allow must be silent, got: {stdout}"
    );
}

// ── shell-eval bypass: DENY cases ───────────────────────────────────────────

#[test]
fn eval_rm_rf_is_denied() {
    let payload = r#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"eval \"rm -rf /\""}}"#;
    let (code, stdout) = run(payload);
    assert_eq!(code, 0, "hook must always exit 0");
    assert!(
        stdout.contains(r#""permissionDecision":"deny""#),
        "eval with destructive payload must be denied, got stdout: {stdout}"
    );
}

#[test]
fn eval_rm_rf_var_is_denied() {
    // source with an opaque path re-analyses to harmless; eval with an inline
    // destructive command must still be caught.
    let payload = r#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"eval \"rm -rf /var\""}}"#;
    let (code, stdout) = run(payload);
    assert_eq!(code, 0, "hook must always exit 0");
    assert!(
        stdout.contains(r#""permissionDecision":"deny""#),
        "eval with inline rm -rf must be denied, got stdout: {stdout}"
    );
}

#[test]
fn exec_rm_rf_is_denied() {
    let payload = r#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"exec rm -rf /"}}"#;
    let (code, stdout) = run(payload);
    assert_eq!(code, 0, "hook must always exit 0");
    assert!(
        stdout.contains(r#""permissionDecision":"deny""#),
        "exec rm -rf must be denied, got stdout: {stdout}"
    );
}

#[test]
fn bash_c_rm_rf_is_denied() {
    let payload = r#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"bash -c \"rm -rf /\""}}"#;
    let (code, stdout) = run(payload);
    assert_eq!(code, 0, "hook must always exit 0");
    assert!(
        stdout.contains(r#""permissionDecision":"deny""#),
        "bash -c with destructive payload must be denied, got stdout: {stdout}"
    );
}

#[test]
fn sh_c_shred_is_denied() {
    let payload = r#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"sh -c \"shred secret\""}}"#;
    let (code, stdout) = run(payload);
    assert_eq!(code, 0, "hook must always exit 0");
    assert!(
        stdout.contains(r#""permissionDecision":"deny""#),
        "sh -c with shred payload must be denied, got stdout: {stdout}"
    );
}

#[test]
fn find_exec_shell_rm_is_denied() {
    // find . -exec sh -c "rm {}" ;
    let payload = r#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"find . -exec sh -c \"rm {}\" ;"}}"#;
    let (code, stdout) = run(payload);
    assert_eq!(code, 0, "hook must always exit 0");
    assert!(
        stdout.contains(r#""permissionDecision":"deny""#),
        "find -exec sh -c with rm must be denied, got stdout: {stdout}"
    );
}

// ── shell-eval bypass: ALLOW (silent) cases ──────────────────────────────────

#[test]
fn echo_quoting_eval_is_allowed() {
    // "eval" appears in quoted text, not as a command being executed
    let payload = r#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"echo 'eval is cool'"}}"#;
    let (code, stdout) = run(payload);
    assert_eq!(code, 0);
    assert!(
        stdout.trim().is_empty(),
        "echo with eval in quoted text must be silent, got: {stdout}"
    );
}

#[test]
fn sh_help_is_allowed() {
    let payload = r#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"sh --help"}}"#;
    let (code, stdout) = run(payload);
    assert_eq!(code, 0);
    assert!(
        stdout.trim().is_empty(),
        "sh --help must be silent (allowed), got: {stdout}"
    );
}

#[test]
fn find_name_is_allowed() {
    let payload = r#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"find . -name '*.rs'"}}"#;
    let (code, stdout) = run(payload);
    assert_eq!(code, 0);
    assert!(
        stdout.trim().is_empty(),
        "find without -exec must be silent, got: {stdout}"
    );
}

#[test]
fn bash_c_cargo_test_is_allowed() {
    // benign payload passed via -c must not trigger the guard
    let payload = r#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"bash -c \"cargo test\""}}"#;
    let (code, stdout) = run(payload);
    assert_eq!(code, 0);
    assert!(
        stdout.trim().is_empty(),
        "bash -c cargo test must be silent, got: {stdout}"
    );
}

#[test]
fn source_venv_activate_is_allowed() {
    // opaque file path — inline re-analysis finds no destructive command;
    // the no-false-positive bias keeps this allowed.
    let payload = r#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"source .venv/bin/activate"}}"#;
    let (code, stdout) = run(payload);
    assert_eq!(code, 0);
    assert!(
        stdout.trim().is_empty(),
        "source with benign file path must be silent, got: {stdout}"
    );
}

// ── disarm-by-non-write: end-to-end twin of detect.rs's "Round 3" block ─────
//
// The unit tests in `detect.rs` pin the pure verdict; these pin that the
// verdict actually reaches the agent through the hook boundary (a Deny that is
// computed but printed as silence is indistinguishable from an Allow).

/// Feed one Bash command through the real binary and return its stdout.
fn bash_hook(cmd: &str) -> String {
    let escaped = cmd.replace('\\', "\\\\").replace('"', "\\\"");
    let payload = format!(
        r#"{{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{{"command":"{escaped}"}}}}"#
    );
    let (code, stdout) = run(&payload);
    assert_eq!(code, 0, "hook must always exit 0 (cmd: {cmd})");
    stdout
}

#[test]
fn disarming_a_protected_path_without_writing_to_it_is_denied() {
    // One representative of each of the five shapes whose target is a
    // positively recognised protected path: delete / move away / strip the
    // exec bit / restore over it / append via tee.
    let cases = [
        "rm .githooks/pre-commit",
        "mv .githooks/pre-commit /tmp/x",
        "chmod -x .githooks/pre-commit",
        "git checkout -- .claude/settings.json",
        "tee -a .githooks/pre-commit",
    ];
    let failing: Vec<&str> = cases
        .into_iter()
        .filter(|cmd| !bash_hook(cmd).contains(r#""permissionDecision":"deny""#))
        .collect();
    assert!(
        failing.is_empty(),
        "these neutralise a gate without writing to it and must surface a deny: {failing:?}"
    );
}

#[test]
fn recursive_copy_into_a_protected_dir_is_not_silently_allowed() {
    // Sixth shape. The landing set of `cp -r somedir/ .claude/` cannot be
    // enumerated from the command line, so either `deny` or `ask` is a correct
    // answer — silence (Allow) is not.
    let stdout = bash_hook("cp -r evildir/ .claude/");
    assert!(
        stdout.contains(r#""permissionDecision":"deny""#)
            || stdout.contains(r#""permissionDecision":"ask""#),
        "an unmodelled recursive landing into .claude/ must not pass silently, got: {stdout}"
    );
}

#[test]
fn ordinary_non_write_file_operations_stay_silent() {
    // Non-regression: the same five verbs on non-protected targets, plus the
    // read-only `cp` FROM a protected file, must keep passing silently.
    for cmd in [
        "rm src/main.rs",
        "mv a.txt b.txt",
        "chmod +x scripts/build.sh",
        "git checkout somebranch",
        "tee -a /tmp/build.log",
        "cp .githooks/pre-commit /tmp/pre-commit.bak",
    ] {
        let stdout = bash_hook(cmd);
        assert!(
            stdout.trim().is_empty(),
            "ordinary command must stay silent ({cmd}), got: {stdout}"
        );
    }
}
