// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Second round for the quiet failing-check feedback (written by a verifier who
//! did not write the fix, CLAUDE.md §2(a)): the paths `quiet_feedback.rs` leaves
//! untested.
//!
//! * retention failure is DISPLAYED (`record retention failed: …`) and does not
//!   un-block; retention never deletes the current record or session;
//! * the digest keeps compile errors (`error[E…]`), a non-cargo script's `FAILED`
//!   line and clippy lints, even when they are not among the last few lines;
//! * the repeat ledger recognises a repeat when only the per-run record path
//!   differs, and does NOT when the failing test differs.
//!
//! Each test builds its own `$HOME` (idiom of `quiet_feedback.rs`).

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};

static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

fn scratch(tag: &str) -> PathBuf {
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("donegate-qm-{tag}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch");
    dir
}

struct Fixture {
    home: PathBuf,
    root: PathBuf,
    sd: PathBuf,
}

/// A trusted project with one required check that prints `raw.txt` and exits 1.
/// `extra` is extra top-level toml (before the `[[check]]` table).
fn fixture(tag: &str, raw: &str, extra: &str) -> Fixture {
    let home = scratch(tag);
    let root = home.join("project");
    let sd = home.join("sd");
    std::fs::create_dir_all(&root).expect("create project root");
    std::fs::write(root.join("raw.txt"), raw).expect("write raw.txt");
    std::fs::write(
        root.join("donegate.toml"),
        format!(
            "max_attempts = 50\nstate_dir = \"{}\"\n{extra}\n[[check]]\nname = \"chk\"\ncmd = \"cat raw.txt; exit 1\"\n",
            sd.display()
        ),
    )
    .expect("write donegate.toml");
    let canon = std::fs::canonicalize(&root).expect("canonicalize project root");
    std::fs::create_dir_all(home.join(".harness")).expect("create ~/.harness");
    std::fs::write(
        home.join(".harness/trust.toml"),
        format!("trusted = [\"{}\"]\n", canon.display()),
    )
    .expect("write trust.toml");
    Fixture { home, root, sd }
}

struct Run {
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
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

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
            "the stop was not blocked.\n--- stdout ---\n{}\n--- stderr ---\n{}",
            r.stdout, r.stderr
        )
    })
}

fn full_output_path(reason: &str) -> Option<PathBuf> {
    reason
        .lines()
        .find_map(|l| l.trim().strip_prefix("full output: "))
        .map(PathBuf::from)
}

// ===========================================================================
// retention
// ===========================================================================

/// Retention cannot remove an old session directory (its inner directory is
/// read-only). The record was saved and the stop is blocked, but the failure must
/// be shown, not swallowed.
#[test]
fn retention_failure_is_displayed_and_the_stop_still_blocks() {
    let f = fixture("ret-fail", "boom\n", "");
    // The OLDEST session dir holds a read-only subdirectory with a file: removing
    // it fails with EACCES (non-root).
    let victim = f.sd.join("records/aaa-oldest");
    let sub = victim.join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::write(sub.join("f.log"), "x").unwrap();
    std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o555)).unwrap();
    for i in 0..25 {
        std::fs::create_dir_all(f.sd.join(format!("records/old{i:02}"))).unwrap();
    }
    let r = stop(&f, "s-ret-fail");
    std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o755)).unwrap();
    let reason = reason_of(&r);
    assert!(
        reason.contains("record retention failed"),
        "a retention failure must be displayed\n--- reason ---\n{reason}"
    );
    let path = full_output_path(&reason).unwrap_or_else(|| panic!("no record path\n{reason}"));
    assert!(path.is_file(), "the record itself was saved: {path:?}");
}

