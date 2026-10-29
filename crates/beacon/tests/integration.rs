//! End-to-end tests: drive the real built `beacon` binary and assert on exit
//! code + stdout. `beacon notify` is the Stop/Notification hook — it only ever
//! notifies and must always exit 0 (a missing channel or empty stdin costs
//! nothing). The rest is a subcommand CLI.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// Isolated temp HOME so `~/.beacon` config/state is never read or written.
fn temp_home(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "beacon-it-{}-{}-{}",
        tag,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Run `beacon <args>` with `payload` on stdin under an isolated HOME.
/// `cwd` is set to the temp HOME too so any project-local `beacon.toml` lookup
/// resolves to an empty dir.
fn run(args: &[&str], payload: &str, home: &PathBuf) -> (i32, String) {
    let bin = env!("CARGO_BIN_EXE_beacon");
    let mut child = harness_core::git_env::scrub(&mut Command::new(bin))
        .args(args)
        .env("HOME", home)
        .current_dir(home)
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

#[test]
fn version_flag_exits_zero() {
    let home = temp_home("version");
    let (code, stdout) = run(&["--version"], "", &home);
    assert_eq!(code, 0, "--version must exit 0");
    assert!(
        stdout.contains("beacon"),
        "version output should name the binary, got: {stdout}"
    );
}

#[test]
fn status_prints_resolved_config() {
    // Read-only: prints the resolved config (defaults under isolated HOME).
    let home = temp_home("status");
    let (code, stdout) = run(&["status"], "", &home);
    assert_eq!(code, 0, "status must exit 0");
    assert!(
        stdout.contains("enabled:") && stdout.contains("state_dir:"),
        "status should list resolved config fields, got: {stdout}"
    );
}

#[test]
fn notify_with_valid_stop_payload_exits_zero() {
    // The notify hook never blocks; with no channels configured it returns
    // silently, but the load-bearing invariant is exit 0.
    let home = temp_home("notify-valid");
    let payload = r#"{"hook_event_name":"Stop","cwd":"/tmp","transcript_path":""}"#;
    let (code, _stdout) = run(&["notify"], payload, &home);
    assert_eq!(code, 0, "notify hook must always exit 0");
}

#[test]
fn notify_with_malformed_stdin_exits_zero() {
    // Fail-soft invariant: unparseable stdin → early return, never breaks a turn.
    let home = temp_home("notify-bad");
    let (code, stdout) = run(&["notify"], "not json", &home);
    assert_eq!(code, 0, "malformed stdin must still exit 0");
    assert!(stdout.trim().is_empty(), "got: {stdout}");
}
