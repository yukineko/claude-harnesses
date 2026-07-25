//! End-to-end tests for the `fugu-router` binary.
//!
//! `Prompt` is a UserPromptSubmit hook (runs under `run_hook` → must ALWAYS exit
//! 0, never breaking the turn). The other subcommands are an ordinary CLI. Tests
//! that touch the store run with an isolated `HOME` so the real
//! `~/.fugu-router/episodes.jsonl` is never read or written (the config resolves
//! the store path via `dirs::home_dir()`, which honors `$HOME` on Linux).

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// A unique, isolated temp HOME for one test (so store paths never hit real $HOME).
fn temp_home(tag: &str) -> PathBuf {
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("fugu-router-test-{tag}-{pid}-{nanos}"));
    std::fs::create_dir_all(&dir).expect("create temp home");
    dir
}

/// Run the binary with `args` and `payload` on stdin, under an isolated HOME.
/// Returns (exit_code, stdout).
fn run_in(home: &PathBuf, args: &[&str], payload: &str) -> (i32, String) {
    let bin = env!("CARGO_BIN_EXE_fugu-router");
    let mut child = Command::new(bin)
        .args(args)
        .env("HOME", home)
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
fn help_lists_route_subcommand() {
    let home = temp_home("help");
    let (code, stdout) = run_in(&home, &["--help"], "");
    assert_eq!(code, 0, "--help must exit 0");
    assert!(
        stdout.contains("route"),
        "expected the route subcommand in --help, got: {stdout}"
    );
}

#[test]
fn prompt_hook_exits_zero_on_valid_payload() {
    // A real UserPromptSubmit payload. With an empty store the hook emits no
    // additionalContext, but the load-bearing invariant is exit 0.
    let home = temp_home("prompt-valid");
    let payload =
        r#"{"hook_event_name":"UserPromptSubmit","prompt":"add a login feature to the api"}"#;
    let (code, _stdout) = run_in(&home, &["prompt"], payload);
    assert_eq!(code, 0, "Prompt hook must always exit 0");
}

#[test]
fn prompt_hook_exits_zero_on_malformed_stdin() {
    // Fail-soft invariant: garbage stdin must never break the turn.
    let home = temp_home("prompt-bad");
    let (code, stdout) = run_in(&home, &["prompt"], "not json");
    assert_eq!(code, 0, "Prompt hook must exit 0 even on malformed stdin");
    assert!(
        stdout.trim().is_empty(),
        "no actionable prompt → no output, got: {stdout}"
    );
}

#[test]
fn stats_on_empty_store_reports_zero_episodes() {
    // Read-only subcommand against a fresh (nonexistent) store in temp HOME.
    let home = temp_home("stats");
    let (code, stdout) = run_in(&home, &["stats"], "");
    assert_eq!(code, 0, "stats must exit 0 on an empty store");
    assert!(
        stdout.contains("episodes: 0"),
        "expected `episodes: 0`, got: {stdout}"
    );
}

/// Record one episode with `--duration <secs>` (or none). Panics on non-zero
/// exit so a helper call can't silently swallow a rejection.
fn record_ep(home: &PathBuf, title: &str, class: &str, duration: Option<&str>) {
    let mut args: Vec<&str> = vec![
        "record", "--title", title, "--model", "sonnet", "--status", "verified", "--class", class,
    ];
    if let Some(d) = duration {
        args.push("--duration");
        args.push(d);
    }
    let (code, _out) = run_in(home, &args, "");
    assert_eq!(code, 0, "record of `{title}` must succeed");
}

/// `record --duration` MUST reject a non-positive value (0 = "unmeasured" on
/// disk now, so it is unrepresentable) with a non-zero exit; omitting the flag
/// records the episode as unmeasured and succeeds.
#[test]
fn record_rejects_nonpositive_duration_but_accepts_omission() {
    let home = temp_home("dur-reject");

    // --duration 0 → rejected (our explicit validation, not clap).
    let (code0, _o) = run_in(
        &home,
        &[
            "record",
            "--title",
            "z",
            "--model",
            "sonnet",
            "--status",
            "verified",
            "--duration",
            "0",
        ],
        "",
    );
    assert_ne!(
        code0, 0,
        "--duration 0 must be rejected with a non-zero exit"
    );

    // --duration=-2.5 → rejected via our validation path (=<value> so clap
    // doesn't mistake the leading '-' for a flag).
    let (codeneg, _o) = run_in(
        &home,
        &[
            "record",
            "--title",
            "z",
            "--model",
            "sonnet",
            "--status",
            "verified",
            "--duration=-2.5",
        ],
        "",
    );
    assert_ne!(codeneg, 0, "a negative --duration must be rejected");

    // Omitting --duration → recorded as unmeasured, exit 0.
    let (codeok, _o) = run_in(
        &home,
        &[
            "record", "--title", "z", "--model", "sonnet", "--status", "verified",
        ],
        "",
    );
    assert_eq!(codeok, 0, "omitting --duration must record as unmeasured");

    // A real positive measurement is accepted.
    let (codepos, _o) = run_in(
        &home,
        &[
            "record",
            "--title",
            "z",
            "--model",
            "sonnet",
            "--status",
            "verified",
            "--duration",
            "3.5",
        ],
        "",
    );
    assert_eq!(codepos, 0, "a positive --duration must be accepted");
}

/// Both `stats` and `duration` (alias of duration-outliers) surface duration
/// coverage as measured/total, matching the actual counts.
#[test]
fn stats_and_duration_surface_coverage_counts() {
    let home = temp_home("dur-coverage");
    record_ep(&home, "measured", "parallel", Some("5"));
    record_ep(&home, "unmeasured", "parallel", None);

    let (sc, stats_out) = run_in(&home, &["stats", "--json"], "");
    assert_eq!(sc, 0);
    let stats: serde_json::Value = serde_json::from_str(&stats_out).expect("stats json");
    assert_eq!(stats["duration_coverage"]["recorded"], 1);
    assert_eq!(stats["duration_coverage"]["total"], 2);

    // `duration` is the alias for duration-outliers.
    let (dc, dur_out) = run_in(&home, &["duration", "--json"], "");
    assert_eq!(dc, 0);
    let dur: serde_json::Value = serde_json::from_str(&dur_out).expect("duration json");
    assert_eq!(dur["duration_coverage"]["recorded"], 1);
    assert_eq!(dur["duration_coverage"]["total"], 2);
}

/// An unmeasured episode must NOT enter the average denominator: with one
/// measured (10s) and one unmeasured fork episode, the fork bucket's average is
/// 10.0 over a single sample, not 5.0 over two.
#[test]
fn unmeasured_episode_excluded_from_duration_average() {
    // Use the delegation-stats path, which averages duration over measured
    // episodes only.
    let home = temp_home("dur-avg");
    let record_deleg = |title: &str, dur: Option<&str>| {
        let mut args: Vec<&str> = vec![
            "record",
            "--title",
            title,
            "--model",
            "sonnet",
            "--status",
            "verified",
            "--class",
            "flow-delegation",
            "--delegation",
            "fork",
        ];
        if let Some(d) = dur {
            args.push("--duration");
            args.push(d);
        }
        let (code, _o) = run_in(&home, &args, "");
        assert_eq!(code, 0, "record `{title}` must succeed");
    };
    record_deleg("f-measured", Some("10"));
    record_deleg("f-unmeasured", None);

    let (code, out) = run_in(&home, &["delegation-stats", "--json"], "");
    assert_eq!(code, 0);
    let v: serde_json::Value = serde_json::from_str(&out).expect("delegation-stats json");
    let fork = &v["delegation"]["fork"];
    assert_eq!(fork["count"], 2, "both episodes counted in the bucket");
    assert_eq!(
        fork["duration_samples"], 1,
        "only the measured episode has a duration sample"
    );
    assert_eq!(
        fork["avg_duration_secs"].as_f64().unwrap(),
        10.0,
        "the unmeasured episode must not be averaged in as 0 (would give 5.0)"
    );
}
