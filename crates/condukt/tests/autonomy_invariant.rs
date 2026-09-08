//! Autonomy stop-invariant machine checks (backlog task a66e4adb, DoD#5).
//!
//! Ground truth (established in the condukt SKILL Phase 3 work and restated
//! verbatim in `crates/flow/skills/flow/SKILL.md`):
//!
//!   When autonomy is ON, self-driving proceeds WITHOUT confirmation EXCEPT the
//!   sanctioned stops that remain even when `autonomous = true`:
//!     (a) a condukt **worker blocked** (genuinely stuck),
//!     (b) **pivot** (a genuine strategic judgment), and
//!     (c) the judgment framings — merge-conflict pick-a-side (`--conflict`) and
//!         an untestable decision (`--untestable`, CLAUDE.md §2).
//!   `--dry-run` stops are invariant regardless of autonomy.
//!
//! REVISED 2026-08-07 by explicit user directive. The **deploy/push GATED
//! approval** used to be sanctioned stop (b): a human was asked "may I proceed?"
//! before every outward-facing side effect. The user withdrew that prompt —
//! "flow で実行したときは、今後すべて YES/NO の権限認可は行わず全て許諾したい。
//! blastguard とかで止められない限り。あと設計などで判断をもとめる場合の Ask は
//! うけいれる。" — so the axis that governs a prompt is no longer risk alone but
//! **what kind of question it is**:
//!
//!   - a **permission approval** ("may I proceed?", answer known to be yes) is
//!     pre-granted and self-answers, via the `--approval` framing flag;
//!   - a **judgment request** ("which should it be?", "is this right?") still
//!     asks the human, and deliberately does NOT carry `--approval`.
//!
//! What actually stops a dangerous action is therefore the DETERMINISTIC gate
//! layer — blastguard / donegate / the pre-commit and pre-push
//! hooks — plus the policy engine's own `block` verdict, which `--approval`
//! cannot relax. Section 1d pins those guardrails at the binary boundary; this
//! file is the reason a future edit cannot quietly widen the grant.
//!
//! This file mechanically enforces that invariant from two angles:
//!
//!   1. CODE CONTRACT (the parts that live in Rust). We drive the real `condukt`
//!      binary and assert:
//!        - `condukt state autonomy-check` returns the exact JSON + exit-code
//!          contract that every skill branches on (true/false/missing/env).
//!        - `class:"gated"` tasks are NEVER placed into an auto-run batch by
//!          `condukt schedule` — the code-level backbone of the GATED stop.
//!
//!   2. SKILL AUDIT (the parts that live in SKILL.md, which cargo cannot execute).
//!      We scan every `crates/*/skills/**` and `crates/*/agents/**` markdown file,
//!      count each `AskUserQuestion` occurrence, and assert the live set equals a
//!      frozen ALLOWLIST (see `ASK_ALLOWLIST` for the per-file rationale). Adding a
//!      NEW `AskUserQuestion` prompt anywhere — or deleting an audited one — changes
//!      a count and turns this test RED, forcing a human to re-audit whether the new
//!      prompt is (a) a permission approval that should be pre-granted via
//!      `--approval`, (b) a sanctioned worker-blocked stop, or (c) another genuine
//!      judgment request. We additionally pin the ground-truth prose (the invariant
//!      statement, the autonomy switch, the residual stops, and the `--approval`
//!      framing) so it cannot be silently weakened — or silently widened.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

fn condukt_bin() -> &'static str {
    env!("CARGO_BIN_EXE_condukt")
}

/// Repo root = `<manifest>/../..` (manifest dir is `crates/condukt`).
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crates/condukt has a repo root two levels up")
        .to_path_buf()
}

// ---------------------------------------------------------------------------
// 1. CODE CONTRACT: `condukt state autonomy-check`
// ---------------------------------------------------------------------------
//
// Every skill (condukt/flow/scout) branches on exactly this contract to decide
// whether to skip a human gate. If the contract drifts, autonomy either fails
// open (silently skipping gates when it should not) or fails closed (never
// autonomous). Both break the invariant, so we pin it here.

