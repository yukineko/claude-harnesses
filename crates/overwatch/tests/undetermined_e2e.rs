// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg(unix)]
//! Third-party (disinterested-agent) coverage for the "a source I could not
//! read is not a source that held nothing" contract, at the **CLI boundary**.
//!
//! # Why this file exists at all
//!
//! The three commits that introduced the tri-state readers and migrated their
//! consumers (`d9a2d291`, `32237ae5`, `69a90432`) each disclosed the same
//! deviation from CLAUDE.md 2(a): *the tests were written by the agent that
//! wrote the fix*. This file is the compensating independent pass. It was
//! written against the CLI surface only — no `pub` internals, no test-only
//! shims — so nothing here can be satisfied by an implementation detail that
//! merely agrees with the implementer's mental model.
//!
//! # What each test here does NOT prove
//!
//! Stated up front, because "this test is green" is worth exactly as much as
//! the answer to "what would have to break for it to go red":
//!
//! * None of these prove the WARNING text is the right wording, only that the
//!   channel a given consumer actually reads (stdout prose / the `--json`
//!   body / the exit code) can distinguish "unread" from "empty".
//! * None prove anything about a store that changes UNDER the command
//!   (TOCTOU); every ledger here is corrupted before the process starts.
//! * The `make_unreadable` trick (a directory where a file is expected) is one
//!   specific opacity. Permission bits are deliberately not used — `chmod 000`
//!   is a no-op for root, and a test that silently passes for one user and not
//!   another proves less than it appears to.
//!
//! # Anti-vacuity
//!
//! Every failure-arm test below is PAIRED with a control in the opposite
//! polarity, in the SAME file, seeded through the same helpers. An
//! implementation that answered "undetermined" for every read satisfies every
//! failure arm in this file while blinding the whole review surface; only the
//! controls refuse it. Both polarities were confirmed by mutation, not by
//! assertion — see the task report for the counts.

use serde_json::Value;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

fn make_sandbox(tag: &str) -> (PathBuf, PathBuf) {
    let n = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
    let base = std::env::temp_dir().join(format!(
        "overwatch-undet-e2e-{tag}-{}-{n}",
        std::process::id()
    ));
    let home = base.join("home");
    let work = base.join("work");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&work).unwrap();
    (home, work)
}

fn overwatch_bin() -> &'static str {
    env!("CARGO_BIN_EXE_overwatch")
}

/// Run the binary in a sandboxed HOME + cwd and return the whole `Output`.
/// The process HOME of the TEST is never mutated (unlike
/// `bridge_entries_to_backlog.rs`), so these tests cannot race each other or
/// any other test binary over a process-global env var.
fn run_ow_raw(home: &Path, work: &Path, args: &[&str]) -> Output {
    Command::new(overwatch_bin())
        .args(args)
        .env("HOME", home)
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .env_remove("OVERWATCH_BACKLOG_BIN")
        .current_dir(work)
        .output()
        .expect("failed to spawn overwatch binary")
}

/// Run and assert exit 0, returning stdout (for seeding steps only).
fn seed_ow(home: &Path, work: &Path, args: &[&str]) {
    let out = run_ow_raw(home, work, args);
    assert!(
        out.status.success(),
        "seeding step `overwatch {:?}` must succeed: stderr={}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
}

fn stdout_of(out: &Output) -> String {
    String::from_utf8(out.stdout.clone()).expect("stdout not utf8")
}

fn stderr_of(out: &Output) -> String {
    String::from_utf8(out.stderr.clone()).expect("stderr not utf8")
}

fn json_of(out: &Output) -> Value {
    serde_json::from_str(&stdout_of(out))
        .unwrap_or_else(|e| panic!("stdout must be JSON ({e}): {}", stdout_of(out)))
}

/// The overwatch store dir for this sandbox, derived with the SAME
/// `harness_core` symbols `store.rs` uses.
fn store_dir(home: &Path, work: &Path) -> PathBuf {
    let repo_root = harness_core::projkey::repo_root(work);
    let project_key = harness_core::projkey::project_key(&repo_root);
    home.join(".overwatch").join(project_key).join("overwatch")
}

/// Make a path PRESENT but unreadable-as-a-file, root-proof (a directory at
/// the file's path — `chmod 0` does not stop root).
fn make_unreadable(path: &Path) {
    if path.is_file() {
        fs::remove_file(path).expect("remove the valid file");
    }
    fs::create_dir_all(path).expect("replace it with a directory");
}

/// Append one line that no record type can decode.
fn append_undecodable_line(path: &Path) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut txt = fs::read_to_string(path).unwrap_or_default();
    if !txt.is_empty() && !txt.ends_with('\n') {
        txt.push('\n');
    }
    txt.push_str("{ this line decodes as nothing at all\n");
    fs::write(path, txt).unwrap();
}