/// Retention keeps the current session directory and the record just written,
/// even when the store is over both bounds, and keeps the NEWEST records.
#[test]
fn retention_keeps_the_current_session_and_the_new_record() {
    let f = fixture("ret-keep", "boom\n", "");
    let sess = f.sd.join("records/s-ret-keep");
    std::fs::create_dir_all(&sess).unwrap();
    for i in 0..60 {
        std::fs::write(sess.join(format!("{:013}-a1-old.log", i + 1)), "old").unwrap();
    }
    for i in 0..30 {
        std::fs::create_dir_all(f.sd.join(format!("records/other{i:02}"))).unwrap();
    }
    let reason = reason_of(&stop(&f, "s-ret-keep"));
    assert!(
        !reason.contains("record retention failed"),
        "retention should succeed here\n{reason}"
    );
    let path = full_output_path(&reason).unwrap_or_else(|| panic!("no record path\n{reason}"));
    assert!(
        path.is_file(),
        "the just-written record must survive: {path:?}"
    );
    assert!(sess.is_dir(), "the current session dir must survive");
    let n = std::fs::read_dir(&sess).unwrap().count();
    assert_eq!(n, 50, "the current session keeps exactly 50 records");
    assert!(
        !sess.join(format!("{:013}-a1-old.log", 1)).exists(),
        "the OLDEST record is the one removed"
    );
    assert!(
        sess.join(format!("{:013}-a1-old.log", 60)).exists(),
        "the newest old record is kept"
    );
    let dirs = std::fs::read_dir(f.sd.join("records")).unwrap().count();
    assert_eq!(dirs, 20, "20 session dirs are kept (current included)");
}

// ===========================================================================
// digest
// ===========================================================================

fn compile_errors(n: usize) -> String {
    let mut s = String::from("   Compiling app v0.1.0 (/p/app)\n");
    for i in 0..n {
        s.push_str(&format!(
            "error[E0425]: cannot find value `v{i}` in this scope\n --> src/main.rs:{}:5\n  |\n{} |     v{i}\n  |     ^^ not found in this scope\n\n",
            i + 1,
            i + 1
        ));
    }
    s.push_str(&format!(
        "For more information about this error, try `rustc --explain E0425`.\nerror: could not compile `app` (bin \"app\") due to {n} previous errors\n"
    ));
    s
}

#[test]
fn digest_of_a_cargo_compile_error_keeps_the_error_code_header_and_summary() {
    let f = fixture("ce", &compile_errors(8), "");
    let reason = reason_of(&stop(&f, "s-ce"));
    for needle in [
        "error[E0425]: cannot find value `v0` in this scope",
        "could not compile `app`",
    ] {
        assert!(
            reason.contains(needle),
            "lost {needle:?}\n--- reason ---\n{reason}"
        );
    }
}

/// The file and line of a compile error is what the next action needs.
#[test]
fn digest_of_a_cargo_compile_error_keeps_the_location_of_the_first_error() {
    let f = fixture("ce-loc", &compile_errors(8), "");
    let reason = reason_of(&stop(&f, "s-ce-loc"));
    assert!(
        reason.contains("src/main.rs:1:5"),
        "the location of the first compile error was dropped\n--- reason ---\n{reason}"
    );
}

/// A script that runs tests AND builds something: a failing test line makes the
/// output cargo-shaped, and `error[E…]` lines must not be dropped for it.
#[test]
fn digest_of_mixed_test_failure_and_compile_error_keeps_the_error_code_header() {
    let mut raw = String::from("test suite::alpha ... FAILED\n");
    raw.push_str("\nfailures:\n    suite::alpha\n\n");
    raw.push_str("test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s\n");
    raw.push_str("error[E0308]: mismatched types\n --> other/src/lib.rs:7:9\n");
    raw.push_str("more\nmore\nmore\nmore\nmore\nmore\n");
    let f = fixture("mixed", &raw, "");
    let reason = reason_of(&stop(&f, "s-mixed"));
    assert!(
        reason.contains("error[E0308]: mismatched types"),
        "an error[E…] line was dropped from a cargo-shaped output\n--- reason ---\n{reason}"
    );
}

/// A non-cargo script prints its FAILED line before a long tail of other lines.
#[test]
fn digest_of_a_non_cargo_script_keeps_an_early_failed_line() {
    let mut raw = String::from("FAILED check_alpha: expected 3, got 4\n");
    for i in 0..12 {
        raw.push_str(&format!("info: step {i} finished\n"));
    }
    raw.push_str("1 check failed, 12 passed\n");
    let f = fixture("early-failed", &raw, "");
    let reason = reason_of(&stop(&f, "s-early-failed"));
    assert!(
        reason.contains("FAILED check_alpha: expected 3, got 4"),
        "a non-cargo FAILED line outside the last 5 lines was dropped\n--- reason ---\n{reason}"
    );
}