/// Run `condukt state autonomy-check` with a controlled HOME (so it reads a
/// throwaway `~/.condukt/config.toml`, never the developer's real one) and an
/// explicit `CONDUKT_AUTONOMOUS` env state. Returns (exit_code, trimmed stdout).
fn run_autonomy_check(home: &Path, autonomous_env: Option<&str>) -> (i32, String) {
    let mut cmd = Command::new(condukt_bin());
    cmd.args(["state", "autonomy-check"]).env("HOME", home);
    match autonomous_env {
        Some(v) => {
            cmd.env("CONDUKT_AUTONOMOUS", v);
        }
        None => {
            cmd.env_remove("CONDUKT_AUTONOMOUS");
        }
    }
    let out = cmd.output().expect("condukt binary should run");
    let code = out.status.code().expect("process exits with a code");
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (code, stdout)
}

fn write_config(home: &Path, body: &str) {
    let dir = home.join(".condukt");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("config.toml"), body).unwrap();
}

#[test]
fn autonomy_check_true_config_exits_zero_and_prints_true() {
    let tmp = tempfile::tempdir().unwrap();
    write_config(tmp.path(), "autonomous = true\n");
    let (code, out) = run_autonomy_check(tmp.path(), None);
    assert_eq!(code, 0, "autonomous=true must exit 0 (skip the gate)");
    assert_eq!(out, r#"{"autonomous":true}"#, "exact JSON contract");
}

#[test]
fn autonomy_check_false_config_exits_one_and_prints_false() {
    let tmp = tempfile::tempdir().unwrap();
    write_config(tmp.path(), "autonomous = false\n");
    let (code, out) = run_autonomy_check(tmp.path(), None);
    assert_eq!(code, 1, "autonomous=false must exit 1 (keep the gate)");
    assert_eq!(out, r#"{"autonomous":false}"#);
}

#[test]
fn autonomy_check_missing_config_defaults_to_not_autonomous() {
    // No ~/.condukt at all: the safe default is NON-autonomous (gate stays on).
    let tmp = tempfile::tempdir().unwrap();
    let (code, out) = run_autonomy_check(tmp.path(), None);
    assert_eq!(code, 1, "missing config must fail closed (exit 1)");
    assert_eq!(out, r#"{"autonomous":false}"#);
}

#[test]
fn autonomy_check_env_override_turns_it_on() {
    // Env override with no config file present -> autonomous.
    let tmp = tempfile::tempdir().unwrap();
    let (code, out) = run_autonomy_check(tmp.path(), Some("1"));
    assert_eq!(code, 0);
    assert_eq!(out, r#"{"autonomous":true}"#);
}

#[test]
fn autonomy_check_env_false_overrides_config_true() {
    // Precedence: env is applied AFTER the file, so CONDUKT_AUTONOMOUS=0 wins
    // over `autonomous = true` in config -> gate stays on. This guards against a
    // "sticky autonomy" bug where the switch could not be turned back off.
    let tmp = tempfile::tempdir().unwrap();
    write_config(tmp.path(), "autonomous = true\n");
    let (code, out) = run_autonomy_check(tmp.path(), Some("0"));
    assert_eq!(code, 1, "env=0 must override config true");
    assert_eq!(out, r#"{"autonomous":false}"#);
}

// ---------------------------------------------------------------------------
// 1b. CODE CONTRACT: GATED tasks are never auto-scheduled
// ---------------------------------------------------------------------------
//
// The GATED carve-out has a code backbone that OUTLIVES the 2026-08-07 move of
// its human approval to `--approval`: `condukt schedule` must route every
// `class:"gated"` task into the `gated` list and NEVER into a parallel batch.
// Pre-granting the *approval* did not pre-grant the *execution* — a gated task
// still leaves the batch and still has to clear `condukt gate check`, whose
// blastguard classification is what actually decides. This is the concrete form
// of "止めるのは人間の Yes/No ではなく deterministic gate の側", so if this
// assertion ever goes red the standing grant has widened into something the
// user did not authorize.

#[test]
fn gated_task_is_isolated_and_never_batched() {
    let tmp = tempfile::tempdir().unwrap();
    let dec = r#"{"goal":"g","tasks":[
        {"id":"work","touched_files":["src/a.rs"],"class":"parallel"},
        {"id":"deploy","touched_files":["deploy.sh"],"class":"gated"}
    ]}"#;
    let f = tmp.path().join("dec.json");
    std::fs::write(&f, dec).unwrap();

    let out = Command::new(condukt_bin())
        .args(["schedule", "--file"])
        .arg(&f)
        .env("HOME", tmp.path())
        .output()
        .expect("condukt schedule should run");
    assert!(
        out.status.success(),
        "schedule failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("schedule emits JSON");

    let gated: Vec<&str> = v["gated"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_str().unwrap())
        .collect();
    assert!(
        gated.contains(&"deploy"),
        "gated task must land in the `gated` list; got {gated:?}"
    );

    // The gated task must appear in NO batch (never auto-run under autonomy).
    for batch in v["batches"].as_array().unwrap() {
        for id in batch["parallel"].as_array().unwrap() {
            assert_ne!(
                id.as_str().unwrap(),
                "deploy",
                "a GATED task must never be placed in an auto-run batch"
            );
        }
    }

    // Sanity: the ordinary parallel task IS scheduled, so the check above is not
    // vacuously true because nothing was batched.
    let batched: Vec<&str> = v["batches"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|b| b["parallel"].as_array().unwrap())
        .map(|x| x.as_str().unwrap())
        .collect();
    assert!(
        batched.contains(&"work"),
        "the non-gated task should be batched; got {batched:?}"
    );
}

// ---------------------------------------------------------------------------
// 1c. CODE CONTRACT: `policy answer --untestable` never self-answers
// ---------------------------------------------------------------------------
//
// The §2 "untestable -> must ask a human" gate: an untestable decision may
// ONLY escalate or block (exactly like the existing `--conflict` clamp), so
// even an input that would otherwise self-answer `auto` must fall through to
// a real AskUserQuestion once framed `--untestable`.

/// Run `condukt policy answer` on a fixed trivially-safe-and-reversible input
/// (low risk / high reversibility / high confidence — the case `decide` Autos)
/// with or without `--untestable`. Returns (exit_code, trimmed stdout).
fn run_policy_answer_untestable(dir: &Path, untestable: bool) -> (i32, String) {
    let mut cmd = Command::new(condukt_bin());
    cmd.args([
        "policy",
        "answer",
        "--risk",
        "low",
        "--reversible",
        "high",
        "--confidence",
        "high",
        "--question",
        "Proceed with the untestable step?",
        "--option",
        "yes",
        "--option",
        "no",
        "--recommend",
        "0",
        "--journal-dir",
    ])
    .arg(dir)
    .env("HOME", dir);
    if untestable {
        cmd.arg("--untestable");
    }
    let out = cmd.output().expect("condukt policy answer should run");
    let code = out.status.code().expect("process exits with a code");
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (code, stdout)
}

#[test]
fn policy_answer_without_untestable_self_answers_the_safe_case() {
    // Baseline: absent --untestable, this trivially-safe input self-answers
    // `auto`. Proves the next test isn't vacuous — the flag is what changes
    // this outcome, not the input itself.
    let tmp = tempfile::tempdir().unwrap();
    let (code, out) = run_policy_answer_untestable(tmp.path(), false);
    assert_eq!(code, 0, "baseline must self-answer auto; got {out:?}");
    assert!(out.contains("\"answered\":true"), "got {out:?}");
}

#[test]
fn policy_answer_untestable_clamps_auto_to_escalate_never_self_answers() {
    // The SAME input that self-answers above must fall through to a human
    // once framed --untestable (auto -> escalate), exactly like the existing
    // --conflict clamp: never self-answered, never journaled.
    let tmp = tempfile::tempdir().unwrap();
    let (code, out) = run_policy_answer_untestable(tmp.path(), true);
    assert_eq!(code, 2, "--untestable must escalate (exit 2); got {out:?}");
    assert_eq!(
        out, r#"{"answered":false,"policy":"escalate"}"#,
        "must print the escalate JSON shape, never self-answer"
    );
    assert!(
        !tmp.path().join("gate-decisions.jsonl").exists(),
        "an escalated untestable decision must not be journaled as a self-answer"
    );
}

// ---------------------------------------------------------------------------
// 1d. CODE CONTRACT: `policy answer --approval` self-answers, but only when
//     autonomous, and never relaxes a `block`
// ---------------------------------------------------------------------------
//
// The 2026-08-07 standing grant: a YES/NO permission gate stops prompting while
// self-driving. `--approval` is the ONLY downward clamp in the policy engine, so
// its three guardrails are pinned here at the binary boundary:
//   (i)   autonomous + escalate-shaped input -> self-answers `auto`,
//   (ii)  NON-autonomous -> completely inert, still escalates,
//   (iii) a `block` verdict is never relaxed, autonomous or not,
//   (iv)  --untestable (a raising clamp) beats --approval.
// Each asserts the EXACT stdout JSON, not just the exit code: a clap error on an
// old binary also exits 2, so an exit-code-only assertion would pass vacuously
// against a build that has never heard of `--approval`.

/// Run `condukt policy answer` on a caller-chosen risk/reversibility triple with
/// a controlled HOME and explicit `CONDUKT_AUTONOMOUS`. Returns (exit, stdout).
fn run_policy_answer_approval(
    dir: &Path,
    risk: &str,
    reversible: &str,
    confidence: &str,
    autonomous: bool,
    extra: &[&str],
) -> (i32, String) {
    let mut cmd = Command::new(condukt_bin());
    cmd.args([
        "policy",
        "answer",
        "--risk",
        risk,
        "--reversible",
        reversible,
        "--confidence",
        confidence,
        "--question",
        "Proceed with the deploy?",
        "--option",
        "yes",
        "--option",
        "no",
        "--recommend",
        "0",
        "--journal-dir",
    ])
    .arg(dir)
    .env("HOME", dir)
    .env("CONDUKT_AUTONOMOUS", if autonomous { "1" } else { "0" });
    cmd.args(extra);
    let out = cmd.output().expect("condukt policy answer should run");
    let code = out.status.code().expect("process exits with a code");
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (code, stdout)
}

#[test]
fn policy_answer_baseline_escalates_the_ambiguous_middle() {
    // Non-vacuity anchor: WITHOUT --approval, the ambiguous middle escalates
    // even when autonomous. So the next test's `auto` is caused by the flag,
    // not by the input already being safe.
    let tmp = tempfile::tempdir().unwrap();
    let (code, out) =
        run_policy_answer_approval(tmp.path(), "medium", "medium", "medium", true, &[]);
    assert_eq!(code, 2, "baseline must escalate; got {out:?}");
    assert_eq!(out, r#"{"answered":false,"policy":"escalate"}"#);
}

#[test]
fn policy_answer_approval_self_answers_when_autonomous() {
    // The standing grant in action: the same input that escalates above
    // self-answers once framed as a permission approval.
    let tmp = tempfile::tempdir().unwrap();
    let (code, out) = run_policy_answer_approval(
        tmp.path(),
        "medium",
        "medium",
        "medium",
        true,
        &["--approval"],
    );
    assert_eq!(
        code, 0,
        "--approval must self-answer under autonomy; got {out:?}"
    );
    assert!(
        out.contains(r#""answered":true"#) && out.contains(r#""policy":"auto""#),
        "must print the auto self-answer JSON; got {out:?}"
    );
    // The gate is answered ON THE RECORD, not deleted: every auto-granted
    // approval lands in the audit trail `condukt policy answers` reads.
    assert!(
        tmp.path().join("gate-decisions.jsonl").exists(),
        "an auto-granted approval must be journaled for audit"
    );
}

#[test]
fn policy_answer_approval_is_inert_when_not_autonomous() {
    // A non-autonomous run keeps every consent prompt. This condition lives in
    // the BINARY, so a skill cannot lose its prompt by forgetting to check
    // autonomy first.
    let tmp = tempfile::tempdir().unwrap();
    let (code, out) = run_policy_answer_approval(
        tmp.path(),
        "medium",
        "medium",
        "medium",
        false,
        &["--approval"],
    );
    assert_eq!(
        code, 2,
        "--approval must be inert when not autonomous; got {out:?}"
    );
    assert_eq!(
        out, r#"{"answered":false,"policy":"escalate"}"#,
        "must print the escalate JSON (an exact match also rules out a clap error)"
    );
    assert!(
        !tmp.path().join("gate-decisions.jsonl").exists(),
        "an escalated approval must not be journaled as a self-answer"
    );
}

#[test]
fn policy_answer_approval_never_relaxes_the_irreversible_block() {
    // The one thing the standing grant does NOT cover. High risk + irreversible
    // hard-blocks even when autonomous and explicitly framed as an approval:
    // "stop asking me for permission" is not consent to a catastrophe.
    let tmp = tempfile::tempdir().unwrap();
    let (code, out) =
        run_policy_answer_approval(tmp.path(), "high", "low", "high", true, &["--approval"]);
    assert_eq!(code, 3, "a block must survive --approval; got {out:?}");
    assert_eq!(out, r#"{"answered":false,"policy":"block"}"#);
    assert!(
        !tmp.path().join("gate-decisions.jsonl").exists(),
        "a blocked approval must not be journaled as a self-answer"
    );
}

#[test]
fn policy_answer_untestable_beats_approval() {
    // Precedence: the raising clamps win. §2's "untestable -> ask a human" gate
    // is NOT covered by a standing permission grant, so passing both flags
    // escalates rather than self-answering.
    let tmp = tempfile::tempdir().unwrap();
    let (code, out) = run_policy_answer_approval(
        tmp.path(),
        "low",
        "high",
        "high",
        true,
        &["--approval", "--untestable"],
    );
    assert_eq!(code, 2, "--untestable must beat --approval; got {out:?}");
    assert_eq!(out, r#"{"answered":false,"policy":"escalate"}"#);
}

#[test]
fn policy_answer_conflict_beats_approval() {
    // Same precedence for merge-conflict resolution: an auto pick-a-side is
    // last-writer-wins, and a permission grant does not authorize it.
    let tmp = tempfile::tempdir().unwrap();
    let (code, out) = run_policy_answer_approval(
        tmp.path(),
        "low",
        "high",
        "high",
        true,
        &["--approval", "--conflict"],
    );
    assert_eq!(code, 2, "--conflict must beat --approval; got {out:?}");
    assert_eq!(out, r#"{"answered":false,"policy":"escalate"}"#);
}

// ---------------------------------------------------------------------------
// 2. SKILL AUDIT: freeze the set of `AskUserQuestion` sites
// ---------------------------------------------------------------------------

/// Frozen allowlist: (path relative to `crates/`, exact number of lines that
/// mention `AskUserQuestion`). This is the machine-checked audit result. Every
/// occurrence was reviewed and falls into one of the categories below; the
/// count freeze means a NEW prompt (or a deletion) forces this list — and thus
/// the audit — to be revisited before the test goes green again.
///
/// Category legend used in the per-file notes:
///   HDR   = `allowed-tools:` front-matter (declares the tool, not a prompt).
///   PROSE = invariant text / heading that names the tool but does not prompt.
///   DEGRADE = a prompt the skill routes through `condukt policy answer` under
///             autonomy (task 98be79b2): an `auto` verdict self-answers it with
///             the recommended option (no prompt, journaled to
///             `gate-decisions.jsonl`), while an `escalate` verdict — or the
///             fail-safe fallback (invalid input / an old binary whose missing
///             `answer` subcommand yields clap exit 2) — re-emits the prompt.
///             Under `condukt state autonomy-check` exit 1 (non-autonomous) it
///             fires as before. Invariant-compatible: routine gates auto-answer,
///             genuine-judgment ones escalate.
///   ESCALATE = a gate deliberately routed to the `escalate` verdict (low
///             confidence / higher risk) so it re-Asks even under autonomy — the
///             retained 質疑 channel (e.g. flow `pivot`).
///   BLOCKED = the sanctioned (a) worker-blocked stop (fires under autonomy: OK).
///   GATED   = the sanctioned (b) deploy/push GATED approval (fires: OK).
///   HOTL    = a human-in-the-loop prompt on a manually-invoked / non-autonomy
///             path (compass reorientation, tdd authoring, hypothesis arg,
///             condukt manual `cancel`/resume selection, issue discovery). These
///             live OUTSIDE the scout/condukt/flow self-driving loop, so the
///             two-stop invariant does not govern them; they are pinned here
///             only so a NEW prompt cannot sneak in unaudited.
///
/// condukt SKILL (23): HDR x1 + PROSE x4 (invariant #1 now documents the
///   policy-answer routing contract — auto self-answers / escalate re-Asks /
///   block refuses — spanning several lines, plus the Phase 3 heading) + DEGRADE
///   (Phase 3 agreement routed through `condukt policy answer` with
///   schedule-derived risk/confidence: auto skips the prompt, escalate/fallback
///   re-emits it; the confidence gate rides on it) + BLOCKED x1 (worker
///   `blocked` escalation) + GATED context (conflict-check safety stop x3) +
///   ESCALATE x1 (Phase 6 RUN-POLICY `ask_human` verdict: the deterministic gate
///   defers to a human — naming `AskUserQuestion` as the channel — when there is
///   no trustworthy automated signal; an invariant-compatible escalation of a
///   genuine no-signal decision, NOT a new autonomous self-driving stop) + HOTL
///   (resume x2, issue discovery x2, open_questions x1, manual cancel x1, curate
///   promote x2 — a manual "eval golden 化しますか?" confirmation before writing
///   a verified run into the curate dataset; out-of-loop, not a self-driving stop).
/// flow SKILL (10): HDR x1 + PROSE (Step 0.5 documents the policy-answer routing
///   contract: the autonomy switch plus the exit 0/2/3 branches that name
///   `AskUserQuestion` on escalate/fallback) + DEGRADE (lock gate, 3-failure —
///   auto self-answers, escalate/fallback re-Asks) + ESCALATE (pivot: routed to
///   `escalate` as a genuine strategic-judgment 質疑). flow states the residual-
///   stops invariant literally (see prose pin below).
/// scout SKILL (8): HDR x1 + PROSE x1 (invariant) + heading x1 + DEGRADE (Phase 4
///   selection routed through `condukt policy answer`: auto adopts top-N,
///   escalate/fallback re-emits the multiSelect prompt; plus auto-handoff and
///   the hard-rule prose) — all skipped/answered under autonomy.
/// overwatch SKILL (7): heading x1 + HOTL x5 (pause / resume / reassign / reap /
///   end — each an "HOTL gate: 実行前に AskUserQuestion で確認" before a
///   side-effecting control command) + PROSE x1 (summary line restating that all
///   side-effect commands confirm via AskUserQuestion). overwatch is a manual
///   cross-session control/monitoring surface (its `allowed-tools` has NO Task
///   tool, so it cannot drive the scout/condukt/flow autonomy loop at all); these
///   are out-of-loop human-control prompts, so the two-stop invariant does not
///   govern them — pinned only so a NEW prompt cannot sneak in unaudited.
/// compass SKILL (3): HDR (L5) + PROSE (L17) + HOTL (L52). compass is a human
///   reorientation layer, not part of the autonomy self-driving loop.
/// tdd SKILL (1): HOTL (L39, optional confirmation while authoring a test).
/// hypothesis add SKILL (1): HOTL (L14, prompt for a missing argument).
const ASK_ALLOWLIST: &[(&str, usize)] = &[
    ("compass/skills/compass/SKILL.md", 3),
    ("condukt/skills/condukt/SKILL.md", 23),
    ("flow/skills/flow/SKILL.md", 10),
    ("hypothesis/skills/add/SKILL.md", 1),
    ("overwatch/skills/overwatch/SKILL.md", 7),
    ("scout/skills/scout/SKILL.md", 8),
    ("tdd/skills/tdd/SKILL.md", 1),
];

/// Recursively collect `*.md` files under `dir` into `out`.
fn collect_md(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.flatten() {
        let p = entry.path();
        if p.is_dir() {
            collect_md(&p, out);
        } else if p.extension().is_some_and(|e| e == "md") {
            out.push(p);
        }
    }
}

/// All skill/agent markdown across every crate: `crates/*/skills/**` and
/// `crates/*/agents/**`.
fn all_skill_and_agent_md(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let crates = root.join("crates");
    for entry in std::fs::read_dir(&crates).unwrap().flatten() {
        let cdir = entry.path();
        if !cdir.is_dir() {
            continue;
        }
        for sub in ["skills", "agents"] {
            collect_md(&cdir.join(sub), &mut out);
        }
    }
    out.sort();
    out
}

fn count_asks(content: &str) -> usize {
    content
        .lines()
        .filter(|l| l.contains("AskUserQuestion"))
        .count()
}

/// The core audit: the live set of `AskUserQuestion` sites must exactly equal
/// the frozen allowlist. A new prompt (in any existing or new skill/agent md)
/// or a deleted one breaks this and forces a re-audit.
#[test]
fn askuserquestion_sites_match_frozen_allowlist() {
    let root = repo_root();
    let crates_dir = root.join("crates");

    // Live map: rel-path -> count, for every file that has >=1 occurrence.
    let mut live: BTreeMap<String, usize> = BTreeMap::new();
    for path in all_skill_and_agent_md(&root) {
        let content = std::fs::read_to_string(&path).unwrap();
        let n = count_asks(&content);
        if n > 0 {
            let rel = path
                .strip_prefix(&crates_dir)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            live.insert(rel, n);
        }
    }

    let expected: BTreeMap<String, usize> = ASK_ALLOWLIST
        .iter()
        .map(|(p, n)| ((*p).to_string(), *n))
        .collect();

    // Direction 1: nothing new/unaudited appeared, and no count grew/shrank.
    for (path, n) in &live {
        match expected.get(path) {
            None => panic!(
                "unaudited AskUserQuestion site(s) in `{path}` ({n} occurrence(s)).\n\
                 Every prompt must be one of: DEGRADE (skipped when `condukt state \
                 autonomy-check` exits 0), a sanctioned worker-blocked stop, a \
                 sanctioned deploy/push GATED approval, or an out-of-loop HOTL \
                 prompt. Classify it, then add `(\"{path}\", {n})` to ASK_ALLOWLIST \
                 with a rationale note."
            ),
            Some(exp) => assert_eq!(
                n, exp,
                "AskUserQuestion count changed in `{path}` (allowlist {exp}, found {n}). \
                 Re-audit the new/removed prompt against the two-stop invariant, then \
                 update ASK_ALLOWLIST."
            ),
        }
    }

    // Direction 2: every allowlisted site still exists (deletion also re-audits).
    for (path, exp) in &expected {
        let got = live.get(path).copied().unwrap_or(0);
        assert_eq!(
            got, *exp,
            "allowlisted AskUserQuestion site(s) missing from `{path}` \
             (expected {exp}, found {got}). If this prompt was intentionally \
             removed, update ASK_ALLOWLIST."
        );
    }
}

/// Pin the ground-truth prose so the invariant statement itself cannot be
/// silently deleted or weakened. If any of these anchors disappears, the audit
/// above would still pass on counts, so we assert them explicitly here.
#[test]
fn invariant_prose_anchors_are_present() {
    let root = repo_root();
    let read = |rel: &str| std::fs::read_to_string(root.join("crates").join(rel)).unwrap();

    // (i) The three self-driving loop skills all branch on the SAME switch.
    for rel in [
        "condukt/skills/condukt/SKILL.md",
        "flow/skills/flow/SKILL.md",
        "scout/skills/scout/SKILL.md",
    ] {
        let md = read(rel);
        assert!(
            md.contains("condukt state autonomy-check"),
            "{rel} must reference the `condukt state autonomy-check` switch"
        );
    }

    // (ii) flow states the residual-stops invariant verbatim: worker-blocked and
    // pivot survive autonomy as judgment requests.
    let flow = read("flow/skills/flow/SKILL.md");
    assert!(
        flow.contains("worker blocked"),
        "flow SKILL must name the (a) worker-blocked residual stop"
    );
    assert!(
        flow.contains("pivot"),
        "flow SKILL must name the (b) pivot residual stop (a genuine judgment)"
    );
    // The deploy/push GATED approval MOVED from a residual stop to a pre-granted
    // permission (2026-08-07). flow must still name it — silently dropping the
    // gate from the docs is exactly how a reader would lose track of where the
    // enforcement went — and must say explicitly that `--approval` is what
    // carries it, so the prose cannot drift back into claiming a prompt that no
    // longer fires.
    assert!(
        flow.contains("GATED") && flow.contains("deploy/push"),
        "flow SKILL must still name the deploy/push GATED gate"
    );
    assert!(
        flow.contains("--approval"),
        "flow SKILL must document the `--approval` permission framing that \
         replaced the deploy/push GATED prompt"
    );
    // The one thing the standing grant does NOT cover must be stated where a
    // reader of the skill will see it.
    assert!(
        flow.contains("block") && flow.contains("常設許諾"),
        "flow SKILL must state that the standing grant does not relax `block`"
    );

    // (iii) condukt keeps the worker-blocked escalation AND the GATED carve-out.
    let condukt = read("condukt/skills/condukt/SKILL.md");
    assert!(
        condukt
            .lines()
            .any(|l| l.contains("blocked") && l.contains("AskUserQuestion")),
        "condukt SKILL must keep the worker-`blocked` -> AskUserQuestion escalation"
    );
    assert!(
        condukt.contains("gated") && condukt.contains("deploy"),
        "condukt SKILL must keep the deploy/gated approval carve-out"
    );
    // Phase 3 consent is a permission approval and must carry the framing flag;
    // if this string vanishes the skill has silently gone back to prompting (or,
    // worse, to auto-answering without declaring why).
    assert!(
        condukt.contains("--approval"),
        "condukt SKILL must route the Phase 3 consent gate through `--approval`"
    );
    // The --dry-run stop is invariant regardless of autonomy.
    assert!(
        condukt.contains("--dry-run"),
        "condukt SKILL must keep the --dry-run stop (invariant under autonomy)"
    );
}
