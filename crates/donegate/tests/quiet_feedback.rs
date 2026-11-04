// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! End-to-end: a failing required check's Stop feedback must be QUIET, with the
//! full raw output kept as a session record.
//!
//! ## The ruling this pins
//!
//! User ruling (verbatim): 「stderrをsession記録としてもっておくのはいい、でも判断を
//! うながすのでなければ表示するのはノイズにすぎない。ログがおおいと大事な情報が流れる」.
//! Keep the full output as a record; DISPLAY only what prompts a decision.
//!
//! Observed defect: a failing `test-changed` check had its captured tail
//! (`output_tail_lines`, default 40) pasted verbatim into the block reason —
//! `---- name stdout ----` / `--- stdout ---` / `--- stderr ---` sections, passing
//! progress lines, backtrace notes — and the model-facing path never named any
//! file holding the full output, although `runner::run` already streams the whole
//! output to `<state_dir>/tmp/<check>.log`.
//!
//! ## What is asserted (properties, not layout)
//!
//! (a) the display keeps: check name, exit code, each failing test name with its
//!     panic line and message line, the summary line, and the ABSOLUTE path of a
//!     file holding the full raw output;
//! (b) the display lacks: raw `stdout`/`stderr` sections, passing-test progress
//!     lines; and the total reason stays under a small line bound;
//! (c) that file exists and equals the raw output byte-for-byte; a planted marker
//!     from the middle of a 2000-line output is IN the file and NOT in the display;
//! (d) fail-closed: if the record cannot be saved the stop is STILL blocked, the
//!     display SAYS the record could not be saved, and it shows MORE output, never
//!     less with no record (CLAUDE.md §3 / §4);
//! (e) a non-cargo check with no recognisable test lines still shows its last few
//!     lines and the record path — never empty.
//!
//! Anti-vacuity controls: `control_*` — a gate that never blocks, or a display that
//! never contains the check, would satisfy several of the above vacuously.
//!
//! Written by a DISINTERESTED author (CLAUDE.md §2(a)); the implementer of the
//! fix did not write these.
//!
//! ## Isolation
//!
//! Each test builds its own `$HOME` and passes it via `.env("HOME", …)` (idiom of
//! `tests/session_scoped_skip.rs`); `state_dir` is pinned inside that `$HOME`.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Upper bound on the TOTAL lines of the block reason (preamble + footer + one
/// failing check) when the check is quiet. The fixed preamble/footer is ~10 lines;
/// the check's own share must therefore stay around 25 lines or fewer.
const QUIET_REASON_MAX_LINES: usize = 40;

const MIDDLE_MARKER: &str = "MIDDLE_MARKER_7f3a91c2d4";

fn scratch(tag: &str) -> PathBuf {
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("donegate-quiet-{tag}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch");
    dir
}

/// cargo-test-shaped raw output of ~2000 lines, a unique marker in the middle,
/// two failing tests, and noisy raw sections inside the tail window.
fn cargo_like_raw(passing: usize) -> String {
    let mut s = String::new();
    s.push_str(&format!("running {} tests\n", passing + 2));
    for i in 0..passing / 2 {
        s.push_str(&format!("test suite::case_{i} ... ok\n"));
    }
    s.push_str(&format!("test suite::{MIDDLE_MARKER} ... ok\n"));
    for i in passing / 2..passing {
        s.push_str(&format!("test suite::case_{i} ... ok\n"));
    }
    s.push_str("test suite::alpha_fails ... FAILED\n");
    s.push_str("test suite::beta_fails ... FAILED\n");
    for i in 0..4 {
        s.push_str(&format!("test suite::late_ok_{i} ... ok\n"));
    }
    s.push_str("\nfailures:\n\n");
    s.push_str("---- suite::alpha_fails stdout ----\n");
    s.push_str("--- stdout ---\n");
    s.push_str("progress: step 1 of 9 done\nprogress: step 2 of 9 done\n");
    s.push_str("--- stderr ---\n");
    s.push_str("thread 'suite::alpha_fails' panicked at crates/x/src/lib.rs:983:5:\n");
    s.push_str("alpha assertion message: left 1 right 2\n");
    s.push_str("note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n");
    s.push_str("\n---- suite::beta_fails stdout ----\n");
    s.push_str("--- stdout ---\nprogress: step 3 of 9 done\n--- stderr ---\n");
    s.push_str("thread 'suite::beta_fails' panicked at crates/x/src/lib.rs:41:9:\n");
    s.push_str("beta boom message: unexpected None\n");
    s.push_str("\nfailures:\n    suite::alpha_fails\n    suite::beta_fails\n\n");
    s.push_str(&format!(
        "test result: FAILED. {passing} passed; 2 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.42s\n"
    ));
    s
}