/// pytest-shaped: many FAILED lines, then a one-line summary.
#[test]
fn digest_of_pytest_shaped_output_keeps_every_failed_test_name() {
    let mut raw = String::from("=========== short test summary info ===========\n");
    for i in 0..8 {
        raw.push_str(&format!(
            "FAILED tests/test_a.py::test_case_{i} - assert 0\n"
        ));
    }
    raw.push_str("======== 8 failed, 3 passed in 0.12s ========\n");
    let f = fixture("pytest", &raw, "");
    let reason = reason_of(&stop(&f, "s-pytest"));
    for i in 0..8 {
        let needle = format!("tests/test_a.py::test_case_{i}");
        assert!(
            reason.contains(&needle),
            "failed test {needle} was dropped\n--- reason ---\n{reason}"
        );
    }
}

/// clippy with `-D warnings`: each lint is an `error:` header followed by its
/// location; the first lint's name and location, and the summary, must show.
#[test]
fn digest_of_clippy_output_keeps_lints_locations_and_summary() {
    let mut raw = String::from("    Checking app v0.1.0 (/p/app)\n");
    for i in 0..7 {
        raw.push_str(&format!(
            "error: unused variable: `x{i}`\n --> src/lib.rs:{}:9\n  |\n{} |     let x{i} = 1;\n  |         ^^ help: if intentional, prefix it with an underscore\n  |\n  = note: `-D unused-variables` implied by `-D warnings`\n\n",
            i + 3,
            i + 3
        ));
    }
    raw.push_str("error: could not compile `app` (lib) due to 7 previous errors\n");
    let f = fixture("clippy", &raw, "");
    let reason = reason_of(&stop(&f, "s-clippy"));
    for needle in [
        "error: unused variable: `x0`",
        "src/lib.rs:3:9",
        "could not compile `app`",
    ] {
        assert!(
            reason.contains(needle),
            "lost {needle:?}\n--- reason ---\n{reason}"
        );
    }
}

// ===========================================================================
// repeat ledger
// ===========================================================================

fn waived(r: &Run) -> bool {
    block_reason(r).is_none() && r.stdout.contains("systemMessage")
}

/// Two stops, same failure. The attempt counter is reset between them (the idle
/// gap exceeds `reset_after_secs`), so the reason differs ONLY by the per-run record path
/// (its timestamp and `-a<attempt>` stay distinct). The second must be recognised
/// as a repeat.
#[test]
fn a_repeat_is_recognised_when_only_the_record_path_differs() {
    let f = fixture(
        "rep-same",
        "boom: always the same\n",
        "reset_after_secs = 1\n",
    );
    let r1 = stop(&f, "s-rep-same");
    let reason1 = reason_of(&r1);
    std::thread::sleep(std::time::Duration::from_millis(2300));
    let r2 = stop(&f, "s-rep-same");
    assert!(
        waived(&r2),
        "the 2nd identical failure must be waived as a repeat\n--- 1st reason ---\n{reason1}\n--- 2nd stdout ---\n{}\n--- stderr ---\n{}",
        r2.stdout,
        r2.stderr
    );
}

