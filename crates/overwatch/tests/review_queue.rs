// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Integration test for the unified review surface (`overwatch review-queue`).
//!
//! Seeds all three sources end-to-end through the REAL CLI + store round trip —
//! a systemic gate violation (recorded across distinct tasks so it escalates),
//! a canary rollback event, and an AI-review finding — then runs
//! `review-queue --json` and asserts every seeded source-type appears in the
//! merged output, time-ordered newest-first.
//!
//! Both the store path (`$HOME/.overwatch/<project-key>/overwatch/`) and the
//! project key (derived from the cwd) are sandboxed via a temp HOME + temp cwd,
//! so nothing real is touched and the test is hermetic.

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

fn make_sandbox(tag: &str) -> (PathBuf, PathBuf) {
    let n = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
    let base = std::env::temp_dir().join(format!(
        "overwatch-rq-test-{tag}-{}-{n}",
        std::process::id()
    ));
    let home = base.join("home");
    let work = base.join("work");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&work).unwrap();
    (home, work)
}

/// Path to the built overwatch binary (cargo sets CARGO_BIN_EXE_<name>).
fn overwatch_bin() -> &'static str {
    env!("CARGO_BIN_EXE_overwatch")
}

/// Run the overwatch binary with a sandboxed HOME + cwd, returning stdout.
fn run_ow(home: &Path, work: &Path, args: &[&str]) -> String {
    let out = Command::new(overwatch_bin())
        .args(args)
        .env("HOME", home)
        // Ensure session id is deterministic-ish / not read from the ambient env.
        .env("CLAUDE_CODE_SESSION_ID", "")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .current_dir(work)
        .output()
        .expect("failed to spawn overwatch binary");
    assert!(
        out.status.success(),
        "overwatch {:?} exited non-zero: stderr={}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("overwatch stdout not utf8")
}

#[test]
fn review_queue_merges_all_three_sources_time_ordered() {
    let (home, work) = make_sandbox("all-three");

    // --- Source 1: a SYSTEMIC violation ------------------------------------
    // Record the same blastguard signature across three DISTINCT tasks so the
    // item-B recurrence path escalates it to systemic (default threshold=3,
    // spanning >1 task). Recorded via the real CLI, not hand-crafted JSONL.
    for task in ["task-a", "task-b", "task-c"] {
        run_ow(
            &home,
            &work,
            &[
                "record-violation",
                "--source",
                "blastguard",
                "--discriminator",
                "rm-rf",
                "--task",
                task,
                "--session",
                task, // distinct session per task too
            ],
        );
    }

    // --- Source 2: a canary ROLLBACK event ---------------------------------
    run_ow(
        &home,
        &work,
        &[
            "record-rollback",
            "--plugin",
            "overwatch",
            "--from-version",
            "0.1.7",
            "--to-version",
            "0.1.8",
            "--stage",
            "1",
            "--reason",
            "systemic",
        ],
    );

    // --- Source 3: an AI-review FINDING ------------------------------------
    run_ow(
        &home,
        &work,
        &[
            "record-finding",
            "--finding-id",
            "F-001",
            "--source",
            "reviewgate",
            "--severity",
            "high",
            "--summary",
            "unchecked unwrap on user input",
            "--file",
            "src/foo.rs",
        ],
    );

    // --- Run review-queue --json -------------------------------------------
    let stdout = run_ow(&home, &work, &["review-queue", "--json"]);
    let arr: Value = serde_json::from_str(&stdout).expect("review-queue --json must be parseable");
    let rows = arr
        .as_array()
        .expect("review-queue --json must be an array");

    // Every seeded source-type must appear, discriminated by `kind`.
    let kinds: Vec<&str> = rows
        .iter()
        .map(|r| r["kind"].as_str().expect("each row has a string kind"))
        .collect();
    assert!(
        kinds.contains(&"systemic"),
        "systemic violation missing from queue: {kinds:?}"
    );
    assert!(
        kinds.contains(&"rollback"),
        "rollback event missing from queue: {kinds:?}"
    );
    assert!(
        kinds.contains(&"ai-finding"),
        "ai-finding missing from queue: {kinds:?}"
    );

    // Time-ordered newest-first: timestamps must be non-increasing.
    let ts: Vec<i64> = rows
        .iter()
        .map(|r| r["ts"].as_i64().expect("each row has an integer ts"))
        .collect();
    for w in ts.windows(2) {
        assert!(
            w[0] >= w[1],
            "review-queue must be ordered newest-first; got {ts:?}"
        );
    }

    // The key identifiers surface on their respective rows.
    let systemic_row = rows.iter().find(|r| r["kind"] == "systemic").unwrap();
    assert_eq!(systemic_row["identifier"], "blastguard:rm-rf");
    let rollback_row = rows.iter().find(|r| r["kind"] == "rollback").unwrap();
    assert_eq!(rollback_row["identifier"], "overwatch");
    let finding_row = rows.iter().find(|r| r["kind"] == "ai-finding").unwrap();
    assert_eq!(finding_row["identifier"], "F-001");
}

#[test]
fn review_queue_collapses_a_refound_finding_to_one_row() {
    // The Continuous-Audit loop re-records a still-confirmed finding every round
    // with the SAME finding-id. Through the real store round-trip, review-queue
    // must surface ONE row (the newest), not one row per round.
    let (home, work) = make_sandbox("dedup");

    // Round 1 and round 2 record the same id F-9; the round-2 summary is revised.
    run_ow(
        &home,
        &work,
        &[
            "record-finding",
            "--finding-id",
            "F-9",
            "--source",
            "continuous-audit",
            "--severity",
            "med",
            "--summary",
            "round-1 wording",
        ],
    );
    run_ow(
        &home,
        &work,
        &[
            "record-finding",
            "--finding-id",
            "F-9",
            "--source",
            "continuous-audit",
            "--severity",
            "high",
            "--summary",
            "round-2 revised wording",
        ],
    );
    // A DIFFERENT id must remain its own row.
    run_ow(
        &home,
        &work,
        &[
            "record-finding",
            "--finding-id",
            "F-10",
            "--source",
            "continuous-audit",
            "--summary",
            "a distinct finding",
        ],
    );

    let stdout = run_ow(&home, &work, &["review-queue", "--json"]);
    let arr: Value = serde_json::from_str(&stdout).expect("parseable");
    let rows = arr.as_array().unwrap();

    let ai: Vec<&Value> = rows.iter().filter(|r| r["kind"] == "ai-finding").collect();
    // F-9 (twice) collapses to one; F-10 stays → exactly two ai-finding rows.
    assert_eq!(
        ai.len(),
        2,
        "re-recorded finding must collapse; distinct id stays: {ai:?}"
    );
    let f9: Vec<&&Value> = ai.iter().filter(|r| r["identifier"] == "F-9").collect();
    assert_eq!(f9.len(), 1, "F-9 recorded twice must be ONE row");
    // The surfaced F-9 row reflects the newest (round-2) record.
    assert!(
        f9[0]["summary"]
            .as_str()
            .unwrap()
            .contains("round-2 revised wording"),
        "the newest record must win: {}",
        f9[0]["summary"]
    );
    assert!(
        ai.iter().any(|r| r["identifier"] == "F-10"),
        "distinct id F-10 must still appear"
    );
}

#[test]
fn review_queue_degrades_gracefully_when_a_source_is_empty() {
    // Only seed two of the three sources (NO ai-finding recorded — the arm with
    // no producer wired in production). The command must still succeed and
    // surface the two present sources rather than erroring on the missing one.
    let (home, work) = make_sandbox("degraded");

    for task in ["t1", "t2", "t3"] {
        run_ow(
            &home,
            &work,
            &[
                "record-violation",
                "--source",
                "propguard",
                "--discriminator",
                "PROP-009",
                "--task",
                task,
                "--session",
                task,
            ],
        );
    }
    run_ow(
        &home,
        &work,
        &[
            "record-rollback",
            "--plugin",
            "backlog",
            "--to-version",
            "0.2.0",
            "--reason",
            "raw",
        ],
    );

    let stdout = run_ow(&home, &work, &["review-queue", "--json"]);
    let arr: Value = serde_json::from_str(&stdout).expect("parseable even with an empty source");
    let rows = arr.as_array().unwrap();

    let kinds: Vec<&str> = rows.iter().map(|r| r["kind"].as_str().unwrap()).collect();
    assert!(
        kinds.contains(&"systemic"),
        "systemic must still appear: {kinds:?}"
    );
    assert!(
        kinds.contains(&"rollback"),
        "rollback must still appear: {kinds:?}"
    );
    // The absent AI-findings source contributes nothing — it must NOT appear,
    // and must NOT have errored the whole command (we got valid JSON above).
    assert!(
        !kinds.contains(&"ai-finding"),
        "no finding was recorded, so none should appear"
    );
}

// --- t3: the OTHER sources must not render as "nothing here" either ---------
//
// `review_queue_review_findings_unreadable_surfaces_warning_not_empty` (below)
// pinned this property for ONE source. The tests in this section pin it for the
// remaining ones (rollbacks, condukt escalations, blocked merges) and for the
// `--json` surface, which had no way at all to express "a source could not be
// read". Each failure arm is PAIRED with a control asserting the opposite
// polarity — a readable source still produces its row, and a genuinely empty
// store still says the queue is empty — so an implementation that answers
// "undetermined" for everything does not satisfy them.

/// Run the binary in a sandbox and return the whole `Output` (status included),
/// unlike [`run_ow`] which asserts success and returns stdout only.
fn run_ow_raw(home: &Path, work: &Path, args: &[&str]) -> std::process::Output {
    Command::new(overwatch_bin())
        .args(args)
        .env("HOME", home)
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .current_dir(work)
        .output()
        .expect("failed to spawn overwatch binary")
}

/// The overwatch store dir for this sandbox (`$HOME/.overwatch/<project-key>/
/// overwatch/`), derived with the SAME `harness_core` symbols `store.rs` uses.
fn store_dir(home: &Path, work: &Path) -> PathBuf {
    let repo_root = harness_core::projkey::repo_root(work);
    let project_key = harness_core::projkey::project_key(&repo_root);
    home.join(".overwatch").join(project_key).join("overwatch")
}

/// condukt's default escalation-queue path for this sandbox (foreign read).
fn escalations_path(home: &Path, work: &Path) -> PathBuf {
    let repo_root = harness_core::projkey::repo_root(work);
    let project_key = harness_core::projkey::project_key(&repo_root);
    home.join(".condukt")
        .join("state")
        .join(project_key)
        .join("escalations.json")
}

/// Make an existing path PRESENT but unreadable-as-a-file, root-proof (a
/// directory at the file's path — `chmod 0` does not stop root).
fn make_unreadable(path: &Path) {
    if path.exists() {
        std::fs::remove_file(path).expect("remove the valid file");
    }
    std::fs::create_dir_all(path).expect("replace it with a directory");
}

fn seed_rollback(home: &Path, work: &Path, plugin: &str) {
    run_ow(
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

#[test]
fn unreadable_rollback_ledger_is_not_rendered_as_no_rollbacks() {
    // A canary rollback WAS recorded, then the ledger became unreadable. The
    // queue must not answer "no rollbacks" / "review queue empty": the shipped
    // regression it names is exactly what this surface exists to show.
    let (home, work) = make_sandbox("rollback-unreadable");
    seed_rollback(&home, &work, "overwatch");
    make_unreadable(&store_dir(&home, &work).join("rollbacks.jsonl"));

    let out = run_ow_raw(&home, &work, &["review-queue"]);
    let stdout = String::from_utf8(out.stdout).unwrap();
    let stderr = String::from_utf8(out.stderr).unwrap();

    assert!(
        stderr.contains("WARNING") && stderr.contains("rollback"),
        "an unreadable rollback ledger must be announced; stderr={stderr:?}"
    );
    assert!(
        !stdout.contains("review queue empty"),
        "an UNDETERMINED rollback source must never be reported as an empty queue; \
         stdout={stdout:?}"
    );
    assert!(
        !stdout.contains("no rollbacks"),
        "the prose must not name rollbacks as absent when the ledger could not be \
         read; stdout={stdout:?}"
    );
}

#[test]
fn a_readable_rollback_ledger_still_lists_its_row() {
    // ANTI-VACUITY control for the test above: the normal path must keep
    // working (an implementation that answers "undetermined" for every read
    // would satisfy the failure arm while blinding the queue).
    let (home, work) = make_sandbox("rollback-readable");
    seed_rollback(&home, &work, "overwatch");

    let out = run_ow_raw(&home, &work, &["review-queue"]);
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(
        out.status.success(),
        "a fully readable store must exit 0: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains("[rollback]") && stdout.contains("overwatch"),
        "the seeded rollback row must be listed: stdout={stdout:?}"
    );

    let json = run_ow_raw(&home, &work, &["review-queue", "--json"]);
    let arr: Value = serde_json::from_str(&String::from_utf8(json.stdout).unwrap()).unwrap();
    let kinds: Vec<&str> = arr
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["kind"].as_str().unwrap())
        .collect();
    assert!(kinds.contains(&"rollback"), "kinds={kinds:?}");
    assert!(
        !kinds.contains(&"undetermined-source"),
        "nothing was unreadable, so no undetermined row may appear: {kinds:?}"
    );
    assert!(json.status.success(), "a readable store must exit 0");
}

#[test]
fn a_truly_empty_queue_still_says_empty_and_exits_zero() {
    // ANTI-VACUITY control: a store where every source was READ and held
    // nothing is a real, trustworthy empty — it must keep saying so.
    let (home, work) = make_sandbox("truly-empty");

    let out = run_ow_raw(&home, &work, &["review-queue"]);
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(out.status.success(), "a clean empty store must exit 0");
    assert!(
        stdout.contains("review queue empty"),
        "a genuinely empty queue must still report itself empty: stdout={stdout:?}"
    );

    let json = run_ow_raw(&home, &work, &["review-queue", "--json"]);
    assert!(json.status.success());
    assert_eq!(
        String::from_utf8(json.stdout).unwrap().trim(),
        "[]",
        "a genuinely empty queue is still the bare empty array"
    );
}

#[test]
fn unreadable_rollback_ledger_is_distinguishable_from_an_empty_queue_in_json() {
    // The `--json` consumer is a SCRIPT: stderr warnings and human prose do not
    // reach it. If "could not read the ledger" and "the queue is empty" are the
    // same bytes on stdout AND the same exit code, that consumer cannot tell a
    // vanished rollback from a clean fleet.
    let (broken_home, broken_work) = make_sandbox("json-undetermined");
    seed_rollback(&broken_home, &broken_work, "overwatch");
    make_unreadable(&store_dir(&broken_home, &broken_work).join("rollbacks.jsonl"));
    let broken = run_ow_raw(&broken_home, &broken_work, &["review-queue", "--json"]);
    let broken_stdout = String::from_utf8(broken.stdout).unwrap();

    let (clean_home, clean_work) = make_sandbox("json-empty");
    let clean = run_ow_raw(&clean_home, &clean_work, &["review-queue", "--json"]);
    let clean_stdout = String::from_utf8(clean.stdout).unwrap();

    assert_eq!(
        clean_stdout.trim(),
        "[]",
        "control: a truly empty queue is the bare empty array"
    );
    assert!(clean.status.success(), "control: an empty queue exits 0");

    assert_ne!(
        broken_stdout.trim(),
        clean_stdout.trim(),
        "an unreadable source must not print the same JSON as an empty queue"
    );
    let arr: Value = serde_json::from_str(&broken_stdout).expect("still parseable JSON");
    let rows = arr.as_array().expect("still an array of rows");
    let undetermined: Vec<&Value> = rows
        .iter()
        .filter(|r| r["kind"] == "undetermined-source")
        .collect();
    assert_eq!(
        undetermined.len(),
        1,
        "the unreadable source must appear IN BAND so `length == 0` can never read \
         as clean: {rows:?}"
    );
    assert!(
        undetermined[0]["identifier"]
            .as_str()
            .unwrap()
            .contains("rollbacks"),
        "the row must name the ledger that could not be read: {undetermined:?}"
    );
    assert_eq!(
        broken.status.code(),
        Some(3),
        "a kind-filtering consumer skips the in-band row, so the exit code must \
         also say the queue is incomplete"
    );
}

#[test]
fn unparseable_escalation_queue_is_not_rendered_as_nobody_asking() {
    // An escalation is a HUMAN QUESTION a run is blocked on. A queue we could
    // not parse must not render as "nobody is asking".
    let (home, work) = make_sandbox("escalations-unparseable");
    let path = escalations_path(&home, &work);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "not json at all {{{").unwrap();

    let out = run_ow_raw(&home, &work, &["review-queue"]);
    let stdout = String::from_utf8(out.stdout).unwrap();
    let stderr = String::from_utf8(out.stderr).unwrap();

    assert!(
        stderr.contains("WARNING") && stderr.contains("escalation"),
        "an unparseable escalation queue must be announced; stderr={stderr:?}"
    );
    assert!(
        !stdout.contains("review queue empty"),
        "an UNDETERMINED escalation source must never be reported as an empty \
         queue; stdout={stdout:?}"
    );
    assert!(
        !stdout.contains("escalations,"),
        "the prose must not name escalations as absent when the queue could not \
         be parsed; stdout={stdout:?}"
    );
}

#[test]
fn a_readable_escalation_queue_still_lists_its_row() {
    // ANTI-VACUITY control for the escalation arm.
    let (home, work) = make_sandbox("escalations-readable");
    let path = escalations_path(&home, &work);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        &path,
        r#"{"escalations":[{"id":"esc-1","run":"runA","task":"t1",
           "question":"Which approach?","created_at":500,"resolved":false}]}"#,
    )
    .unwrap();

    let out = run_ow_raw(&home, &work, &["review-queue"]);
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(out.status.success(), "a readable queue must exit 0");
    assert!(
        stdout.contains("[escalation]") && stdout.contains("Which approach?"),
        "the open escalation must be listed: stdout={stdout:?}"
    );
}

