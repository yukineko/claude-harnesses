// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Spec for `condukt policy answer --resolve`, encoding the user ruling
//! 2026-11-21 verbatim:
//!
//!   「破壊はdeny,未測定はreject,autoで自答はいいが解決できないときは別の問いかcancel, 基本はすべてAllow」
//!
//! In AUTONOMOUS mode with `--resolve` the command NEVER exits 2 (escalate):
//! - destructive (`--risk high --reversible low`, any confidence, even with
//!   `--approval`)          -> exit 3, `"policy":"deny"`, `"answered":false`
//! - `--untestable` / `--unmeasured` (not destructive) -> exit 4, `"policy":"reject"`
//! - `--conflict`           -> exit 5, `"policy":"cancel"`
//! - everything else        -> exit 0, `"policy":"auto"`, the recommended option
//! - every --resolve outcome is journaled with `"resolve":true`.
//!
//! NON-autonomous: --resolve is inert. Without --resolve: legacy unchanged.
//!
//! HOME and CONDUKT_AUTONOMOUS are sandboxed per call (as in
//! autonomy_invariant.rs); the journal goes to a tempdir.

use std::path::Path;
use std::process::Command;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_condukt")
}

struct Run {
    code: i32,
    stdout: String,
    json: Option<serde_json::Value>,
}

/// `condukt policy answer` with options A,B, the given levels, `--recommend`,
/// HOME/cwd/journal sandboxed to `dir`, and explicit autonomy.
fn answer(
    dir: &Path,
    risk: &str,
    reversible: &str,
    confidence: &str,
    recommend: &str,
    autonomous: bool,
    extra: &[&str],
) -> Run {
    let mut cmd = Command::new(bin());
    harness_core::git_env::scrub(&mut cmd);
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
        "Which way?",
        "--option",
        "A",
        "--option",
        "B",
        "--recommend",
        recommend,
        "--journal-dir",
    ])
    .arg(dir)
    .args(extra)
    .current_dir(dir)
    .env("HOME", dir)
    .env("CONDUKT_AUTONOMOUS", if autonomous { "1" } else { "0" })
    .env_remove("CLAUDE_CODE_SESSION_ID");
    let out = cmd.output().expect("spawn condukt");
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let json = serde_json::from_str(&stdout).ok();
    Run {
        code: out.status.code().expect("exit code"),
        stdout: format!(
            "{stdout} | stderr={}",
            String::from_utf8_lossy(&out.stderr).trim()
        ),
        json,
    }
}

fn auto_resolve(
    risk: &str,
    reversible: &str,
    confidence: &str,
    recommend: &str,
    extra: &[&str],
) -> (Run, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let mut args = vec!["--resolve"];
    args.extend_from_slice(extra);
    let r = answer(
        dir.path(),
        risk,
        reversible,
        confidence,
        recommend,
        true,
        &args,
    );
    (r, dir)
}

fn json(r: &Run) -> &serde_json::Value {
    r.json
        .as_ref()
        .unwrap_or_else(|| panic!("stdout must be one JSON object: {}", r.stdout))
}

fn journal_rows(dir: &Path) -> Vec<serde_json::Value> {
    let path = dir.join("gate-decisions.jsonl");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("a --resolve outcome must be journaled at {path:?}: {e}"));
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("bad journal line {l:?}: {e}")))
        .collect()
}

fn assert_deny(r: &Run) {
    assert_eq!(r.code, 3, "destructive must deny (exit 3): {}", r.stdout);
    let v = json(r);
    assert_eq!(v["policy"], "deny", "{}", r.stdout);
    assert_eq!(v["answered"], false, "{}", r.stdout);
}

fn assert_reject(r: &Run) {
    assert_eq!(
        r.code, 4,
        "unmeasured/untestable must reject (exit 4): {}",
        r.stdout
    );
    let v = json(r);
    assert_eq!(v["policy"], "reject", "{}", r.stdout);
    assert_ne!(
        v["answered"], true,
        "a reject is never a self-answer: {}",
        r.stdout
    );
}