/// Record the same gate-violation signature across three DISTINCT tasks so the
/// recurrence path escalates it to systemic (threshold 3, spanning >1 task).
fn seed_systemic_violation(home: &Path, work: &Path) {
    for task in ["ta", "tb", "tc"] {
        seed_ow(
            home,
            work,
            &[
                "record-violation",
                "--source",
                "blastguard",
                "--discriminator",
                "rm-rf",
                "--task",
                task,
                "--session",
                task,
            ],
        );
    }
}

fn seed_rollback(home: &Path, work: &Path, plugin: &str) {
    seed_ow(
        home,
        work,
        &[
            "record-rollback",
            "--plugin",
            plugin,
            "--to-version",
            "0.2.0",
            "--reason",
            "raw",
        ],
    );
}

fn seed_finding(home: &Path, work: &Path, id: &str) {
    seed_ow(
        home,
        work,
        &[
            "record-finding",
            "--finding-id",
            id,
            "--source",
            "continuous-audit",
            "--severity",
            "high",
            "--summary",
            "a confirmed finding",
        ],
    );
}

/// One valid `merge_conflicts.jsonl` line (the shape `append_merge_conflict`
/// writes), seeded directly because only condukt produces this ledger.
fn seed_merge_conflict(dir: &Path, id: &str) {
    fs::create_dir_all(dir).unwrap();
    let line = serde_json::json!({
        "conflict_id": id,
        "origin": "merge-conflict",
        "run_id": "runA",
        "branch": "condukt/t2",
        "default_branch": "main",
        "base_ref": "base",
        "conflicted_files": ["crates/x/src/main.rs"],
        "diff_ours": "ours",
        "diff_theirs": "theirs",
        "ts": 400
    });
    fs::write(dir.join("merge_conflicts.jsonl"), format!("{line}\n")).unwrap();
}

/// The `kind: "undetermined-source"` marker rows in a `review-queue --json`
/// body, by the `identifier` (file name) each names.
fn marker_identifiers(body: &Value) -> Vec<String> {
    body.as_array()
        .expect("review-queue --json is an array of rows")
        .iter()
        .filter(|r| r["kind"] == "undetermined-source")
        .map(|r| r["identifier"].as_str().unwrap_or_default().to_string())
        .collect()
}

// ===========================================================================
// 1. The violation source — the ONE review-queue source t3 left without a
//    CLI-level failure arm (rollbacks, escalations, merge conflicts and the
//    resolution filter all got one in `tests/review_queue.rs`; source 1 did
//    not). Same mirror-gap class the three commits were fixing.
// ===========================================================================