#[test]
fn undecodable_merge_conflict_ledger_is_not_rendered_as_no_blocked_merges() {
    // A blocked merge is a work stoppage. A ledger holding a line we could not
    // decode is a PARTIAL view; it must not be rendered as "nothing is blocked".
    let (home, work) = make_sandbox("merge-conflicts-undecodable");
    let dir = store_dir(&home, &work);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("merge_conflicts.jsonl"), "{not json at all\n").unwrap();

    let out = run_ow_raw(&home, &work, &["review-queue"]);
    let stdout = String::from_utf8(out.stdout).unwrap();
    let stderr = String::from_utf8(out.stderr).unwrap();

    assert!(
        stderr.contains("WARNING") && stderr.contains("merge"),
        "an undecodable merge-conflict ledger must be announced; stderr={stderr:?}"
    );
    assert!(
        !stdout.contains("review queue empty"),
        "an UNDETERMINED merge-conflict source must never be reported as an empty \
         queue; stdout={stdout:?}"
    );
    assert!(
        !stdout.contains("merge conflicts)"),
        "the prose must not name merge conflicts as absent when the ledger could \
         not be read; stdout={stdout:?}"
    );
}

/// One valid `merge_conflicts.jsonl` line (the shape `append_merge_conflict`
/// writes), seeded directly because only condukt produces this ledger.
fn seed_merge_conflict(dir: &Path, id: &str) {
    std::fs::create_dir_all(dir).unwrap();
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
    std::fs::write(dir.join("merge_conflicts.jsonl"), format!("{line}\n")).unwrap();
}