fn assert_cancel(r: &Run) {
    assert_eq!(r.code, 5, "conflict must cancel (exit 5): {}", r.stdout);
    let v = json(r);
    assert_eq!(v["policy"], "cancel", "{}", r.stdout);
    assert_ne!(
        v["answered"], true,
        "a cancel is never a self-answer: {}",
        r.stdout
    );
}

fn assert_auto(r: &Run, chosen: &str) {
    assert_eq!(r.code, 0, "must self-answer auto (exit 0): {}", r.stdout);
    let v = json(r);
    assert_eq!(v["policy"], "auto", "{}", r.stdout);
    assert_eq!(v["answered"], true, "{}", r.stdout);
    assert_eq!(v["chosen"], chosen, "{}", r.stdout);
}

// --- 破壊は deny -----------------------------------------------------------

#[test]
fn resolve_destructive_denies_at_every_confidence() {
    for conf in ["low", "medium", "high"] {
        let (r, _d) = auto_resolve("high", "low", conf, "0", &[]);
        assert_deny(&r);
    }
}

#[test]
fn resolve_destructive_denies_even_with_approval() {
    for conf in ["low", "high"] {
        let (r, _d) = auto_resolve("high", "low", conf, "0", &["--approval"]);
        assert_deny(&r);
    }
}

// --- 未測定は reject -------------------------------------------------------

#[test]
fn resolve_untestable_rejects_on_safe_and_middle_inputs() {
    for (risk, rev, conf) in [
        ("low", "high", "high"),
        ("medium", "high", "low"),
        ("medium", "medium", "medium"),
    ] {
        let (r, _d) = auto_resolve(risk, rev, conf, "0", &["--untestable"]);
        assert_reject(&r);
    }
}

#[test]
fn resolve_untestable_rejects_even_with_approval() {
    let (r, _d) = auto_resolve("low", "high", "high", "0", &["--untestable", "--approval"]);
    assert_reject(&r);
}

#[test]
fn resolve_unmeasured_rejects() {
    for (risk, rev, conf) in [("low", "high", "high"), ("medium", "high", "low")] {
        let (r, _d) = auto_resolve(risk, rev, conf, "0", &["--unmeasured"]);
        assert_reject(&r);
    }
}

#[test]
fn resolve_reject_is_journaled_with_resolve_true() {
    let (r, d) = auto_resolve("medium", "high", "low", "0", &["--untestable"]);
    assert_reject(&r);
    let rows = journal_rows(d.path());
    let last = rows.last().expect("at least one journal row");
    assert_eq!(last["resolve"], true, "{last}");
    assert_eq!(last["policy"], "reject", "{last}");
}

// --- 解決できないときは cancel ---------------------------------------------

#[test]
fn resolve_conflict_cancels() {
    for (risk, rev, conf) in [("low", "high", "high"), ("medium", "high", "low")] {
        let (r, _d) = auto_resolve(risk, rev, conf, "0", &["--conflict"]);
        assert_cancel(&r);
    }
}

// --- 基本はすべて Allow ----------------------------------------------------

#[test]
fn resolve_ordinary_escalate_case_self_answers_recommended() {
    let (r, _d) = auto_resolve("medium", "high", "low", "1", &[]);
    assert_auto(&r, "B");
}

#[test]
fn resolve_ambiguous_middle_self_answers_recommended() {
    let (r, _d) = auto_resolve("medium", "medium", "medium", "0", &[]);
    assert_auto(&r, "A");
}

#[test]
fn resolve_trivially_auto_case_self_answers_recommended() {
    let (r, _d) = auto_resolve("low", "high", "high", "1", &[]);
    assert_auto(&r, "B");
}

#[test]
fn resolve_with_approval_self_answers_recommended() {
    let (r, _d) = auto_resolve("medium", "high", "low", "1", &["--approval"]);
    assert_auto(&r, "B");
}

