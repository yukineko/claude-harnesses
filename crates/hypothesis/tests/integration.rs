//! End-to-end tests: run the real built `hypothesis` binary. Most commands are
//! subcommand-CLI; `session-start` is a SessionStart hook that must always exit
//! 0 (the harness invariant: never break a turn). Everything runs against an
//! isolated temp HOME so the real `~/.hypothesis` store is never touched.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// A unique throwaway directory used as HOME for one test.
fn temp_home(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "hypothesis-{tag}-{}-{:?}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Run the binary with `args` in an isolated HOME. Returns (exit_code, stdout, home).
fn run(args: &[&str], tag: &str) -> (i32, String, PathBuf) {
    let bin = env!("CARGO_BIN_EXE_hypothesis");
    let home = temp_home(tag);
    let out = Command::new(bin)
        .args(args)
        .env("HOME", &home)
        .output()
        .expect("binary runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        home,
    )
}

/// Feed `payload` on stdin to `args` in an isolated HOME. Returns (exit_code, stdout).
fn run_with_stdin(args: &[&str], payload: &str, tag: &str) -> (i32, String) {
    let bin = env!("CARGO_BIN_EXE_hypothesis");
    let home = temp_home(tag);
    let mut child = Command::new(bin)
        .args(args)
        .env("HOME", &home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("binary spawns");
    if let Some(mut child_stdin) = child.stdin.take() {
        let _ = child_stdin.write_all(payload.as_bytes());
    }
    let out = child.wait_with_output().expect("binary runs");
    std::fs::remove_dir_all(&home).ok();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

#[test]
fn help_describes_lifecycle() {
    let (code, stdout, home) = run(&["--help"], "help");
    std::fs::remove_dir_all(&home).ok();
    assert_eq!(code, 0, "--help must exit 0");
    assert!(
        stdout.contains("PDO hypothesis lifecycle management"),
        "expected the about string, got: {stdout}"
    );
}

#[test]
fn add_then_list_round_trips() {
    // `add` prints the new id; reusing the SAME home, `list` echoes it back.
    let bin = env!("CARGO_BIN_EXE_hypothesis");
    let home = temp_home("roundtrip");

    let add = Command::new(bin)
        .args(["add", "memory pressure crashes Colab"])
        .env("HOME", &home)
        .output()
        .expect("add runs");
    assert_eq!(add.status.code().unwrap_or(-1), 0, "add must exit 0");
    let id = String::from_utf8_lossy(&add.stdout).trim().to_string();
    assert!(!id.is_empty(), "add must print a non-empty id");

    let list = Command::new(bin)
        .args(["list"])
        .env("HOME", &home)
        .output()
        .expect("list runs");
    let list_out = String::from_utf8_lossy(&list.stdout).into_owned();
    std::fs::remove_dir_all(&home).ok();
    assert_eq!(list.status.code().unwrap_or(-1), 0, "list must exit 0");
    assert!(
        list_out.contains(&id) && list_out.contains("memory pressure crashes Colab"),
        "list must echo the added hypothesis, got: {list_out}"
    );
}

#[test]
fn list_json_uses_hyphenated_status_vocabulary() {
    // overwatch's bucket_hypotheses() (crates/overwatch/src/aggregate.rs) parses
    // `hypothesis list --json` and matches status strings against the hyphenated
    // vocabulary "open"|"awaiting-measurement"|"validated"|"rejected" — NOT
    // serde's default snake_case rendering of `Status` (which would emit
    // "awaiting_measurement" with an underscore). This test pins that contract
    // end-to-end through the real binary so a future change to `Status`'s serde
    // derive (or to `list --json`'s field mapping) can't silently regress it.
    let bin = env!("CARGO_BIN_EXE_hypothesis");
    let home = temp_home("list-json");

    let add = Command::new(bin)
        .args(["add", "list --json emits hyphenated status"])
        .env("HOME", &home)
        .output()
        .expect("add runs");
    let id = String::from_utf8_lossy(&add.stdout).trim().to_string();
    assert!(!id.is_empty(), "add must print a non-empty id");

    let list = Command::new(bin)
        .args(["list", "--json"])
        .env("HOME", &home)
        .output()
        .expect("list --json runs");
    let list_out = String::from_utf8_lossy(&list.stdout).into_owned();
    assert_eq!(
        list.status.code().unwrap_or(-1),
        0,
        "list --json must exit 0"
    );

    let items: serde_json::Value =
        serde_json::from_str(&list_out).expect("list --json must emit valid JSON");
    let arr = items
        .as_array()
        .expect("list --json must emit a JSON array");
    assert_eq!(arr.len(), 1, "expected exactly the one added hypothesis");
    assert_eq!(
        arr[0]["status"], "open",
        "a freshly-added hypothesis must report status \"open\" (hyphen-free case), got: {list_out}"
    );
    assert_eq!(
        arr[0]["id"], id,
        "the JSON item's id must match the added hypothesis"
    );

    // AwaitingMeasurement is the vocabulary word bucket_hypotheses() actually
    // depends on being hyphenated ("awaiting-measurement"), since serde's
    // derive on `Status` would otherwise render it "awaiting_measurement".
    let awaiting = Command::new(bin)
        .args(["await-measurement", &id])
        .env("HOME", &home)
        .output()
        .expect("await-measurement runs");
    assert_eq!(
        awaiting.status.code().unwrap_or(-1),
        0,
        "await-measurement must exit 0"
    );

    let list2 = Command::new(bin)
        .args(["list", "--json"])
        .env("HOME", &home)
        .output()
        .expect("list --json runs (2nd)");
    let list2_out = String::from_utf8_lossy(&list2.stdout).into_owned();
    std::fs::remove_dir_all(&home).ok();
    let items2: serde_json::Value =
        serde_json::from_str(&list2_out).expect("list --json must emit valid JSON (2nd)");
    assert_eq!(
        items2[0]["status"], "awaiting-measurement",
        "expected the hyphenated form \"awaiting-measurement\" (matching overwatch's \
         bucket_hypotheses vocabulary), got: {list2_out}"
    );
}

#[test]
fn stats_reports_shipped_vs_measured_counts_as_one_json_object() {
    let bin = env!("CARGO_BIN_EXE_hypothesis");
    let home = temp_home("stats");

    let add = Command::new(bin)
        .args(["add", "stats end-to-end fixture"])
        .env("HOME", &home)
        .output()
        .expect("add runs");
    let id = String::from_utf8_lossy(&add.stdout).trim().to_string();

    let stats0 = Command::new(bin)
        .args(["stats"])
        .env("HOME", &home)
        .output()
        .expect("stats runs");
    let stats0_out = String::from_utf8_lossy(&stats0.stdout).into_owned();
    assert_eq!(stats0.status.code().unwrap_or(-1), 0, "stats must exit 0");
    let v0: serde_json::Value =
        serde_json::from_str(&stats0_out).expect("stats must emit valid JSON");
    assert_eq!(
        v0["shipped"], 0,
        "nothing has shipped yet, got: {stats0_out}"
    );
    assert_eq!(v0["awaiting"], 0);
    assert!(v0["avg_measurement_delay_days"].is_null());

    let awaiting = Command::new(bin)
        .args(["await-measurement", &id])
        .env("HOME", &home)
        .output()
        .expect("await-measurement runs");
    assert_eq!(awaiting.status.code().unwrap_or(-1), 0);

    let stats1 = Command::new(bin)
        .args(["stats"])
        .env("HOME", &home)
        .output()
        .expect("stats runs (2nd)");
    let stats1_out = String::from_utf8_lossy(&stats1.stdout).into_owned();
    std::fs::remove_dir_all(&home).ok();
    let v1: serde_json::Value =
        serde_json::from_str(&stats1_out).expect("stats must emit valid JSON (2nd)");
    assert_eq!(
        v1["shipped"], 1,
        "one hypothesis has shipped (awaiting-measurement), got: {stats1_out}"
    );
    assert_eq!(
        v1["awaiting"], 1,
        "it is currently outstanding, got: {stats1_out}"
    );
    assert_eq!(v1["validated"], 0);
}

#[test]
fn session_start_hook_survives_empty_stdin() {
    // SessionStart runs under run_hook: malformed/empty stdin must never break a
    // turn, so the exit code is always 0.
    let (code, _stdout) = run_with_stdin(&["session-start"], "", "hook-empty");
    assert_eq!(code, 0, "session-start hook must exit 0 on empty stdin");
}

#[test]
fn session_start_hook_survives_garbage_stdin() {
    let (code, _stdout) = run_with_stdin(&["session-start"], "not json", "hook-garbage");
    assert_eq!(code, 0, "session-start hook must exit 0 on garbage stdin");
}