#[test]
fn undecodable_violation_ledger_is_not_rendered_as_no_systemic_violations() {
    // A systemic gate-violation signature IS on the ledger, and then one line
    // of it stops decoding. `scan_violations` calls the whole scan
    // untrustworthy, and the queue must not print the sentence that names
    // systemic violations as absent.
    let (home, work) = make_sandbox("violations-undecodable");
    seed_systemic_violation(&home, &work);
    append_undecodable_line(&store_dir(&home, &work).join("violations.jsonl"));

    let out = run_ow_raw(&home, &work, &["review-queue"]);
    let (stdout, stderr) = (stdout_of(&out), stderr_of(&out));

    assert!(
        stderr.contains("WARNING") && stderr.contains("violation"),
        "an undecodable violation ledger must be announced; stderr={stderr:?}"
    );
    assert!(
        !stdout.contains("review queue empty"),
        "an UNDETERMINED violation source must never be reported as an empty queue; \
         stdout={stdout:?}"
    );
    assert!(
        !stdout.contains("no systemic violations"),
        "the prose must not name systemic violations as absent when the ledger could \
         not be read; stdout={stdout:?}"
    );
    assert_eq!(
        out.status.code(),
        Some(3),
        "a `--json`-filtering or exit-code-checking consumer only learns the queue is \
         incomplete from the exit code; stderr={stderr}"
    );

    // And in band, so a `jq 'length == 0'` consumer cannot read it as clean.
    let json = run_ow_raw(&home, &work, &["review-queue", "--json"]);
    assert_eq!(
        marker_identifiers(&json_of(&json)),
        vec!["violations.jsonl".to_string()],
        "the marker row must name the ledger that could not be read"
    );
    assert_eq!(json.status.code(), Some(3));
}

#[test]
fn a_readable_violation_ledger_still_lists_its_systemic_row() {
    // ANTI-VACUITY control for the test above. Without this, an implementation
    // that answered "undetermined" for every violation read would satisfy the
    // failure arm while permanently hiding the systemic stream.
    let (home, work) = make_sandbox("violations-readable");
    seed_systemic_violation(&home, &work);

    let out = run_ow_raw(&home, &work, &["review-queue"]);
    let stdout = stdout_of(&out);
    assert!(
        out.status.success(),
        "a fully readable store must exit 0: stderr={}",
        stderr_of(&out)
    );
    assert!(
        stdout.contains("[systemic]") && stdout.contains("blastguard:rm-rf"),
        "the seeded systemic signature must be listed: stdout={stdout:?}"
    );

    let json = run_ow_raw(&home, &work, &["review-queue", "--json"]);
    assert!(
        marker_identifiers(&json_of(&json)).is_empty(),
        "nothing was unreadable, so no marker row may appear"
    );
    assert!(json.status.success());
}

/// The one review-queue failure arm whose control lived in a DIFFERENT test
/// (`review_queue_merges_all_three_sources_time_ordered`, which asserts an
/// `ai-finding` row appears but says nothing about the exit code or about
/// marker rows). `review_queue_review_findings_unreadable_surfaces_warning_
/// not_empty` therefore had no same-shape partner. This is that partner.
#[test]
fn a_readable_findings_ledger_still_lists_its_row_and_exits_zero() {
    let (home, work) = make_sandbox("findings-readable");
    seed_finding(&home, &work, "F-77");

    let out = run_ow_raw(&home, &work, &["review-queue"]);
    let stdout = stdout_of(&out);
    assert!(
        out.status.success(),
        "a readable findings ledger must exit 0: stderr={}",
        stderr_of(&out)
    );
    assert!(
        stdout.contains("[ai-finding]") && stdout.contains("F-77"),
        "the seeded finding must be listed: stdout={stdout:?}"
    );

    let json = run_ow_raw(&home, &work, &["review-queue", "--json"]);
    assert!(
        marker_identifiers(&json_of(&json)).is_empty(),
        "nothing was unreadable, so no marker row may appear"
    );
    assert!(json.status.success());
}

// ===========================================================================
// 2. `--limit` / `--since` must not be able to shed the marker row.
//
//    `review_queue::assemble`'s unit test pins this for `assemble` in
//    ISOLATION. It cannot prove that `run` calls `truncate` BEFORE `assemble`
//    — swap those two lines and the unit test stays green while `--limit`
//    silently drops the statement that the cap may be hiding something. That
//    ordering is only observable from outside the process, which is what
//    these two do.
// ===========================================================================