#[test]
fn resolve_auto_is_journaled_with_resolve_true() {
    let (r, d) = auto_resolve("medium", "high", "low", "1", &[]);
    assert_auto(&r, "B");
    let rows = journal_rows(d.path());
    let last = rows.last().expect("at least one journal row");
    assert_eq!(last["resolve"], true, "{last}");
    assert_eq!(last["policy"], "auto", "{last}");
    assert_eq!(last["chosen"], "B", "{last}");
}

#[test]
fn resolve_never_exits_2_when_autonomous() {
    let levels = ["low", "medium", "high"];
    let flag_sets: &[&[&str]] = &[
        &[],
        &["--approval"],
        &["--untestable"],
        &["--unmeasured"],
        &["--conflict"],
    ];
    for risk in levels {
        for rev in levels {
            for conf in levels {
                for flags in flag_sets {
                    let (r, _d) = auto_resolve(risk, rev, conf, "0", flags);
                    assert!(
                        matches!(r.code, 0 | 3 | 4 | 5),
                        "--resolve must never escalate (r={risk} v={rev} c={conf} {flags:?}): \
                         exit={} {}",
                        r.code,
                        r.stdout
                    );
                    let p = &json(&r)["policy"];
                    assert!(
                        ["auto", "deny", "reject", "cancel"].iter().any(|w| p == *w),
                        "{}",
                        r.stdout
                    );
                }
            }
        }
    }
}

// --- NON-autonomous: --resolve is inert ------------------------------------

#[test]
fn resolve_is_inert_when_not_autonomous_on_the_escalate_case() {
    let plain_dir = tempfile::tempdir().unwrap();
    let flag_dir = tempfile::tempdir().unwrap();
    let plain = answer(plain_dir.path(), "medium", "high", "low", "1", false, &[]);
    let with = answer(
        flag_dir.path(),
        "medium",
        "high",
        "low",
        "1",
        false,
        &["--resolve"],
    );
    assert_eq!(
        plain.code, 2,
        "control: non-autonomous escalates: {}",
        plain.stdout
    );
    assert_eq!(with.code, 2, "--resolve must be inert: {}", with.stdout);
    assert_eq!(json(&with)["policy"], "escalate", "{}", with.stdout);
    assert_eq!(json(&with)["answered"], false, "{}", with.stdout);
    assert_eq!(
        json(&with),
        json(&plain),
        "non-autonomous --resolve must print exactly what the legacy path prints"
    );
}

#[test]
fn resolve_is_inert_when_not_autonomous_on_flagged_cases() {
    let cases: &[(&str, &str, &str, &[&str])] = &[
        ("low", "high", "high", &["--untestable"]),
        ("low", "high", "high", &["--conflict"]),
        ("high", "low", "high", &[]),
    ];
    for (risk, rev, conf, flags) in cases {
        let plain_dir = tempfile::tempdir().unwrap();
        let flag_dir = tempfile::tempdir().unwrap();
        let plain = answer(plain_dir.path(), risk, rev, conf, "0", false, flags);
        let mut with_flags = vec!["--resolve"];
        with_flags.extend_from_slice(flags);
        let with = answer(flag_dir.path(), risk, rev, conf, "0", false, &with_flags);
        assert!(
            plain.json.is_some(),
            "control must print JSON: {}",
            plain.stdout
        );
        assert_eq!(
            with.code, plain.code,
            "{flags:?}: {} vs {}",
            with.stdout, plain.stdout
        );
        assert_eq!(json(&with), json(&plain), "{flags:?}");
    }
}

// --- Without --resolve: legacy behaviour unchanged (control) ---------------

#[test]
fn without_resolve_autonomous_escalate_case_still_exits_2() {
    let dir = tempfile::tempdir().unwrap();
    let r = answer(dir.path(), "medium", "high", "low", "1", true, &[]);
    assert_eq!(
        r.code, 2,
        "legacy autonomous path must escalate: {}",
        r.stdout
    );
    assert_eq!(json(&r)["policy"], "escalate", "{}", r.stdout);
    assert_eq!(json(&r)["answered"], false, "{}", r.stdout);
}
