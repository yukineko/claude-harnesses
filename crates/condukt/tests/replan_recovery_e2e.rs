// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! End-to-end coverage for the phase-5 replan-recovery loop, exercised
//! through the REAL built `condukt` binary (no mocks, no direct calls into
//! `replan::decide_replan` — only `condukt replan handoff` / `condukt replan
//! stats` subprocess invocations).
//!
//! Proves the "model 昇格では回復不能だが replan なら回復する" claim: a task
//! that keeps failing even after being escalated to the top model tier
//! (`opus`) cannot be rescued by further model escalation — but a `replan`
//! (re-decomposition) directive breaks the deadlock and reopens the path to
//! recovery (F→P). It also proves the three directives
//! (`escalate_model` / `replan` / `escalate_to_user`) are distinguishable in
//! the `condukt replan stats` observability aggregate (phase-5 DoD#3).
//!
//! Modeled after the sibling `fp_oracle_e2e.rs`: `unique_dir` + a
//! `run_condukt` helper spawning `env!("CARGO_BIN_EXE_condukt")` with an
//! isolated `$HOME` so `~/.condukt/state` is never touched by these tests.
//! Unlike `fp_oracle_e2e.rs` this test never shells out to a `tdd` sidecar —
//! `condukt replan handoff`/`stats` are pure classifier CLIs with no external
//! process dependency — so the `extra_path`/`tdd_bin_dir` machinery is
//! dropped entirely.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

fn unique_dir(tag: &str) -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let id = N.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "condukt-replan-e2e-{tag}-{}-{}",
        std::process::id(),
        id
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create isolated dir");
    dir
}

