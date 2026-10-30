//! End-to-end tests: spawn the real built `compass` binary and assert on its
//! stdout and exit code. compass is a clap subcommand CLI whose `breadcrumb`
//! subcommand is also a Stop hook (must never break a turn → always exit 0).

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

/// Run `compass <args...>` with `cwd` as the working dir and `stdin` piped in.
/// Returns (exit_code, stdout). A unique temp cwd keeps every test off the real
/// repo so `Config::load`/charter lookups are deterministic.
fn run_in(cwd: &Path, args: &[&str], stdin: &str) -> (i32, String) {
    let bin = env!("CARGO_BIN_EXE_compass");
    let mut child = harness_core::git_env::scrub(&mut Command::new(bin))
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("binary spawns");
    if let Some(mut child_stdin) = child.stdin.take() {
        let _ = child_stdin.write_all(stdin.as_bytes());
    }
    let out = child.wait_with_output().expect("binary runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

/// A unique empty temp dir to act as an isolated project root.
fn temp_root(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("compass-it-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp root");
    dir
}

#[test]
fn help_lists_real_subcommands() {
    let cwd = temp_root("help");
    let (code, stdout) = run_in(&cwd, &["--help"], "");
    assert_eq!(code, 0);
    assert!(stdout.contains("Usage: compass"), "got: {stdout}");
    // Real subcommands from the clap dispatch.
    assert!(stdout.contains("nudge"), "got: {stdout}");
    assert!(stdout.contains("breadcrumb"), "got: {stdout}");
}

#[test]
fn pivot_check_emits_persevere_with_no_outcomes() {
    // No .compass/outcomes.json in a fresh temp root → deterministic persevere.
    let cwd = temp_root("pivot");
    let (code, stdout) = run_in(&cwd, &["pivot-check"], "");
    assert_eq!(code, 0, "pivot-check always exits 0");
    assert!(
        stdout.contains(r#""recommendation":"persevere""#),
        "got: {stdout}"
    );
    assert!(stdout.contains(r#""streak":0"#), "got: {stdout}");
}

#[test]
fn nudge_json_reports_absent_charter() {
    // No charter in a fresh temp root → C1 says "not fresh", names the file.
    let cwd = temp_root("nudge");
    let (code, stdout) = run_in(&cwd, &["nudge", "--json"], "");
    assert_eq!(code, 0, "nudge always exits 0 (never breaks SessionStart)");
    assert!(stdout.contains(r#""fresh":false"#), "got: {stdout}");
    assert!(stdout.contains(".compass/charter.md"), "got: {stdout}");
}

#[test]
fn breadcrumb_empty_stdin_is_silent_and_exits_0() {
    // Stop-hook invariant: malformed/empty payload must never break the turn.
    let cwd = temp_root("crumb-empty");
    let (code, stdout) = run_in(&cwd, &["breadcrumb"], "");
    assert_eq!(code, 0, "breadcrumb must always exit 0");
    assert!(stdout.trim().is_empty(), "got: {stdout}");
}

#[test]
fn breadcrumb_malformed_stdin_exits_0() {
    let cwd = temp_root("crumb-bad");
    let (code, _stdout) = run_in(&cwd, &["breadcrumb"], "not json");
    assert_eq!(code, 0, "breadcrumb fail-soft on garbage input");
}