#[test]
fn a_readable_merge_conflict_ledger_still_lists_its_row() {
    // ANTI-VACUITY control for the merge-conflict arm.
    let (home, work) = make_sandbox("merge-conflicts-readable");
    seed_merge_conflict(&store_dir(&home, &work), "c-1");

    let out = run_ow_raw(&home, &work, &["review-queue"]);
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(out.status.success(), "a readable ledger must exit 0");
    assert!(
        stdout.contains("[merge-conflict]") && stdout.contains("c-1"),
        "the open blocked merge must be listed: stdout={stdout:?}"
    );
}

#[test]
fn unreadable_resolution_ledger_keeps_the_conflict_open_and_says_so() {
    // DIRECTION JUDGEMENT: an unreadable RESOLUTION ledger falls to "nothing is
    // resolved", which SHOWS more, not fewer, blocked merges — the conservative
    // side, so the source is NOT withheld. But the join is then not trustworthy
    // (an already-resolved conflict may be shown again), and that must be said
    // rather than passed off as a clean read.
    let (home, work) = make_sandbox("resolutions-unreadable");
    let dir = store_dir(&home, &work);
    seed_merge_conflict(&dir, "c-2");
    make_unreadable(&dir.join("merge_conflict_resolutions.jsonl"));

    let out = run_ow_raw(&home, &work, &["review-queue"]);
    let stdout = String::from_utf8(out.stdout).unwrap();
    let stderr = String::from_utf8(out.stderr).unwrap();

    assert!(
        stdout.contains("[merge-conflict]") && stdout.contains("c-2"),
        "the conflict must still be shown (conservative direction): stdout={stdout:?}"
    );
    assert!(
        stderr.contains("WARNING") && stderr.contains("resolution"),
        "the unreadable resolution ledger must be announced; stderr={stderr:?}"
    );
}

