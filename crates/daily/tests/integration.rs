//! End-to-end tests for `daily`: a clap CLI whose `session-start` subcommand is a
//! SessionStart hook (must NEVER break a turn → always exit 0). Every test pins
//! HOME to an isolated temp dir so the hook's `~/.daily/state` writes can't touch
//! the real home, and so output never depends on the real environment.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

/// Run `daily <args...>` with `home` as $HOME and `stdin` piped in.
/// Returns (exit_code, stdout).
fn run_with_home(home: &Path, args: &[&str], stdin: &str) -> (i32, String) {
    let bin = env!("CARGO_BIN_EXE_daily");
    let mut child = Command::new(bin)
        .args(args)
        .env("HOME", home)
        .current_dir(home)
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

/// A unique empty temp dir to serve as an isolated $HOME.
fn temp_home(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("daily-it-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp home");
    dir
}

#[test]
fn help_lists_real_subcommands() {
    let home = temp_home("help");
    let (code, stdout) = run_with_home(&home, &["--help"], "");
    assert_eq!(code, 0);
    assert!(stdout.contains("Usage: daily"), "got: {stdout}");
    assert!(stdout.contains("session-start"), "got: {stdout}");
    assert!(stdout.contains("install"), "got: {stdout}");
}

#[test]
fn session_start_valid_payload_exits_0() {
    // A valid SessionStart payload: whether or not cargo-deny exists on PATH, the
    // run_hook wrapper guarantees exit 0 (the hook must never break a turn).
    let home = temp_home("valid");
    let payload = format!(
        r#"{{"hook_event_name":"SessionStart","cwd":"{}"}}"#,
        home.display()
    );
    let (code, _stdout) = run_with_home(&home, &["session-start"], &payload);
    assert_eq!(code, 0, "SessionStart hook must always exit 0");
}

#[test]
fn session_start_empty_stdin_exits_0() {
    // Empty stdin: HookInput::parse falls back to default; still must exit 0.
    let home = temp_home("empty");
    let (code, _stdout) = run_with_home(&home, &["session-start"], "");
    assert_eq!(code, 0, "empty stdin must not break the turn");
}

#[test]
fn session_start_malformed_stdin_exits_0() {
    // Garbage payload: fail-soft, never breaks the turn.
    let home = temp_home("bad");
    let (code, _stdout) = run_with_home(&home, &["session-start"], "not json");
    assert_eq!(code, 0, "malformed stdin must not break the turn");
}

#[test]
fn add_then_list_shows_registered_task() {
    let home = temp_home("add");
    // No config yet → list shows the built-in default security task.
    let (code, out) = run_with_home(&home, &["list"], "");
    assert_eq!(code, 0);
    assert!(
        out.contains("security"),
        "default should be security: {out}"
    );

    // Register a task; it must land in ~/.daily/config.toml.
    let (code, out) = run_with_home(
        &home,
        &["add", "--name", "notes", "--command", "echo sync"],
        "",
    );
    assert_eq!(code, 0);
    assert!(out.contains("registered task 'notes'"), "got: {out}");

    // list now shows the registered task (and no longer the default).
    let (code, out) = run_with_home(&home, &["list"], "");
    assert_eq!(code, 0);
    assert!(out.contains("notes"), "got: {out}");
    assert!(out.contains("echo sync"), "got: {out}");
    assert!(out.contains("pending today"), "not run yet: {out}");
}

#[test]
fn add_rejects_duplicate_name() {
    let home = temp_home("dup");
    let (code, _) = run_with_home(&home, &["add", "--name", "x", "--command", "true"], "");
    assert_eq!(code, 0);
    // Second add with the same name must fail (non-zero) — one state key per name.
    let (code, _) = run_with_home(&home, &["add", "--name", "x", "--command", "false"], "");
    assert_ne!(code, 0, "duplicate task name must be rejected");
}

/// Write a `~/.daily/config.toml` under `home` that disables the driver-skip
/// gate, so session-start tests run tasks deterministically regardless of any
/// ambient backlog lock on the test host.
fn write_no_driver_skip_config(home: &Path) {
    let dir = home.join(".daily");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("config.toml"), "skip_when_driver_active = false\n").unwrap();
}

#[test]
fn session_start_runs_registered_task_once_per_day() {
    let home = temp_home("once");
    write_no_driver_skip_config(&home);
    // Register a task that always succeeds and creates a marker file.
    let marker = home.join("ran.marker");
    let cmd = format!("touch {}", marker.display());
    let (code, _) = run_with_home(&home, &["add", "--name", "toucher", "--command", &cmd], "");
    assert_eq!(code, 0);

    let payload = format!(
        r#"{{"hook_event_name":"SessionStart","cwd":"{}"}}"#,
        home.display()
    );
    let (code, out) = run_with_home(&home, &["session-start"], &payload);
    assert_eq!(code, 0);
    assert!(marker.exists(), "task should have run");
    assert!(out.contains("toucher (ok)"), "summary injected: {out}");

    // Second session same day: guard skips it → no summary, marker unchanged.
    std::fs::remove_file(&marker).ok();
    let (code, out) = run_with_home(&home, &["session-start"], &payload);
    assert_eq!(code, 0);
    assert!(!marker.exists(), "must not run twice the same day");
    assert!(out.trim().is_empty(), "nothing ran → silent: {out}");
}

#[test]
fn session_start_writes_report_and_report_cmd_shows_it() {
    let home = temp_home("report");
    write_no_driver_skip_config(&home);
    // One task that succeeds, one that fails — the report must capture both,
    // and a failing task must still count as "ran" (not retried).
    run_with_home(&home, &["add", "--name", "good", "--command", "true"], "");
    run_with_home(&home, &["add", "--name", "bad", "--command", "exit 3"], "");

    let payload = format!(
        r#"{{"hook_event_name":"SessionStart","cwd":"{}"}}"#,
        home.display()
    );
    let (code, _) = run_with_home(&home, &["session-start"], &payload);
    assert_eq!(code, 0);

    // The JSONL report file exists.
    assert!(
        home.join(".daily/reports.jsonl").exists(),
        "report file should be written"
    );

    // `daily report` (today) shows both tasks with their status.
    let (code, out) = run_with_home(&home, &["report"], "");
    assert_eq!(code, 0);
    assert!(out.contains("good"), "report shows ok task: {out}");
    assert!(out.contains("bad"), "report shows failed task: {out}");
    assert!(
        out.contains("fail exit 3"),
        "failure detail recorded: {out}"
    );

    // The failed task is marked done for the day (won't retry): a second
    // session runs nothing.
    let (code, out) = run_with_home(&home, &["session-start"], &payload);
    assert_eq!(code, 0);
    assert!(
        out.trim().is_empty(),
        "failed task must not retry today: {out}"
    );
}

#[test]
fn report_on_empty_history_is_graceful() {
    let home = temp_home("emptyreport");
    let (code, out) = run_with_home(&home, &["report"], "");
    assert_eq!(code, 0);
    assert!(out.contains("記録なし"), "got: {out}");
}