#[test]
fn limit_zero_cannot_shed_the_undetermined_marker_row() {
    let (home, work) = make_sandbox("limit-marker");
    // A real row exists (so `--limit 0` has something to shed) AND a source is
    // unreadable (so a marker exists).
    seed_merge_conflict(&store_dir(&home, &work), "c-1");
    seed_rollback(&home, &work, "overwatch");
    make_unreadable(&store_dir(&home, &work).join("rollbacks.jsonl"));

    let out = run_ow_raw(&home, &work, &["review-queue", "--json", "--limit", "0"]);
    let body = json_of(&out);
    let rows = body.as_array().unwrap();
    assert_eq!(
        rows.len(),
        1,
        "`--limit 0` sheds every real row but must NOT shed the marker: {rows:?}"
    );
    assert_eq!(rows[0]["kind"], "undetermined-source");
    assert_eq!(rows[0]["identifier"], "rollbacks.jsonl");
    assert_eq!(out.status.code(), Some(3));

    // `--since` in the far future is the other way a caller empties the queue.
    let far_future = format!("{}", i64::from(u32::MAX));
    let out = run_ow_raw(
        &home,
        &work,
        &["review-queue", "--json", "--since", &far_future],
    );
    assert_eq!(
        marker_identifiers(&json_of(&out)),
        vec!["rollbacks.jsonl".to_string()],
        "`--since` must not shed the marker either"
    );
    assert_eq!(out.status.code(), Some(3));
}

#[test]
fn limit_zero_on_a_readable_store_sheds_every_row_and_exits_zero() {
    // ANTI-VACUITY control: the cap must still cap. If the marker row were
    // unconditional furniture, this would be `[]` vs one row and would fail.
    let (home, work) = make_sandbox("limit-control");
    seed_merge_conflict(&store_dir(&home, &work), "c-1");

    let capped = run_ow_raw(&home, &work, &["review-queue", "--json", "--limit", "0"]);
    assert_eq!(
        stdout_of(&capped).trim(),
        "[]",
        "with every source read, `--limit 0` really is an empty array"
    );
    assert!(capped.status.success(), "a readable store exits 0");

    let uncapped = run_ow_raw(&home, &work, &["review-queue", "--json"]);
    assert_eq!(
        json_of(&uncapped).as_array().unwrap().len(),
        1,
        "control on the control: the row the cap shed really did exist"
    );
}

// ===========================================================================
// 3. `review-queue --to-backlog`, END TO END through a fake `backlog`.
//
//    69a90432 explicitly disclosed this gap: "`--to-backlog`'s undetermined
//    paths are covered by UNIT tests of `drain_ledger` and `plan_entry_adds`,
//    not end-to-end through the fake-backlog integration harness."
//
//    The unit test proves `drain_ledger` returns `None`. It does NOT prove
//    that `run_in` then refrains from SPAWNING `backlog add` — that is the
//    consequence a human pays for (duplicate tasks to reconcile by hand), and
//    it is only observable by counting the spawns.
// ===========================================================================

/// A fake `backlog` that appends one line per `add` to `$FAKE_BACKLOG_LOG`.
fn write_fake_backlog(dir: &Path) -> PathBuf {
    fs::create_dir_all(dir).unwrap();
    let script = dir.join("backlog");
    fs::write(
        &script,
        r#"#!/bin/sh
if [ "$1" = "add" ]; then
  shift
  title=""
  while [ $# -gt 0 ]; do
    case "$1" in
      --title) title="$2"; shift 2 ;;
      *) shift ;;
    esac
  done
  printf '%s\n' "$title" >> "$FAKE_BACKLOG_LOG"
fi
exit 0
"#,
    )
    .unwrap();
    let mut perm = fs::metadata(&script).unwrap().permissions();
    perm.set_mode(0o755);
    fs::set_permissions(&script, perm).unwrap();
    script
}

/// Run `review-queue --to-backlog` with the fake backlog wired in via
/// `OVERWATCH_BACKLOG_BIN` (no PATH mutation, so nothing leaks between tests).
fn run_drain(home: &Path, work: &Path, backlog: &Path, log: &Path) -> Output {
    Command::new(overwatch_bin())
        .args(["review-queue", "--to-backlog"])
        .env("HOME", home)
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .env("OVERWATCH_BACKLOG_BIN", backlog)
        .env("FAKE_BACKLOG_LOG", log)
        .current_dir(work)
        .output()
        .expect("failed to spawn overwatch binary")
}