/// The retention failure text is per-run state too (it lists whichever old
/// directories could not be removed). Same failure twice, but the second run's
/// retention error lists one more directory: it must still be a repeat.
/// Pins the `<why>` placeholder in the repeat identity against removal.
#[test]
fn a_repeat_is_recognised_when_only_the_retention_failure_text_differs() {
    let f = fixture(
        "rep-why",
        "boom: always the same\n",
        "reset_after_secs = 1\n",
    );
    let make_victim = |name: &str| -> PathBuf {
        let sub = f.sd.join("records").join(name).join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("f.log"), "x").unwrap();
        std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o555)).unwrap();
        sub
    };
    let age = |name: &str| {
        let d = std::fs::File::open(f.sd.join("records").join(name)).unwrap();
        d.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1000))
            .unwrap();
    };
    let sub_a = make_victim("aaa-victim-a");
    age("aaa-victim-a");
    for i in 0..25 {
        std::fs::create_dir_all(f.sd.join(format!("records/old{i:02}"))).unwrap();
    }
    let r1 = stop(&f, "s-rep-why");
    let reason1 = reason_of(&r1);
    let sub_b = make_victim("aaa-victim-b");
    age("aaa-victim-b");
    std::thread::sleep(std::time::Duration::from_millis(2300));
    let r2 = stop(&f, "s-rep-why");
    for s in [&sub_a, &sub_b] {
        std::fs::set_permissions(s, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    assert!(
        reason1.contains("record retention failed: ") && reason1.contains("aaa-victim-a"),
        "setup: the 1st run must display a retention failure naming victim a\n{reason1}"
    );
    assert!(
        r2.stdout.contains("aaa-victim-b") || waived(&r2) || block_reason(&r2).is_some(),
        "setup: the 2nd run produced no verdict\n{}",
        r2.stdout
    );
    if let Some(reason2) = block_reason(&r2) {
        assert!(
            reason2.contains("aaa-victim-b"),
            "setup: the 2nd retention error must differ from the 1st (name victim b)\n{reason2}"
        );
    }
    assert!(
        waived(&r2),
        "the 2nd identical failure must be waived as a repeat although the retention text differs\n--- 1st reason ---\n{reason1}\n--- 2nd stdout ---\n{}\n--- stderr ---\n{}",
        r2.stdout,
        r2.stderr
    );
}

/// Same session, but the failing test differs: NOT a repeat, still a block.
#[test]
fn a_different_failure_is_not_a_repeat() {
    let f = fixture(
        "rep-diff",
        "test suite::alpha ... FAILED\n\nfailures:\n    suite::alpha\n\ntest result: FAILED. 0 passed; 1 failed\n",
        "reset_after_secs = 1\n",
    );
    let _ = reason_of(&stop(&f, "s-rep-diff"));
    std::fs::write(
        f.root.join("raw.txt"),
        "test suite::beta ... FAILED\n\nfailures:\n    suite::beta\n\ntest result: FAILED. 0 passed; 1 failed\n",
    )
    .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(2300));
    let r2 = stop(&f, "s-rep-diff");
    let reason2 = reason_of(&r2);
    assert!(
        reason2.contains("suite::beta"),
        "a different failing test must block again\n{reason2}"
    );
}

// ===========================================================================
// holes found in review of cc63509a, and the repeat key in the real flow
// ===========================================================================

/// The same error text in several files: each diagnostic has the same header
/// line and a different location. When the header of the LAST one is among the
/// final lines of the output, the earlier ones must not be skipped because
/// their header text is "already shown".
#[test]
fn digest_keeps_the_locations_of_same_header_diagnostics_in_different_files() {
    let mut raw = String::from("   Compiling app v0.1.0 (/p/app)\n");
    for (n, file) in ["a", "b", "c"].iter().enumerate() {
        raw.push_str(&format!(
            "error[E0425]: cannot find value `x` in this scope\n --> src/{file}.rs:{}:5\n {} | x\n\n",
            n + 1,
            n + 1
        ));
    }
    raw.push_str("error: could not compile `app` (bin \"app\") due to 3 previous errors\n");
    let f = fixture("same-header", &raw, "");
    let reason = reason_of(&stop(&f, "s-same-header"));
    for loc in ["src/a.rs:1:5", "src/b.rs:2:5", "src/c.rs:3:5"] {
        assert!(
            reason.contains(loc),
            "the location {loc} of a compile error was dropped\n--- reason ---\n{reason}"
        );
    }
}

/// `go test` names its failures `--- FAIL: TestName`; there is no `FAILED`.
#[test]
fn digest_of_go_test_output_keeps_every_failing_test_name() {
    let mut raw = String::new();
    for i in 0..8 {
        raw.push_str(&format!(
            "--- FAIL: TestCase{i} (0.00s)\n    case_test.go:{}: expected 1, got 2\n",
            i + 10
        ));
    }
    raw.push_str("FAIL\nFAIL\tapp/pkg\t0.012s\n");
    let f = fixture("gotest", &raw, "");
    let reason = reason_of(&stop(&f, "s-gotest"));
    for i in 0..8 {
        let name = format!("TestCase{i}");
        assert!(
            reason.contains(&name),
            "failing test {name} was dropped\n--- reason ---\n{reason}"
        );
    }
}

