// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! CLI integration test for the condukt-gate-decisions bridge
//! (`overwatch auto-approved`, `review_gate_decisions.rs`).
//!
//! `harness_core::config::base_dir` resolves `~/.condukt` via
//! `dirs::home_dir()`, which on unix reads the `HOME` env var — the SAME
//! override mechanism `tests/review_escalation.rs` already uses to sandbox
//! condukt's foreign state. So this test seeds a temp-HOME condukt
//! `gate-decisions.jsonl` by hand at its FLAT default path (condukt's own
//! on-disk shape — no condukt binary/crate needed, keeping the dependency
//! direction intact), runs the REAL `overwatch auto-approved --json` CLI
//! against that sandboxed HOME, and asserts the auto-only count, the sample
//! size, and determinism across two runs with the same seed.

use serde_json::Value;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

fn make_sandbox(tag: &str) -> (PathBuf, PathBuf) {
    let n = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
    let base = std::env::temp_dir().join(format!(
        "overwatch-auto-approved-test-{tag}-{}-{n}",
        std::process::id()
    ));
    let home = base.join("home");
    let work = base.join("work");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&work).unwrap();
    (home, work)
}

fn overwatch_bin() -> &'static str {
    env!("CARGO_BIN_EXE_overwatch")
}

#[test]
fn auto_approved_reports_only_auto_rows_and_is_deterministic() {
    let (home, work) = make_sandbox("basic");

    // Seed condukt's gate-decisions.jsonl directly at its DEFAULT (FLAT,
    // no project-key segment) path, mirroring condukt's own
    // `gatelog::decisions_path` / `config.rs::base_dir` derivation, without
    // depending on the condukt crate.
    let decisions_path = home
        .join(".condukt")
        .join("state")
        .join("gate-decisions.jsonl");
    std::fs::create_dir_all(decisions_path.parent().unwrap()).unwrap();

    let m = 8usize;
    let k = 3usize;
    let mut lines = Vec::new();
    for i in 0..m {
        lines.push(format!(
            r#"{{"question":"Q{i}","options":["a","b"],"recommend_index":0,"chosen":"a","policy":"auto","created_at":{}}}"#,
            100 + i
        ));
    }
    // One non-auto row that must be excluded.
    lines.push(
        r#"{"question":"Qesc","options":["x"],"recommend_index":0,"chosen":"x","policy":"escalate","created_at":999}"#
            .to_string(),
    );
    std::fs::write(&decisions_path, lines.join("\n")).unwrap();

    let run = |seed: &str| {
        let out = Command::new(overwatch_bin())
            .args([
                "auto-approved",
                "--json",
                "--sample",
                &k.to_string(),
                "--seed",
                seed,
            ])
            .env("HOME", &home)
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .current_dir(&work)
            .output()
            .expect("failed to spawn overwatch binary");
        assert!(
            out.status.success(),
            "auto-approved exited non-zero: stderr={}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).expect("stdout not utf8")
    };

    let stdout1 = run("42");
    let stdout2 = run("42");

    let v1: Value = serde_json::from_str(&stdout1).expect("auto-approved --json must be parseable");
    let v2: Value = serde_json::from_str(&stdout2).expect("auto-approved --json must be parseable");

    assert_eq!(v1["count"], m as i64, "only auto rows counted: {v1}");
    assert_eq!(v1["sample_size"], k as i64);
    assert_eq!(v1["seed"], 42);
    assert_eq!(v1["since"], Value::Null);

    let sample1 = v1["sample"].as_array().expect("sample must be an array");
    assert_eq!(sample1.len(), k);
    for row in sample1 {
        assert_eq!(
            row["policy"], "auto",
            "non-auto row leaked into sample: {row}"
        );
    }

    // Determinism: same seed => byte-identical sample across two runs.
    assert_eq!(
        v1["sample"], v2["sample"],
        "same (population, k, seed) must yield an identical sample"
    );
    assert_eq!(stdout1, stdout2, "full JSON output must be byte-identical");
}

#[test]
fn auto_approved_missing_journal_reports_zero() {
    let (home, work) = make_sandbox("missing");
    // Intentionally never seed gate-decisions.jsonl.

    let out = Command::new(overwatch_bin())
        .args(["auto-approved", "--json"])
        .env("HOME", &home)
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .current_dir(&work)
        .output()
        .expect("failed to spawn overwatch binary");
    assert!(
        out.status.success(),
        "auto-approved exited non-zero: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("stdout not utf8");
    let v: Value = serde_json::from_str(&stdout).expect("auto-approved --json must be parseable");
    assert_eq!(v["count"], 0);
    assert_eq!(v["sample_size"], 0);
    assert_eq!(v["sample"].as_array().unwrap().len(), 0);
}

// ---------------------------------------------------------------------------
// backlog afdcfd4d — "could not read the journal" must not render as
// "nothing was auto-approved".
//
// `overwatch auto-approved` is the AUDIT SURFACE for gates that were answered
// automatically instead of by a human. `review_gate_decisions::read_auto_approved`
// currently maps BOTH "the journal does not exist" and "the journal exists but
// could not be read" to `Vec::new()`, and `parse_auto_approved` drops
// undecodable lines with `.filter_map(.. .ok())`. All three states print the
// same reassuring "0 decision(s) passed a gate without human review" / a
// partial count, which is the gate asserting the answer it specifically failed
// to establish (CLAUDE.md §1 "判定を持つ" is decided by consumption, §3 lists
// IO failure / unparseable input / the empty set as things that must never map
// to clean).
//
// These tests pin the PROPERTY (the three states are distinguishable to the
// reader, and undetermined does not exit clean), not any particular rendering.
// `crates/overwatch/src/store.rs`'s `scan_jsonl`/`decode_jsonl_lines` is the
// established tri-state idiom in this crate and
// `harness_core::boundary::read_to_string` already distinguishes absent / read
// / IO-error — but nothing below assumes that specific shape.
// ---------------------------------------------------------------------------

/// Three well-formed `policy == "auto"` rows: the determined population every
/// comparison below is measured against.
const AUTO_ROW_LINES: &str = concat!(
    r#"{"question":"Q1","options":["a"],"recommend_index":0,"chosen":"a","policy":"auto","created_at":101}"#,
    "\n",
    r#"{"question":"Q2","options":["b"],"recommend_index":0,"chosen":"b","policy":"auto","created_at":102}"#,
    "\n",
    r#"{"question":"Q3","options":["c"],"recommend_index":0,"chosen":"c","policy":"auto","created_at":103}"#,
);

/// Run the real `overwatch auto-approved` CLI against a sandboxed `$HOME` and
/// return `(exit_code, stdout, stderr)` WITHOUT asserting success — the exit
/// code is itself part of the contract under test, so it must not be consumed
/// by an `assert!(status.success())` on the way in.
fn run_auto_approved_cli(
    home: &std::path::Path,
    work: &std::path::Path,
    args: &[&str],
) -> (Option<i32>, String, String) {
    let out = Command::new(overwatch_bin())
        .arg("auto-approved")
        .args(args)
        .env("HOME", home)
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .current_dir(work)
        .output()
        .expect("failed to spawn overwatch binary");
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// condukt's FLAT default journal path under a sandboxed `$HOME`.
fn journal_path(home: &std::path::Path) -> PathBuf {
    home.join(".condukt")
        .join("state")
        .join("gate-decisions.jsonl")
}

fn seed_journal(home: &std::path::Path, body: &str) -> PathBuf {
    let p = journal_path(home);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, body).unwrap();
    p
}

/// Seed a journal that EXISTS but cannot be read, and then OBSERVE that it
/// really is unreadable. The observation matters: `chmod 000` is a no-op for
/// uid 0, and a test that silently degrades to "readable file" would assert
/// the wrong contract while staying green. CLAUDE.md §2 says an unmeasurable
/// property must be surfaced, not quietly waved through, so the precondition
/// failure is loud and labels itself as a precondition.
fn seed_unreadable_journal(home: &std::path::Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let p = seed_journal(home, AUTO_ROW_LINES);
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o000)).unwrap();
    assert!(
        std::fs::read_to_string(&p).is_err(),
        "PRECONDITION FAILED (not a contract failure): {} is still readable after chmod 000 \
         — running as uid 0? The unreadable-journal contract cannot be OBSERVED in this \
         environment, so this test must not report a verdict about it.",
        p.display()
    );
    p
}

