// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED reproductions for findings filed 2026-10-15 (independent test writer).
//! Every test drives the real `condukt` binary with an isolated `$HOME`
//! (fixture style follows loop_max_iters_flag.rs / drift_b1_loop_cap.rs).

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

fn base(tag: &str) -> PathBuf {
    let b = std::env::temp_dir().join(format!(
        "condukt-findings-20261015-{tag}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&b);
    std::fs::create_dir_all(&b).unwrap();
    b
}

/// A git repo + a `$HOME` whose `.condukt/config.toml` is `config`.
fn fixture(tag: &str, config: &str) -> (PathBuf, PathBuf) {
    let b = base(tag);
    let repo = b.join("repo");
    let home = b.join("home");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(home.join(".condukt")).unwrap();
    assert!(Command::new("git")
        .args(["init", "-q"])
        .current_dir(&repo)
        .status()
        .unwrap()
        .success());
    std::fs::write(home.join(".condukt/config.toml"), config).unwrap();
    (repo, home)
}

fn condukt(repo: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_condukt"))
        .args(args)
        .current_dir(repo)
        .env("HOME", home)
        .env_remove("CONDUKT_AUTONOMOUS")
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn json(o: &Output) -> serde_json::Value {
    serde_json::from_slice(&o.stdout).unwrap_or_else(|e| {
        panic!(
            "output not json: {e}: stdout={} stderr={}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        )
    })
}

// ── Finding 1: Config::load silently drops an unparseable config.toml ─────

fn schedule_with_config(tag: &str, config: &str) -> Output {
    let (repo, home) = fixture(tag, config);
    let decomp = repo.join("decomp.json");
    std::fs::write(
        &decomp,
        serde_json::to_string(&serde_json::json!({
            "goal": "shared glob serialization",
            "tasks": [
                {"id": "touch-shared", "title": "touch-shared",
                 "touched_files": ["shared/x.rs"], "deps": [], "class": "parallel"},
                {"id": "other", "title": "other",
                 "touched_files": ["other/y.rs"], "deps": [], "class": "parallel"}
            ]
        }))
        .unwrap(),
    )
    .unwrap();
    condukt(
        &repo,
        &home,
        &["schedule", "--file", decomp.to_str().unwrap()],
    )
}

fn is_serial(o: &Output, id: &str) -> bool {
    json(o)["serial"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v == id)
}

/// Defect: `Config::load` (config.rs) does
/// `if let Ok(txt) = read_to_string(..) { if let Ok(fc) = toml::from_str(..) {..} }`,
/// so a config.toml that exists but does not parse is silently replaced by
/// defaults. `condukt schedule` then schedules with `shared_globs = []`: a task
/// the user's `shared_globs` would demote to serial runs in a parallel batch,
/// exit 0, no diagnostic.
///
/// Expected: an unparseable config makes `condukt schedule` fail non-zero (the
/// configured shared_globs cannot be determined), or at minimum the task stays
/// serialized. The control half proves the same shared_globs DO serialize the
/// task when the file parses, so the RED is attributable to the swallow.
#[test]
#[ignore = "backlog TBD: RED repro"]
fn unparseable_config_does_not_silently_drop_shared_globs_in_schedule() {
    // Control: well-formed config serializes the shared-glob task.
    let ok = schedule_with_config("cfg-ok", "shared_globs = [\"shared/**\"]\n");
    assert!(ok.status.success(), "control schedule failed: {ok:?}");
    assert!(
        is_serial(&ok, "touch-shared"),
        "control: shared_globs must serialize the task: {}",
        String::from_utf8_lossy(&ok.stdout)
    );

    // Same shared_globs, plus one malformed line elsewhere in the file.
    let broken = schedule_with_config(
        "cfg-broken",
        "shared_globs = [\"shared/**\"]\nmax_parallel = \"four\"\n",
    );
    assert!(
        !broken.status.success() || is_serial(&broken, "touch-shared"),
        "unparseable config.toml was silently replaced by defaults: exit {:?}, \
         task touching a configured shared glob was scheduled in parallel.\n\
         stdout={}\nstderr={}",
        broken.status.code(),
        String::from_utf8_lossy(&broken.stdout),
        String::from_utf8_lossy(&broken.stderr)
    );
}

// ── Finding 2: count_test_failures miscounts realistic runner summaries ───

/// `condukt loop --module server` (deploy=true) whose test command prints
/// `summary` and exits `code`. Returns the loop JSON.
fn loop_with_test_output(tag: &str, summary: &str, code: i32) -> serde_json::Value {
    let config = format!(
        "[test]\ncommand = 'printf \"{summary}\"; exit {code}'\n\n[loop]\ndeploy_command = \"true\"\n"
    );
    let (repo, home) = fixture(tag, &config);
    let o = condukt(
        &repo,
        &home,
        &["loop", "--module", "server", "--prev-failures", "9"],
    );
    json(&o)
}

/// Defect: `count_test_failures` (state.rs) only parses the pytest summary when
/// the line contains BOTH "failed" and "passed". A run where every collected
/// test fails prints `===== 7 failed in 0.12s =====` (no "passed"), which falls
/// through to the "unknown format -> 1" fallback.
///
/// Expected: failure_count == 7.
#[test]
#[ignore = "backlog TBD: RED repro"]
fn pytest_all_failed_summary_counts_every_failure() {
    let j = loop_with_test_output(
        "pytest-allfail",
        "========== 7 failed in 0.12s ==========\\n",
        1,
    );
    assert_eq!(
        j["failure_count"], 7,
        "pytest `7 failed in ..` miscounted: {j}"
    );
}

/// Defect: `count_test_failures` returns the `M failed` of the FIRST
/// `test result:` line, and it also accepts `test result: ok` lines. With
/// `cargo test --no-fail-fast` (or a workspace run) the first binary is
/// typically `test result: ok. 5 passed; 0 failed; ..`, so the count is 0
/// although the run exited non-zero with 7 failures in a later binary. 0 then
/// makes `loop_should_stop` report "all tests pass" — a failing suite is
/// reported as passing (fail-open).
///
/// Expected: failure_count == 7 (sum over the summaries) and the loop does not
/// stop with "all tests pass".
#[test]
#[ignore = "backlog TBD: RED repro"]
fn cargo_multi_binary_summary_is_not_read_as_all_pass() {
    let j = loop_with_test_output(
        "cargo-multibin",
        "test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\\n\
         test result: FAILED. 1 passed; 7 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s\\n",
        101,
    );
    assert_ne!(
        j["stop_reason"], "all tests pass",
        "a run that exited 101 with 7 failures was reported as all-pass: {j}"
    );
    assert_eq!(
        j["failure_count"], 7,
        "cargo multi-binary summary miscounted: {j}"
    );
}

// ── Finding 3: condukt-loop SKILL.md claims one command loops up to N times ─

/// Extract N from the `# ... 最大N回ループ` comment that sits immediately above
/// the verbatim `condukt loop --module server` example in the skill doc.
fn documented_loop_claim() -> Option<(usize, String)> {
    let doc = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("skills/condukt-loop/SKILL.md"),
    )
    .unwrap();
    let lines: Vec<&str> = doc.lines().collect();
    for w in lines.windows(2) {
        let (comment, cmd) = (w[0].trim(), w[1].trim());
        if cmd == "condukt loop --module server" && comment.starts_with('#') {
            let tail = comment.split("最大").nth(1)?;
            let digits: String = tail.chars().take_while(|c| c.is_ascii_digit()).collect();
            if tail[digits.len()..].starts_with("回ループ") {
                return Some((digits.parse().ok()?, cmd.to_string()));
            }
        }
    }
    None
}

/// Defect: crates/condukt/skills/condukt-loop/SKILL.md documents
/// `condukt loop --module server` as "最大10回ループ" (loops at most 10 times).
/// The binary runs exactly ONE cycle per invocation, and `--iteration`
/// defaults to 1, so re-running the documented command verbatim is never
/// capped: N+1 verbatim invocations run N+1 cycles. The bound only exists when
/// the caller passes `--iteration <i>` (and `--prev-failures`).
///
/// This test executes the documented command verbatim N+1 times (N read from
/// the doc) with a test command that counts its own executions, and asserts
/// the documented maximum holds. It passes once the doc stops claiming a cap
/// for that command (no `最大N回ループ` comment) or the binary enforces it.
#[test]
#[ignore = "backlog TBD: RED repro"]
fn documented_server_loop_cap_holds_for_the_documented_command() {
    let Some((n, cmd)) = documented_loop_claim() else {
        return; // doc no longer claims a cap for the verbatim command
    };
    let b = base("doc-loopcap");
    let counter = b.join("cycles");
    let config = format!(
        "[test]\ncommand = 'echo x >> \"{}\"; exit 1'\n\n[loop]\ndeploy_command = \"true\"\n",
        counter.display()
    );
    let (repo, home) = fixture("doc-loopcap", &config);
    let args: Vec<&str> = cmd.split_whitespace().skip(1).collect();
    for _ in 0..=n {
        let _ = condukt(&repo, &home, &args);
    }
    let cycles = std::fs::read_to_string(&counter)
        .map(|s| s.lines().count())
        .unwrap_or(0);
    assert!(
        cycles <= n,
        "SKILL.md documents `{cmd}` as looping at most {n} times, but {} verbatim \
         invocations ran {cycles} test cycles (no cap without --iteration)",
        n + 1
    );
}