fn add_count(log: &Path) -> usize {
    fs::read_to_string(log)
        .map(|t| t.lines().filter(|l| !l.trim().is_empty()).count())
        .unwrap_or(0)
}

/// `(home, work, backlog script, add log)` with a rollback already recorded.
fn drain_sandbox(tag: &str) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
    let (home, work) = make_sandbox(tag);
    let backlog = write_fake_backlog(&home.join("bin"));
    let log = home.join("backlog-add.log");
    seed_rollback(&home, &work, "specguard");
    (home, work, backlog, log)
}

#[test]
fn to_backlog_with_a_readable_store_bridges_the_row_and_exits_zero() {
    // ANTI-VACUITY control, deliberately FIRST: the two failure arms below
    // both assert "0 adds", which a drain that bridges nothing ever also
    // satisfies. This is the test that says the drain works at all.
    let (home, work, backlog, log) = drain_sandbox("drain-control");

    let out = run_drain(&home, &work, &backlog, &log);
    assert!(
        out.status.success(),
        "a fully readable store must exit 0: stderr={}",
        stderr_of(&out)
    );
    assert_eq!(add_count(&log), 1, "the seeded rollback must be forwarded");
    let body = json_of(&out);
    assert_eq!(body["entries_bridged"], 1);
    assert_eq!(
        body["undetermined_sources"].as_array().unwrap().len(),
        0,
        "every ledger was read, so the list must be empty (present, not absent)"
    );
}

#[test]
fn to_backlog_with_an_unreadable_idempotency_ledger_bridges_nothing() {
    // The idempotency ledger is the OPPOSITE direction from a source: reading
    // it as empty does not hide a row, it re-forwards every row that was
    // already filed. So the whole stream must be SKIPPED — no `backlog add`
    // may be spawned — and that must reach the shell.
    let (home, work, backlog, log) = drain_sandbox("drain-idempotency");
    make_unreadable(&store_dir(&home, &work).join("bridged_entries.jsonl"));

    let out = run_drain(&home, &work, &backlog, &log);
    let stderr = stderr_of(&out);

    assert_eq!(
        add_count(&log),
        0,
        "with the already-bridged set unknown, NOTHING may be forwarded — \
         proceeding would re-file items a human then reconciles by hand"
    );
    assert!(
        stderr.contains("SKIPPED") && stderr.contains("bridged_entries.jsonl"),
        "the skipped stream must be named on stderr: stderr={stderr:?}"
    );
    let body = json_of(&out);
    let named: Vec<&str> = body["undetermined_sources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(
        named.contains(&"bridged_entries.jsonl"),
        "the JSON summary must name the ledger, not just count zero: {body}"
    );
    assert_eq!(
        out.status.code(),
        Some(3),
        "`\"entries_bridged\": 0` is byte-identical to a clean no-op run, so the exit \
         code is the channel that carries the difference; stderr={stderr}"
    );
}

#[test]
fn to_backlog_with_an_undecodable_source_ledger_bridges_nothing_from_it() {
    // The other direction: the SOURCE could not be read. Bridging nothing is
    // safe (append-only ledgers, re-derived every run) but must not be
    // reported as "there was nothing to bridge".
    let (home, work, backlog, log) = drain_sandbox("drain-source");
    append_undecodable_line(&store_dir(&home, &work).join("rollbacks.jsonl"));

    let out = run_drain(&home, &work, &backlog, &log);
    let stderr = stderr_of(&out);

    assert_eq!(
        add_count(&log),
        0,
        "the rollback stream contributed no rows, so no add may be spawned"
    );
    assert!(
        stderr.contains("WARNING") && stderr.contains("rollbacks.jsonl"),
        "stderr must name the source ledger: stderr={stderr:?}"
    );
    assert!(
        stderr.contains("NOT a report that the source is empty"),
        "the warning must refuse the zero reading explicitly: stderr={stderr:?}"
    );
    assert_eq!(out.status.code(), Some(3));

    // The idempotency ledger must NOT have been written for a row that was
    // never forwarded (otherwise a later, readable run would skip it).
    let entries = store_dir(&home, &work).join("bridged_entries.jsonl");
    assert!(
        !entries.exists() || fs::read_to_string(&entries).unwrap().trim().is_empty(),
        "nothing was bridged, so nothing may be recorded as bridged"
    );
}