/// An unreadable journal must not be rendered as a determined zero, and must
/// be distinguishable from a journal that genuinely does not exist.
#[test]
fn unreadable_journal_is_not_reported_as_zero_auto_approved() {
    let (home_absent, work_absent) = make_sandbox("unreadable-text-absent");
    let (home_bad, work_bad) = make_sandbox("unreadable-text-bad");
    // home_absent: deliberately never seeded.
    seed_unreadable_journal(&home_bad);

    let absent = run_auto_approved_cli(&home_absent, &work_absent, &[]);
    let unreadable = run_auto_approved_cli(&home_bad, &work_bad, &[]);

    assert!(
        !unreadable
            .1
            .contains("0 decision(s) passed a gate without human review"),
        "an UNREADABLE journal printed the determined-zero claim the command specifically \
         failed to establish.\nstdout:\n{}\nstderr:\n{}",
        unreadable.1,
        unreadable.2
    );
    assert_ne!(
        (unreadable.0, unreadable.1.as_str()),
        (absent.0, absent.1.as_str()),
        "a journal that COULD NOT BE READ produced byte-identical (exit code, stdout) to a \
         journal that genuinely DOES NOT EXIST — the audit surface cannot tell a reader \
         'nothing was auto-approved' apart from 'I could not find out'.\nabsent stderr:\n{}\n\
         unreadable stderr:\n{}",
        absent.2,
        unreadable.2
    );
}