/// Run the real `condukt` binary with `args`, in `dir`, with `home` as its
/// `$HOME` (isolating `~/.condukt/state`). Returns `(exit_code, stdout,
/// stderr)`. No `PATH` manipulation and no sidecar binary is needed: the
/// `replan handoff`/`replan stats` subcommands under test never shell out to
/// anything.
fn run_condukt(dir: &Path, home: &Path, args: &[&str]) -> (i32, String, String) {
    let bin = env!("CARGO_BIN_EXE_condukt");
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("condukt spawns");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Build the `ReplanHandoffInput` JSON body for `replan handoff --file`.
/// Every field the struct accepts is supplied explicitly (no reliance on
/// `#[serde(default)]`) so the fixture stays correct even if defaults change.
fn handoff_input_json(
    reason: &str,
    failed_tests: &str,
    diff: &str,
    model_tier: &str,
    done_criteria: &str,
    task_summary: &str,
    replan_count: usize,
) -> String {
    serde_json::json!({
        "reason": reason,
        "failed_tests": failed_tests,
        "diff": diff,
        "model_tier": model_tier,
        "done_criteria": done_criteria,
        "task_summary": task_summary,
        "replan_count": replan_count,
    })
    .to_string()
}

/// Write `input_json` to `<dir>/handoff-input-<tag>.json` and run
/// `condukt replan handoff --file <path> [--run <run_id>]`, returning the
/// parsed stdout JSON `Value`. Panics on nonzero exit (the handoff CLI is
/// documented to always exit 0 for well-formed input).
fn run_handoff(
    dir: &Path,
    home: &Path,
    tag: &str,
    input_json: &str,
    run_id: Option<&str>,
) -> serde_json::Value {
    let path = dir.join(format!("handoff-input-{tag}.json"));
    std::fs::write(&path, input_json).expect("write handoff input json");
    let mut args = vec!["replan", "handoff", "--file", path.to_str().unwrap()];
    if let Some(rid) = run_id {
        args.push("--run");
        args.push(rid);
    }
    let (code, stdout, stderr) = run_condukt(dir, home, &args);
    assert_eq!(
        code, 0,
        "replan handoff must exit 0\nstdout: {stdout}\nstderr: {stderr}"
    );
    serde_json::from_str::<serde_json::Value>(&stdout).unwrap_or_else(|e| {
        panic!("replan handoff stdout must be valid JSON: {e}\nstdout: {stdout}")
    })
}

/// An ordinary (non-scope-mismatch) implementation-bug-shaped failure: a
/// plain assertion mismatch. Verified against `replan.rs`'s own unit test
/// `assertion_failure_below_top_tier_escalates_model` (same reason text
/// classifies to `EscalateModel` at a below-top-tier `model_tier`) and its
/// doc comment for `SCOPE_MISMATCH_MARKERS`, which explicitly calls out that
/// a bare compiler diagnostic like "mismatched types" is an ordinary bug
/// (case (c)), not a scope mismatch (case (b)) — the marker list only matches
/// COMPOUND phrases such as "scope mismatch" / "done_criteria mismatch",
/// never the bare word "mismatch"/"mismatched" alone. This reason string
/// contains neither the bare word nor any compound marker.
const ORDINARY_BUG_REASON: &str = "assertion `left == right` failed\n  left: 4\n right: 5";
const ORDINARY_BUG_FAILED_TESTS: &str = "test_foo::test_bar";

// ── Assertions 1 & 2: escalate fails to recover; replan does ──────────────

#[test]
fn ordinary_failure_below_top_tier_escalates_model_not_replan() {
    // Assertion 1 (F -> escalate fail): a below-top-tier worker hitting an
    // ordinary implementation bug gets `escalate_model` — the cascade tries
    // a stronger model FIRST, not a replan.
    let dir = unique_dir("escalate");
    let home = unique_dir("escalate-home");

    let input = handoff_input_json(
        ORDINARY_BUG_REASON,
        ORDINARY_BUG_FAILED_TESTS,
        "diff --git a/x b/x",
        "sonnet",
        "done_criteria: fix the off-by-one",
        "fix the counter bug",
        0,
    );
    let v = run_handoff(&dir, &home, "step1", &input, None);

    assert_eq!(
        v["directive"].as_str(),
        Some("escalate_model"),
        "an ordinary bug below top tier must escalate the model, not replan: {v}"
    );
    assert!(
        v.get("handoff").is_none() || v["handoff"].is_null(),
        "escalate_model directive must not carry a handoff: {v}"
    );
}

#[test]
fn same_failure_already_at_top_tier_replans_instead() {
    // Assertion 2 (escalate fail -> replan): the SAME ordinary-shaped failure
    // but now at `model_tier: "opus"` (the top tier — escalation has already
    // been exhausted and still failed) resolves to `replan`, carrying a
    // non-empty re-decomposition instruction. This is the "model escalation
    // cannot recover it, but replan can" pivot.
    let dir = unique_dir("replan-pivot");
    let home = unique_dir("replan-pivot-home");

    let input = handoff_input_json(
        ORDINARY_BUG_REASON,
        ORDINARY_BUG_FAILED_TESTS,
        "diff --git a/x b/x",
        "opus",
        "done_criteria: fix the off-by-one",
        "fix the counter bug",
        0,
    );
    let v = run_handoff(&dir, &home, "step2", &input, None);

    assert_eq!(
        v["directive"].as_str(),
        Some("replan"),
        "the same failure still failing at the top tier must replan: {v}"
    );
    let handoff = &v["handoff"];
    assert!(
        handoff.is_object(),
        "replan directive must carry a handoff object: {v}"
    );
    let instruction = handoff["instruction"]
        .as_str()
        .expect("handoff.instruction must be a string");
    assert!(
        !instruction.trim().is_empty(),
        "handoff.instruction must be a non-empty re-decomposition instruction"
    );
    assert!(
        instruction.to_lowercase().contains("new decomposition")
            || instruction
                .to_lowercase()
                .contains("brand new decomposition"),
        "handoff.instruction must explicitly direct a NEW decomposition: {instruction}"
    );
}

// ── Assertion 3: the replan unblocks the path back to recovery (->P) ──────

#[test]
fn replan_reopens_the_convergent_escalate_path_toward_pass() {
    // Assertion 3 (replan -> P): the heavier proof (re-running the actual
    // `tdd` RED/GREEN oracle after a real re-decomposition) needs the `tdd`
    // sidecar and is already covered structurally by `fp_oracle_e2e.rs`'s
    // proof that a corrected-scope task's worker converges to `verified`
    // via the ordinary escalate/fix loop. What THIS test proves, purely
    // through the real `condukt replan` classifier CLI, is the piece that is
    // actually replan's responsibility: that consuming the replan (assertion
    // 2) does NOT leave the task permanently stuck being replanned forever.
    // Once the interpreter has re-decomposed the task (new approach, new
    // scope, per the handoff instruction) and dispatches a FRESH attempt —
    // starting again from a below-top-tier model (`sonnet`, the baseline
    // tier, since this is a brand-new task shape, not a continuation of the
    // old opus attempt) — an ordinary-bug-shaped failure on THAT fresh
    // attempt is classified `escalate_model`, not `replan` and not
    // `escalate_to_user`, even though `replan_count` (1) has already been
    // incremented by the consumed replan. That is exactly the "->P" leg
    // reopening: the task is back on the normal, convergent
    // escalate-model-then-fix loop (the same loop `fp_oracle_e2e.rs` proves
    // terminates in `verified`) instead of being trapped replanning forever.
    // The replan cap (assertion 4) only ever blocks a REPLAN-classified
    // failure, never an ordinary bug — so replanning once is guaranteed to
    // eventually hand control back to the normal pass-converging path.
    let dir = unique_dir("recovery");
    let home = unique_dir("recovery-home");

    // Step A: reproduce assertion 2's replan pivot to obtain a real
    // handoff.instruction from the binary (not fabricated).
    let escalate_exhausted_input = handoff_input_json(
        ORDINARY_BUG_REASON,
        ORDINARY_BUG_FAILED_TESTS,
        "diff --git a/x b/x",
        "opus",
        "done_criteria: fix the off-by-one",
        "fix the counter bug",
        0,
    );
    let replan_directive = run_handoff(
        &dir,
        &home,
        "recovery-step-a",
        &escalate_exhausted_input,
        None,
    );
    assert_eq!(replan_directive["directive"].as_str(), Some("replan"));
    let instruction = replan_directive["handoff"]["instruction"]
        .as_str()
        .expect("replan handoff must carry an instruction")
        .to_string();
    assert!(
        !instruction.trim().is_empty(),
        "the re-decomposition instruction driving the recovery must be non-empty"
    );

    // Step B: the interpreter has now (per the instruction) produced a NEW
    // decomposition and dispatched a fresh, below-top-tier attempt at it.
    // That fresh attempt hits an ordinary (non-scope-mismatch) bug — a
    // realistic shape for "almost fixed, one more escalate-and-retry cycle
    // needed" — with replan_count carried forward at 1 (one replan already
    // consumed).
    let fresh_attempt_input = handoff_input_json(
        "assertion `left == right` failed\n  left: 1\n right: 2",
        "test_new_approach::test_case",
        "diff --git a/y b/y",
        "sonnet",
        "done_criteria: fix the off-by-one (re-scoped)",
        "fix the counter bug (re-decomposed)",
        1,
    );
    let fresh_directive = run_handoff(&dir, &home, "recovery-step-b", &fresh_attempt_input, None);

    assert_eq!(
        fresh_directive["directive"].as_str(),
        Some("escalate_model"),
        "a fresh, non-scope-mismatch failure after a replan must resume the normal \
         escalate-model-then-fix loop, not be trapped in another replan or escalated to the \
         user: {fresh_directive}"
    );
    assert!(
        fresh_directive.get("handoff").is_none() || fresh_directive["handoff"].is_null(),
        "escalate_model directive must not carry a handoff: {fresh_directive}"
    );
}

// ── Assertion 4: replan cap exhausted fails soft to user escalation ───────

#[test]
fn replan_cap_exhausted_fails_soft_to_user_escalation() {
    let dir = unique_dir("cap-exhausted");
    let home = unique_dir("cap-exhausted-home");

    // Same failure as assertion 2 (opus, still failing -> Replan
    // classification) but replan_count is already at MAX_REPLANS (1), so the
    // cap is exhausted and the cascade must fail-soft to escalate_to_user
    // instead of replanning again.
    let input = handoff_input_json(
        ORDINARY_BUG_REASON,
        ORDINARY_BUG_FAILED_TESTS,
        "diff --git a/x b/x",
        "opus",
        "done_criteria: fix the off-by-one",
        "fix the counter bug",
        1,
    );
    let v = run_handoff(&dir, &home, "step4", &input, None);

    assert_eq!(
        v["directive"].as_str(),
        Some("escalate_to_user"),
        "replan cap exhausted must fail-soft to escalate_to_user: {v}"
    );
    assert!(
        v.get("handoff").is_none() || v["handoff"].is_null(),
        "escalate_to_user directive must not carry a handoff: {v}"
    );
    assert!(
        v.get("user_escalation").is_some() && !v["user_escalation"].is_null(),
        "escalate_to_user directive must carry a user_escalation message: {v}"
    );
}

// ── Assertion 5: the three directives are distinguishable in stats ────────

#[test]
fn replan_stats_distinguish_all_three_directives() {
    let dir = unique_dir("stats");
    let home = unique_dir("stats-home");
    let run_id = "run-replan-stats";

    // escalate_model: ordinary bug, below top tier.
    let escalate_model_input = handoff_input_json(
        ORDINARY_BUG_REASON,
        ORDINARY_BUG_FAILED_TESTS,
        "diff --git a/x b/x",
        "sonnet",
        "done_criteria: fix the off-by-one",
        "fix the counter bug",
        0,
    );
    let d1 = run_handoff(&dir, &home, "stats-1", &escalate_model_input, Some(run_id));
    assert_eq!(d1["directive"].as_str(), Some("escalate_model"));

    // replan: same failure, but already at top tier and within cap.
    let replan_input = handoff_input_json(
        ORDINARY_BUG_REASON,
        ORDINARY_BUG_FAILED_TESTS,
        "diff --git a/x b/x",
        "opus",
        "done_criteria: fix the off-by-one",
        "fix the counter bug",
        0,
    );
    let d2 = run_handoff(&dir, &home, "stats-2", &replan_input, Some(run_id));
    assert_eq!(d2["directive"].as_str(), Some("replan"));

    // escalate_to_user: same failure, at top tier, cap already exhausted.
    let escalate_to_user_input = handoff_input_json(
        ORDINARY_BUG_REASON,
        ORDINARY_BUG_FAILED_TESTS,
        "diff --git a/x b/x",
        "opus",
        "done_criteria: fix the off-by-one",
        "fix the counter bug",
        1,
    );
    let d3 = run_handoff(
        &dir,
        &home,
        "stats-3",
        &escalate_to_user_input,
        Some(run_id),
    );
    assert_eq!(d3["directive"].as_str(), Some("escalate_to_user"));

    // The `stats` call MUST use the same cwd + HOME as the three `handoff
    // --run` calls above: the replan-log record path derives from cwd +
    // config base under HOME.
    let (code, stdout, stderr) = run_condukt(&dir, &home, &["replan", "stats", "--run", run_id]);
    assert_eq!(
        code, 0,
        "replan stats must exit 0\nstdout: {stdout}\nstderr: {stderr}"
    );
    let stats: serde_json::Value = serde_json::from_str(&stdout).unwrap_or_else(|e| {
        panic!("replan stats stdout must be valid JSON: {e}\nstdout: {stdout}")
    });

    assert!(
        stats["escalate_model"].as_u64().unwrap_or(0) >= 1,
        "stats must count at least one escalate_model record: {stats}"
    );
    assert!(
        stats["replan"].as_u64().unwrap_or(0) >= 1,
        "stats must count at least one replan record: {stats}"
    );
    assert!(
        stats["escalate_to_user"].as_u64().unwrap_or(0) >= 1,
        "stats must count at least one escalate_to_user record: {stats}"
    );
}