/// Recursively find a file named `name` under `root`, or `None`.
fn find_file(root: &Path, name: &str) -> Option<PathBuf> {
    for entry in std::fs::read_dir(root).ok()?.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(found) = find_file(&path, name) {
                return Some(found);
            }
        } else if path.file_name().map(|f| f == name).unwrap_or(false) {
            return Some(path);
        }
    }
    None
}

#[test]
fn review_queue_review_findings_unreadable_surfaces_warning_not_empty() {
    // A CONFIRMED adversarial-review finding was recorded (a real producer
    // wrote review_findings.jsonl), but the file is then made present-but-
    // UNREADABLE (replaced by a directory at the same path — root-proof,
    // unlike chmod 0). The review queue must NOT silently collapse this to
    // "no findings" / "review queue empty": that would be a confirmed finding
    // vanishing from the queue with no trace. It must instead surface a
    // WARNING and refuse to claim the queue is empty.
    let (home, work) = make_sandbox("findings-unreadable");

    run_ow(
        &home,
        &work,
        &[
            "record-finding",
            "--finding-id",
            "F-77",
            "--source",
            "reviewgate",
            "--severity",
            "high",
            "--summary",
            "a confirmed adversarial finding that must not silently vanish",
        ],
    );

    let findings_path = find_file(&home, "review_findings.jsonl")
        .expect("record-finding must have created review_findings.jsonl under HOME");
    std::fs::remove_file(&findings_path).expect("remove the valid file");
    std::fs::create_dir(&findings_path)
        .expect("replace it with a directory (present, unreadable-as-file)");

    let out = Command::new(overwatch_bin())
        .args(["review-queue"])
        .env("HOME", &home)
        .env("CLAUDE_CODE_SESSION_ID", "")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .current_dir(&work)
        .output()
        .expect("failed to spawn overwatch binary");
    let stdout = String::from_utf8(out.stdout).expect("stdout not utf8");
    let stderr = String::from_utf8(out.stderr).expect("stderr not utf8");

    // REVERSED in t3, deliberately. This assertion used to read:
    //
    //     assert!(out.status.success(),
    //         "review-queue must still exit 0 even when a source is undetermined");
    //
    // which fixed as the CONTRACT the very thing the rest of this test objects
    // to: the command told the shell "ran fine, nothing to see" about a queue
    // it could not assemble. The warning it does print goes to stderr, which a
    // `--json` consumer never reads, and the row it now prints in band is
    // skipped by any consumer that filters on `kind`. So exit 3 (the
    // "this is an answer, not a crash" code `canary-gate` already uses) is the
    // one channel that reaches them, and this test now pins THAT instead.
    // Everything else it asserted is unchanged.
    assert_eq!(
        out.status.code(),
        Some(3),
        "an undetermined source must reach the shell as a non-zero exit, not the \
         exit 0 a script reads as 'nothing to see': stderr={stderr}"
    );

    assert!(
        stderr.contains("WARNING") && stderr.contains("review-findings"),
        "expected a WARNING that the review-findings source could not be read; got stderr={stderr:?}"
    );
    assert!(
        !stdout.contains("review queue empty"),
        "an UNDETERMINED review-findings source must never be reported as an empty queue \
         (a confirmed finding could be the very thing that failed to read back); \
         got stdout={stdout:?}"
    );
}