// ===========================================================================
// tsc / gcc / clang diagnostics (single-line `file(line,col)` / `file:line:col`)
// ===========================================================================

fn tsc_output(n: usize) -> String {
    let mut s = String::new();
    for i in 0..n {
        s.push_str(&format!(
            "src/mod{i}.ts({},5): error TS2322: Type 'string' is not assignable to type 'number'.\n",
            i + 3
        ));
    }
    s.push_str(&format!("\nFound {n} errors in {n} files.\n"));
    s
}

fn gcc_output(n: usize) -> String {
    let mut s = String::new();
    for i in 0..n {
        s.push_str(&format!(
            "src/mod{i}.c:{}:5: error: use of undeclared identifier 'v{i}'\n    v{i} = 1;\n    ^\n",
            i + 3
        ));
    }
    s.push_str(&format!("{n} errors generated.\nmake: *** [all] Error 1\n"));
    s
}

/// Few tsc diagnostics: each one's code, message and `file(line,col)` survives,
/// as does the summary line.
#[test]
fn digest_of_tsc_output_keeps_every_diagnostic_and_the_summary() {
    // 5 diagnostics + the summary: the first is outside the last 5 lines.
    let f = fixture("tsc", &tsc_output(5), "");
    let reason = reason_of(&stop(&f, "s-tsc"));
    for i in 0..5 {
        let loc = format!("src/mod{i}.ts({},5): error TS2322", i + 3);
        assert!(
            reason.contains(&loc),
            "tsc diagnostic {loc} was dropped\n--- reason ---\n{reason}"
        );
    }
    assert!(
        reason.contains("Found 5 errors in 5 files."),
        "tsc summary dropped\n--- reason ---\n{reason}"
    );
}

/// Many tsc diagnostics: not tail-only. The FIRST diagnostic and the summary
/// are shown, and the rest are counted.
#[test]
fn digest_of_many_tsc_diagnostics_is_not_tail_only() {
    let f = fixture("tsc-many", &tsc_output(40), "");
    let reason = reason_of(&stop(&f, "s-tsc-many"));
    assert!(
        reason.contains("src/mod0.ts(3,5): error TS2322"),
        "the first tsc diagnostic was dropped (tail-only display)\n--- reason ---\n{reason}"
    );
    assert!(
        reason.contains("Found 40 errors in 40 files."),
        "tsc summary dropped\n--- reason ---\n{reason}"
    );
    assert!(
        reason.contains("more"),
        "dropped diagnostics must be counted\n--- reason ---\n{reason}"
    );
}

/// Few gcc/clang diagnostics over several files plus a trailing summary.
#[test]
fn digest_of_gcc_output_keeps_every_diagnostic_and_the_summary() {
    let f = fixture("gcc", &gcc_output(4), "");
    let reason = reason_of(&stop(&f, "s-gcc"));
    for i in 0..4 {
        let loc = format!(
            "src/mod{i}.c:{}:5: error: use of undeclared identifier 'v{i}'",
            i + 3
        );
        assert!(
            reason.contains(&loc),
            "gcc diagnostic {loc} was dropped\n--- reason ---\n{reason}"
        );
    }
    assert!(
        reason.contains("4 errors generated."),
        "gcc summary dropped\n--- reason ---\n{reason}"
    );
}

/// Many gcc diagnostics: the first diagnostic and the summary are shown.
#[test]
fn digest_of_many_gcc_diagnostics_is_not_tail_only() {
    let f = fixture("gcc-many", &gcc_output(40), "");
    let reason = reason_of(&stop(&f, "s-gcc-many"));
    assert!(
        reason.contains("src/mod0.c:3:5: error: use of undeclared identifier 'v0'"),
        "the first gcc diagnostic was dropped (tail-only display)\n--- reason ---\n{reason}"
    );
    assert!(
        reason.contains("40 errors generated."),
        "gcc summary dropped\n--- reason ---\n{reason}"
    );
    assert!(
        reason.contains("more"),
        "dropped diagnostics must be counted\n--- reason ---\n{reason}"
    );
}

// ===========================================================================
// pins added by the verifier for the digest's remaining branches
// ===========================================================================