/// Undetermined must resolve restrictively: an unreadable journal must not
/// exit 0 (CLAUDE.md §3). This crate's existing convention for "this is an
/// answer, not a crash" is exit 3 — `review_queue::SourceHealth::exit_code`
/// ("0 when the answer is complete, 3 when it is not"), which
/// `exit_on_undetermined_sources` already applies to `review-queue` and
/// `review-metrics`. This test only pins NON-ZERO, so adopting 3 satisfies it
/// without the test prescribing the number.
#[test]
fn unreadable_journal_does_not_exit_clean() {
    let (home, work) = make_sandbox("unreadable-exit");
    seed_unreadable_journal(&home);

    let (code, stdout, stderr) = run_auto_approved_cli(&home, &work, &[]);
    assert_ne!(
        code,
        Some(0),
        "an unreadable journal exited 0, so `set -e` / `if overwatch auto-approved` / any \
         script wrapper reads 'the audit surface ran and found nothing'.\nstdout:\n{stdout}\n\
         stderr:\n{stderr}"
    );
}

/// The `--json` surface carries the same distinction, so a machine consumer
/// cannot read a successful `count: 0` as "nothing was auto-approved" when the
/// truth is "could not read".
#[test]
fn unreadable_journal_json_is_distinguishable_from_absent_json() {
    let (home_absent, work_absent) = make_sandbox("unreadable-json-absent");
    let (home_bad, work_bad) = make_sandbox("unreadable-json-bad");
    seed_unreadable_journal(&home_bad);

    let absent = run_auto_approved_cli(&home_absent, &work_absent, &["--json"]);
    let unreadable = run_auto_approved_cli(&home_bad, &work_bad, &["--json"]);

    // Sanity: the absent case really is the determined zero we are contrasting
    // against (otherwise the inequality below could pass for the wrong reason).
    let va: Value = serde_json::from_str(&absent.1)
        .expect("absent-journal --json must still be parseable JSON");
    assert_eq!(
        va["count"], 0,
        "precondition: an absent journal is a determined zero"
    );

    if let Ok(vb) = serde_json::from_str::<Value>(&unreadable.1) {
        assert!(
            !(unreadable.0 == Some(0) && vb.get("count") == Some(&Value::from(0))),
            "`--json` handed a machine consumer a SUCCESSFUL `count: 0` for a journal that \
             could not be read.\nstdout:\n{}\nstderr:\n{}",
            unreadable.1,
            unreadable.2
        );
    }
    assert_ne!(
        (unreadable.0, unreadable.1.as_str()),
        (absent.0, absent.1.as_str()),
        "`--json` is byte-identical for 'could not read the journal' and 'the journal does \
         not exist'; no machine consumer can separate them."
    );
}

/// A journal holding N decodable rows plus one undecodable line must not be
/// reported exactly as N, as if N were the whole population. Whether the right
/// answer is "undetermined" or "N plus an explicit warning naming the bad line"
/// is a design call — this pins only that the corruption is VISIBLE in the
/// output, on both the human and the machine surface.
#[test]
fn an_undecodable_line_is_visible_in_the_output() {
    let (home_clean, work_clean) = make_sandbox("corrupt-clean");
    let (home_corrupt, work_corrupt) = make_sandbox("corrupt-bad");
    seed_journal(&home_clean, AUTO_ROW_LINES);
    seed_journal(
        &home_corrupt,
        &(AUTO_ROW_LINES.to_owned() + "\nnot json at all {{{"),
    );

    let clean_json = run_auto_approved_cli(&home_clean, &work_clean, &["--json", "--sample", "0"]);
    let corrupt_json =
        run_auto_approved_cli(&home_corrupt, &work_corrupt, &["--json", "--sample", "0"]);

    // Sanity: the clean baseline really is a determined 3.
    let vc: Value =
        serde_json::from_str(&clean_json.1).expect("clean-journal --json must be parseable JSON");
    assert_eq!(vc["count"], 3, "precondition: 3 decodable auto rows");
    assert_eq!(clean_json.0, Some(0), "precondition: clean journal exits 0");

    assert_ne!(
        (corrupt_json.0, corrupt_json.1.as_str()),
        (clean_json.0, clean_json.1.as_str()),
        "a journal with 3 good rows AND an undecodable line produced byte-identical \
         (exit code, --json stdout) to a journal with only the 3 good rows — the dropped \
         line is invisible, so a PARTIAL population is indistinguishable from a complete \
         one.\ncorrupt stderr:\n{}",
        corrupt_json.2
    );

    let clean_text = run_auto_approved_cli(&home_clean, &work_clean, &["--sample", "0"]);
    let corrupt_text = run_auto_approved_cli(&home_corrupt, &work_corrupt, &["--sample", "0"]);
    assert_ne!(
        (corrupt_text.0, corrupt_text.1.as_str()),
        (clean_text.0, clean_text.1.as_str()),
        "same on the human-readable surface: the undecodable line changes nothing a reader \
         can see.\ncorrupt stderr:\n{}",
        corrupt_text.2
    );
}