/// Non-cargo raw output: no `test … ok/FAILED` lines, no panic lines.
fn plain_raw(lines: usize) -> String {
    let mut s = String::new();
    for i in 0..lines {
        s.push_str(&format!("building step {i} of {lines}\n"));
    }
    s.push_str("linker: undefined reference to `frobnicate`\n");
    s.push_str("LAST_LINE_UNIQUE_55e1: build FAILED\n");
    s
}

struct Fixture {
    home: PathBuf,
    root: PathBuf,
    raw: String,
}

/// A trusted project with one required check `name` that prints `raw` (read from
/// a file in the project) and exits `exit`. `state_dir` is `<home>/sd` unless the
/// caller overrides it via `state_dir_toml`.
fn fixture(tag: &str, name: &str, raw: String, exit: i32, state_dir_toml: Option<&str>) -> Fixture {
    let home = scratch(tag);
    let root = home.join("project");
    std::fs::create_dir_all(&root).expect("create project root");
    std::fs::write(root.join("raw.txt"), &raw).expect("write raw.txt");
    let sd = match state_dir_toml {
        Some(s) => s.to_string(),
        None => home.join("sd").display().to_string(),
    };
    std::fs::write(
        root.join("donegate.toml"),
        format!(
            "max_attempts = 50\nstate_dir = \"{sd}\"\n\n[[check]]\nname = \"{name}\"\ncmd = \"cat raw.txt; exit {exit}\"\n"
        ),
    )
    .expect("write donegate.toml");
    // Must be trusted, else the config is ignored and "blocked" can never happen.
    let canon = std::fs::canonicalize(&root).expect("canonicalize project root");
    std::fs::create_dir_all(home.join(".harness")).expect("create ~/.harness");
    std::fs::write(
        home.join(".harness/trust.toml"),
        format!("trusted = [\"{}\"]\n", canon.display()),
    )
    .expect("write trust.toml");
    Fixture { home, root, raw }
}

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