/// python unittest: `FAIL: test_x (mod.Class)` blocks, no `FAILED` before the
/// last line.
#[test]
fn digest_of_unittest_output_keeps_every_fail_line() {
    let mut raw = String::new();
    for i in 0..8 {
        raw.push_str(&format!(
            "======================\nFAIL: test_x{i} (mod.Class)\n----------------------\nAssertionError: 1 != 2\n"
        ));
    }
    raw.push_str("\nRan 8 tests in 0.01s\n\nFAILED (failures=8)\n");
    let f = fixture("unittest", &raw, "");
    let reason = reason_of(&stop(&f, "s-unittest"));
    for i in 0..8 {
        let name = format!("FAIL: test_x{i} (mod.Class)");
        assert!(
            reason.contains(&name),
            "{name} was dropped\n--- reason ---\n{reason}"
        );
    }
}

/// jest / go package style: `FAIL <path>` lines among other output.
#[test]
fn digest_of_fail_path_lines_keeps_every_path() {
    let mut raw = String::new();
    for i in 0..8 {
        raw.push_str(&format!(
            "FAIL src/a{i}.test.js\n  ● suite {i} › case\n    expect(received).toBe(expected)\n"
        ));
    }
    raw.push_str("Tests: 8 failed, 8 total\nTime: 1.2 s\nRan all test suites.\n");
    let f = fixture("jest", &raw, "");
    let reason = reason_of(&stop(&f, "s-jest"));
    for i in 0..8 {
        let name = format!("FAIL src/a{i}.test.js");
        assert!(
            reason.contains(&name),
            "{name} was dropped\n--- reason ---\n{reason}"
        );
    }
}

/// 14 FAILED lines: the first 10 are shown and the other 4 are counted.
#[test]
fn digest_caps_failed_lines_at_ten_and_counts_the_rest() {
    let mut raw = String::new();
    for i in 0..14 {
        raw.push_str(&format!(
            "FAILED tests/test_b.py::test_case_{i:02} - assert 0\n"
        ));
    }
    raw.push_str("noise 1\nnoise 2\nnoise 3\nnoise 4\nnoise 5\n");
    let f = fixture("failed-cap", &raw, "");
    let reason = reason_of(&stop(&f, "s-failed-cap"));
    assert!(
        reason.contains("test_case_00") && reason.contains("test_case_09"),
        "the first ten FAILED lines must be shown\n--- reason ---\n{reason}"
    );
    assert!(
        !reason.contains("test_case_10"),
        "only the first ten FAILED lines are shown; the rest are counted\n--- reason ---\n{reason}"
    );
    assert!(
        reason.contains("… and 4 more FAILED lines"),
        "the rest must be counted\n--- reason ---\n{reason}"
    );
}

/// An early gcc `fatal error` and `warning` (outside the last 5 lines) are
/// shown.
#[test]
fn digest_keeps_gcc_fatal_error_and_warning_lines() {
    let mut raw = String::from("src/f.h:1:10: fatal error: nofile.h: No such file or directory\n");
    raw.push_str("src/w.c:2:1: warning: unused variable 'q' [-Wunused-variable]\n");
    for i in 0..10 {
        raw.push_str(&format!("cc -c unit{i}.c\n"));
    }
    raw.push_str("make: *** [all] Error 1\n");
    let f = fixture("gcc-kinds", &raw, "");
    let reason = reason_of(&stop(&f, "s-gcc-kinds"));
    for needle in [
        "src/f.h:1:10: fatal error: nofile.h",
        "src/w.c:2:1: warning: unused variable 'q'",
    ] {
        assert!(
            reason.contains(needle),
            "{needle} was dropped\n--- reason ---\n{reason}"
        );
    }
}

/// Seven warnings come before the only error: the error must not be pushed out
/// by the cap of five.
#[test]
fn digest_shows_an_error_that_follows_many_warnings() {
    let mut raw = String::new();
    for i in 0..7 {
        raw.push_str(&format!(
            "src/w{i}.c:{}:1: warning: unused variable 'q{i}'\n",
            i + 1
        ));
    }
    raw.push_str("src/e.c:9:9: error: expected ';' before '}' token\n");
    for i in 0..8 {
        raw.push_str(&format!("cc -c unit{i}.c\n"));
    }
    raw.push_str("make: *** [all] Error 1\n");
    let f = fixture("err-after-warn", &raw, "");
    let reason = reason_of(&stop(&f, "s-err-after-warn"));
    assert!(
        reason.contains("src/e.c:9:9: error: expected ';'"),
        "the error behind many warnings was dropped\n--- reason ---\n{reason}"
    );
}