/// The MIRROR TWIN of the test above. `--to-backlog` keeps TWO idempotency
/// ledgers on purpose (`bridged_findings.jsonl` keyed on bare finding-ids —
/// also the review-metrics "resolved" source — and `bridged_entries.jsonl`
/// keyed on `<kind>:<identifier>`), and they are read by two separate
/// `drain_ledger` calls. This repo's recurring failure mode is fixing one
/// mirror and calling it converged, so the finding half is pinned separately
/// rather than assumed to follow from the entry half.
#[test]
fn to_backlog_with_an_unreadable_finding_idempotency_ledger_bridges_nothing() {
    let (home, work) = make_sandbox("drain-findings-idempotency");
    let backlog = write_fake_backlog(&home.join("bin"));
    let log = home.join("backlog-add.log");
    seed_finding(&home, &work, "CA-e2e-010");

    // Control first, in this same test: the finding really does bridge when
    // the ledger is readable, so the "0 adds" below is not trivially true.
    let control = run_drain(&home, &work, &backlog, &log);
    assert!(control.status.success(), "stderr={}", stderr_of(&control));
    assert_eq!(
        add_count(&log),
        1,
        "the confirmed finding must be forwarded"
    );

    // Now the same store, with the finding-idempotency ledger unreadable and a
    // SECOND finding pending: proceeding with an unknown already-bridged set
    // would re-file CA-e2e-010 as well.
    seed_finding(&home, &work, "CA-e2e-011");
    make_unreadable(&store_dir(&home, &work).join("bridged_findings.jsonl"));

    let out = run_drain(&home, &work, &backlog, &log);
    let stderr = stderr_of(&out);
    assert_eq!(
        add_count(&log),
        1,
        "no further add may be spawned while the already-bridged set is unknown"
    );
    assert!(
        stderr.contains("SKIPPED") && stderr.contains("bridged_findings.jsonl"),
        "the skipped stream must be named: stderr={stderr:?}"
    );
    assert!(
        json_of(&out)["undetermined_sources"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "bridged_findings.jsonl"),
        "the JSON summary must name it: {}",
        stdout_of(&out)
    );
    assert_eq!(out.status.code(), Some(3));
}

// ===========================================================================
// 4. `reconcile-fixed`, END TO END.
//
//    69a90432 disclosed: "`reconcile-fixed` and `review-metrics` were migrated
//    and compile-checked, ... but their undetermined branches were not
//    exercised end-to-end either." `reconcile.rs` additionally carries NO unit
//    test at all for `reconcile_inputs` / `stale_undisposed_count` / the
//    undetermined arm of `run` — every test in that module tests the two pure
//    functions. These are the first tests that execute those branches.
// ===========================================================================