fn stop(f: &Fixture, session: &str) -> Run {
    let payload = serde_json::json!({
        "session_id": session,
        "cwd": f.root.to_string_lossy(),
        "hook_event_name": "Stop",
    })
    .to_string();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_donegate"));
    harness_core::git_env::scrub(&mut cmd);
    cmd.arg("gate")
        .current_dir(&f.root)
        .env("HOME", &f.home)
        .env("CLAUDE_CODE_SESSION_ID", session)
        .env_remove("DONEGATE_DISABLE")
        .env_remove("HARNESS_TRUST_ALL");
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("donegate spawns");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(payload.as_bytes())
        .expect("write stdin");
    let out = child.wait_with_output().expect("donegate runs");
    Run {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

/// The block reason as the model sees it: the `reason` of the `decision:block`
/// JSON on stdout. `None` if the stop was not blocked (a missing/unparseable
/// decision is NOT treated as a block).
fn block_reason(r: &Run) -> Option<String> {
    for line in r.stdout.lines() {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
            if v.get("decision").and_then(|d| d.as_str()) == Some("block") {
                return v.get("reason").and_then(|x| x.as_str()).map(String::from);
            }
        }
    }
    None
}

fn reason_of(r: &Run) -> String {
    block_reason(r).unwrap_or_else(|| {
        panic!(
            "the stop was not blocked (code {}).\n--- stdout ---\n{}\n--- stderr ---\n{}",
            r.code, r.stdout, r.stderr
        )
    })
}

/// Absolute-path-looking tokens in `text`, trimmed of surrounding punctuation.
fn abs_path_tokens(text: &str) -> Vec<PathBuf> {
    text.split_whitespace()
        .map(|t| {
            t.trim_matches(|c: char| "`\"'()<>[],;:".contains(c))
                .trim_end_matches('.')
        })
        .filter(|t| t.starts_with('/') && t.len() > 1)
        .map(PathBuf::from)
        .collect()
}

/// The record file the display points at: an absolute path token that is an
/// existing file whose bytes equal `raw`. `None` if no token qualifies.
fn record_path(reason: &str, raw: &str) -> Option<PathBuf> {
    abs_path_tokens(reason)
        .into_iter()
        .find(|p| p.is_absolute() && std::fs::read(p).is_ok_and(|b| b == raw.as_bytes()))
}

fn has_line_containing(text: &str, needle: &str) -> bool {
    text.lines().any(|l| l.contains(needle))
}

// ===========================================================================
// CONTROLS
// ===========================================================================

/// Without this, "the display lacks X" could hold because the gate never blocks.
#[test]
fn control_a_failing_check_blocks_and_names_itself_and_its_exit_code() {
    let f = fixture("ctl", "test-changed", cargo_like_raw(40), 101, None);
    let r = stop(&f, "s-ctl");
    let reason = reason_of(&r);
    assert!(reason.contains("test-changed"), "check name: {reason}");
    assert!(reason.contains("101"), "exit code: {reason}");
}

// ===========================================================================
// (a) what must stay in the display
// ===========================================================================

#[test]
fn display_keeps_decision_relevant_facts() {
    let f = fixture("a", "test-changed", cargo_like_raw(2000), 101, None);
    let reason = reason_of(&stop(&f, "s-a"));
    for needle in [
        "test-changed",
        "101",
        "suite::alpha_fails",
        "suite::beta_fails",
        "panicked at",
        ":983:5:",
        "alpha assertion message",
        ":41:9:",
        "beta boom message",
        "test result: FAILED",
    ] {
        assert!(
            reason.contains(needle),
            "display lost {needle:?}\n--- reason ---\n{reason}"
        );
    }
    assert!(
        record_path(&reason, &f.raw).is_some(),
        "the display must name the ABSOLUTE path of a file holding the full raw output \
         (no absolute-path token in the reason is a file equal to the raw output)\n\
         --- reason ---\n{reason}"
    );
}

// ===========================================================================
// (b) what must NOT be in the display
// ===========================================================================

#[test]
fn display_has_no_raw_stdout_stderr_sections_or_progress_lines() {
    let f = fixture("b1", "test-changed", cargo_like_raw(2000), 101, None);
    let reason = reason_of(&stop(&f, "s-b1"));
    for banned in [
        "--- stdout ---",
        "--- stderr ---",
        "stdout ----",
        "stderr ----",
        "progress: step",
        "RUST_BACKTRACE",
    ] {
        assert!(
            !reason.contains(banned),
            "bulk text {banned:?} leaked into the display\n--- reason ---\n{reason}"
        );
    }
    assert!(
        !has_line_containing(&reason, " ... ok"),
        "passing-test progress lines leaked into the display\n--- reason ---\n{reason}"
    );
}

#[test]
fn display_line_count_is_bounded_for_a_2000_line_output() {
    let f = fixture("b2", "test-changed", cargo_like_raw(2000), 101, None);
    assert!(
        f.raw.lines().count() >= 2000,
        "fixture must be >= 2000 lines"
    );
    let reason = reason_of(&stop(&f, "s-b2"));
    let n = reason.lines().count();
    assert!(
        n < QUIET_REASON_MAX_LINES,
        "the block reason is {n} lines; the bound is {QUIET_REASON_MAX_LINES}\n\
         --- reason ---\n{reason}"
    );
}

// ===========================================================================
// (c) the record
// ===========================================================================

#[test]
fn record_file_holds_the_full_raw_output_and_the_middle_marker_is_not_displayed() {
    let f = fixture("c", "test-changed", cargo_like_raw(2000), 101, None);
    let reason = reason_of(&stop(&f, "s-c"));
    assert!(
        !reason.contains(MIDDLE_MARKER),
        "a line from the middle of the output leaked into the display\n--- reason ---\n{reason}"
    );
    let path = record_path(&reason, &f.raw).unwrap_or_else(|| {
        panic!("no absolute path in the display is a file equal to the full raw output\n--- reason ---\n{reason}")
    });
    let bytes = std::fs::read(&path).expect("record readable");
    assert_eq!(
        bytes,
        f.raw.as_bytes(),
        "record must equal raw byte-for-byte"
    );
    assert!(
        String::from_utf8_lossy(&bytes).contains(MIDDLE_MARKER),
        "the middle marker must be in the record"
    );
}

// ===========================================================================
// (d) fail-closed when the record cannot be saved
// ===========================================================================

#[test]
fn unsavable_record_still_blocks_says_so_and_shows_more_output() {
    // state_dir lies UNDER a regular file, so nothing can be created beneath it
    // whatever the implementation names its record directory.
    let home = scratch("d-blocker");
    let blocker = home.join("blocker");
    std::fs::write(&blocker, "not a directory").expect("blocker file");
    let sd = blocker.join("sd");
    let f = fixture(
        "d",
        "test-changed",
        cargo_like_raw(150),
        101,
        Some(&sd.display().to_string()),
    );
    let r = stop(&f, "s-d");
    let reason = reason_of(&r); // (1) the block must still happen
    let lower = reason.to_lowercase();
    assert!(
        [
            "could not be saved",
            "could not save",
            "failed to save",
            "unable to save",
            "not saved",
            "not be saved",
        ]
        .iter()
        .any(|p| lower.contains(p)),
        "the display must say the record could not be saved\n--- reason ---\n{reason}"
    );
    // (2) never less with no record: the facts are still shown ...
    for needle in ["panicked at", ":983:5:", "test result: FAILED"] {
        assert!(
            reason.contains(needle),
            "fallback lost {needle:?}\n--- reason ---\n{reason}"
        );
    }
    // ... and MORE of the output is shown than in the quiet case: a
    // reasonable chunk of the passing-progress lines is allowed back in.
    let ok_lines = reason.lines().filter(|l| l.contains(" ... ok")).count();
    assert!(
        ok_lines >= 4,
        "with no record, fall back to showing more output (>= 4 progress lines here; got {ok_lines})\n\
         --- reason ---\n{reason}"
    );
}

// ===========================================================================
// (e) non-cargo check
// ===========================================================================

#[test]
fn non_cargo_check_shows_last_lines_and_the_record_path_never_empty() {
    let f = fixture("e", "typecheck", plain_raw(300), 2, None);
    let reason = reason_of(&stop(&f, "s-e"));
    assert!(
        reason.contains("typecheck") && reason.contains('2'),
        "{reason}"
    );
    for needle in [
        "LAST_LINE_UNIQUE_55e1",
        "undefined reference to `frobnicate`",
    ] {
        assert!(
            reason.contains(needle),
            "the last lines of a plain check must be displayed: lost {needle:?}\n--- reason ---\n{reason}"
        );
    }
    assert!(
        !reason.contains("building step 12 of"),
        "early bulk lines must not be displayed\n--- reason ---\n{reason}"
    );
    assert!(
        reason.lines().count() < QUIET_REASON_MAX_LINES,
        "plain-check display must also be bounded\n--- reason ---\n{reason}"
    );
    let path = record_path(&reason, &f.raw).unwrap_or_else(|| {
        panic!("no absolute record path equal to the raw output in the display\n--- reason ---\n{reason}")
    });
    assert!(Path::new(&path).is_absolute());
}