/// tsc prints a per-file table AFTER its `Found N errors` summary, so the
/// summary is not among the last five lines.
#[test]
fn digest_keeps_a_tsc_summary_that_is_followed_by_the_per_file_table() {
    let mut raw = String::new();
    for i in 0..5 {
        raw.push_str(&format!(
            "src/m{i}.ts({},5): error TS2322: Type 'string' is not assignable to type 'number'.\n",
            i + 1
        ));
    }
    raw.push_str("\nFound 5 errors in 5 files.\n\nErrors  Files\n");
    for i in 0..5 {
        raw.push_str(&format!("     1  src/m{i}.ts:{}\n", i + 1));
    }
    let f = fixture("tsc-table", &raw, "");
    let reason = reason_of(&stop(&f, "s-tsc-table"));
    assert!(
        reason.contains("Found 5 errors in 5 files."),
        "the tsc summary line was dropped\n--- reason ---\n{reason}"
    );
}

/// The same for clang's `N errors generated.` followed by more lines.
#[test]
fn digest_keeps_a_clang_summary_that_is_followed_by_more_lines() {
    let mut raw = String::from("src/a.c:3:5: error: use of undeclared identifier 'q'\n");
    raw.push_str("    q = 1;\n    ^\n2 errors generated.\n");
    for i in 0..7 {
        raw.push_str(&format!("make[{i}]: leaving directory\n"));
    }
    let f = fixture("clang-sum", &raw, "");
    let reason = reason_of(&stop(&f, "s-clang-sum"));
    assert!(
        reason.contains("2 errors generated."),
        "the clang summary line was dropped\n--- reason ---\n{reason}"
    );
}

/// A `-->` line belongs to a header only within three lines of it: the third
/// line after the header still counts, the fourth does not. The far one is
/// something else (a note quoting a path) and must not be attributed to the
/// error. Pins the location window against removal.
#[test]
fn digest_attributes_a_location_line_only_within_three_lines_of_the_header() {
    let mut raw = String::from("   Compiling app v0.1.0 (/p/app)\n");
    raw.push_str("error[E0001]: near error\n  |\n  |\n --> src/near.rs:11:5\n\n");
    raw.push_str("error[E0002]: far error\n  |\n  |\n  |\n --> src/far.rs:77:5\n\n");
    for i in 0..8 {
        raw.push_str(&format!(
            "warning: filler {i}\n --> src/filler{i}.rs:{}:1\n\n",
            i + 1
        ));
    }
    raw.push_str("error: could not compile `app` (bin \"app\") due to 2 previous errors\n");
    let f = fixture("diag-window", &raw, "");
    let reason = reason_of(&stop(&f, "s-diag-window"));
    assert!(
        reason.contains("error[E0002]: far error"),
        "the far error's header must be shown\n--- reason ---\n{reason}"
    );
    assert!(
        reason.contains("src/near.rs:11:5"),
        "a location 3 lines after the header is attributed\n--- reason ---\n{reason}"
    );
    assert!(
        !reason.contains("src/far.rs:77:5"),
        "a location 4 lines after the header must not be attributed\n--- reason ---\n{reason}"
    );
}

/// Eight located rustc errors: five are shown, three counted.
#[test]
fn digest_caps_located_diagnostics_at_five_and_counts_the_rest() {
    let f = fixture("diag-cap", &compile_errors(8), "");
    let reason = reason_of(&stop(&f, "s-diag-cap"));
    assert!(
        reason.contains("src/main.rs:5:5") && !reason.contains("src/main.rs:6:5"),
        "exactly the first five located diagnostics are shown\n--- reason ---\n{reason}"
    );
    assert!(
        reason.contains("… and 3 more diagnostics"),
        "the rest must be counted\n--- reason ---\n{reason}"
    );
}