fn git(work: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .env("GIT_AUTHOR_NAME", "test")
        .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
        .env("GIT_COMMITTER_NAME", "test")
        .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .current_dir(work)
        .output()
        .expect("failed to spawn git");
    assert!(
        out.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A sandbox whose work dir is a real git repo holding one commit that
/// references finding `CA-e2e-001`, which is also on the findings ledger.
fn reconcile_sandbox(tag: &str) -> (PathBuf, PathBuf) {
    let (home, work) = make_sandbox(tag);
    git(&work, &["init", "-q"]);
    fs::write(work.join("a.txt"), "a").unwrap();
    git(&work, &["add", "-A"]);
    git(&work, &["commit", "-q", "-m", "fix: resolve CA-e2e-001"]);
    seed_finding(&home, &work, "CA-e2e-001");
    (home, work)
}

#[test]
fn reconcile_fixed_with_readable_ledgers_reconciles_and_exits_zero() {
    // ANTI-VACUITY control, first again: the failure arm below asserts
    // "nothing was reconciled", which a reconcile that never works also
    // satisfies.
    let (home, work) = reconcile_sandbox("reconcile-control");

    let out = run_ow_raw(
        &home,
        &work,
        &["reconcile-fixed", "--last-n", "10", "--json"],
    );
    assert!(
        out.status.success(),
        "readable ledgers must exit 0: stderr={}",
        stderr_of(&out)
    );
    let body = json_of(&out);
    assert_eq!(
        body["reconciled"].as_array().unwrap(),
        &vec![Value::String("CA-e2e-001".into())],
        "the referenced finding must be auto-dispositioned: {body}"
    );
    assert_eq!(body["undetermined_sources"].as_array().unwrap().len(), 0);
    assert!(
        store_dir(&home, &work).join("dispositions.jsonl").is_file(),
        "the disposition must actually have been written"
    );
}

#[test]
fn reconcile_fixed_with_an_unreadable_disposition_ledger_writes_nothing() {
    // Reading the disposition ledger as empty makes every finding look
    // undisposed, so the SAME finding is disposed again — and
    // `append_disposition`'s own dedup cannot catch it, because it re-reads
    // the same unreadable ledger. The run must write nothing and say so.
    let (home, work) = reconcile_sandbox("reconcile-dispositions");
    let ledger = store_dir(&home, &work).join("dispositions.jsonl");
    make_unreadable(&ledger);

    let out = run_ow_raw(
        &home,
        &work,
        &["reconcile-fixed", "--last-n", "10", "--json"],
    );
    let stderr = stderr_of(&out);
    let body = json_of(&out);

    assert_eq!(
        out.status.code(),
        Some(3),
        "`\"reconciled\": []` is byte-identical to a clean no-op run; stderr={stderr}"
    );
    assert!(
        body["reconciled"].as_array().unwrap().is_empty(),
        "nothing may be claimed as reconciled: {body}"
    );
    let named: Vec<&str> = body["undetermined_sources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(
        named.contains(&"dispositions.jsonl"),
        "the JSON must name the ledger it could not read: {body}"
    );
    assert!(
        ledger.is_dir(),
        "the run must not have written over the ledger it could not read"
    );

    // The HUMAN channel must not say the reassuring thing either.
    let human = run_ow_raw(&home, &work, &["reconcile-fixed", "--last-n", "10"]);
    let stdout = stdout_of(&human);
    assert!(
        !stdout.contains("0 finding(s) reconciled"),
        "that sentence reads as 'nothing needed doing'; stdout={stdout:?}"
    );
    assert!(
        stdout.contains("NOTHING was reconciled") && stdout.contains("NOT a report"),
        "the human line must refuse the zero reading: stdout={stdout:?}"
    );
    assert_eq!(human.status.code(), Some(3));
}

#[test]
fn reconcile_fixed_with_an_unreadable_findings_ledger_writes_nothing() {
    // The mirror half of the same join: an empty FINDINGS set makes every
    // referenced id "unknown", so nothing reconciles and the report says
    // "0 reconciled" — a sentence a human reads as "nothing needed doing".
    let (home, work) = reconcile_sandbox("reconcile-findings");
    make_unreadable(&store_dir(&home, &work).join("review_findings.jsonl"));

    let out = run_ow_raw(
        &home,
        &work,
        &["reconcile-fixed", "--last-n", "10", "--json"],
    );
    let body = json_of(&out);
    assert_eq!(out.status.code(), Some(3), "stderr={}", stderr_of(&out));
    assert!(body["reconciled"].as_array().unwrap().is_empty());
    let named: Vec<&str> = body["undetermined_sources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(
        named.contains(&"review_findings.jsonl"),
        "the findings half must be named too, not only the disposition half: {body}"
    );
    assert!(
        !store_dir(&home, &work).join("dispositions.jsonl").exists(),
        "no disposition may be written from a join that could not be made"
    );
}

// ===========================================================================
// 5. `review-metrics`, END TO END. Same disclosed gap.
//
//    A rate computed from a ledger that could not be read is not a low rate,
//    it is no rate — and `0.0` is a number a human acts on.
// ===========================================================================

#[test]
fn review_metrics_with_a_readable_store_reports_numbers_and_exits_zero() {
    // ANTI-VACUITY control: `null` everywhere is not an acceptable steady
    // state, and this is what refuses it.
    let (home, work) = make_sandbox("metrics-control");
    seed_finding(&home, &work, "CA-e2e-002");
    seed_ow(
        &home,
        &work,
        &[
            "record-disposition",
            "--finding-id",
            "CA-e2e-002",
            "--verdict",
            "confirmed",
            "--reviewer",
            "tester",
        ],
    );

    let out = run_ow_raw(&home, &work, &["review-metrics", "--json"]);
    assert!(
        out.status.success(),
        "a readable store must exit 0: stderr={}",
        stderr_of(&out)
    );
    let body = json_of(&out);
    assert_eq!(body["total"], 1, "a measured count, not null: {body}");
    assert_eq!(
        body["stale_undisposed_with_fix_commit"], 0,
        "a measured zero here is real and must stay a number: {body}"
    );
    assert_eq!(body["undetermined_sources"].as_array().unwrap().len(), 0);
}

#[test]
fn review_metrics_with_an_unreadable_disposition_ledger_reports_null_not_zero() {
    let (home, work) = make_sandbox("metrics-dispositions");
    seed_finding(&home, &work, "CA-e2e-003");
    seed_ow(
        &home,
        &work,
        &[
            "record-disposition",
            "--finding-id",
            "CA-e2e-003",
            "--verdict",
            "false-positive",
            "--reviewer",
            "tester",
        ],
    );
    make_unreadable(&store_dir(&home, &work).join("dispositions.jsonl"));

    let out = run_ow_raw(&home, &work, &["review-metrics", "--json"]);
    let body = json_of(&out);

    assert_eq!(out.status.code(), Some(3), "stderr={}", stderr_of(&out));
    for key in [
        "total",
        "false_positive_rate",
        "agreement_rate",
        "median_latency_secs",
        "closure_rate",
        "stale_undisposed_with_fix_commit",
    ] {
        assert!(
            body[key].is_null(),
            "`{key}` must be null, never a number computed from a ledger that could \
             not be read (a `0` false-positive rate is a number a human acts on): {body}"
        );
    }
    let named: Vec<&str> = body["undetermined_sources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(
        named.contains(&"dispositions.jsonl"),
        "the JSON must name the ledger: {body}"
    );

    // The human channel must not print the reassuring "nothing yet" line.
    let human = run_ow_raw(&home, &work, &["review-metrics"]);
    let stdout = stdout_of(&human);
    assert!(
        stdout.contains("UNDETERMINED"),
        "the report must say it is undetermined: stdout={stdout:?}"
    );
    assert!(
        !stdout.contains("no dispositions recorded yet"),
        "that line reads as a measured zero; stdout={stdout:?}"
    );
    assert_eq!(human.status.code(), Some(3));
}

#[test]
fn review_metrics_with_an_unreadable_findings_archive_reports_null_not_zero() {
    // The findings side of the same report is read as hot store PLUS cold
    // archive. `compact_review_findings` moves resolved findings into that
    // archive, so an unreadable ARCHIVE with a perfectly readable hot store is
    // a real state — and the closure rate computed from the half that read is
    // a partial history indistinguishable from a complete one.
    let (home, work) = make_sandbox("metrics-archive");
    seed_finding(&home, &work, "CA-e2e-004");
    let archive = store_dir(&home, &work).join("review_findings_archive.jsonl");
    make_unreadable(&archive);

    let out = run_ow_raw(&home, &work, &["review-metrics", "--json"]);
    let body = json_of(&out);
    assert_eq!(out.status.code(), Some(3), "stderr={}", stderr_of(&out));
    assert!(
        body["closure_rate"].is_null() && body["total"].is_null(),
        "a half-read history must not produce a closure rate: {body}"
    );
    let named: Vec<&str> = body["undetermined_sources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(
        named.contains(&"review_findings.jsonl"),
        "the history half must be named: {body}"
    );
}
