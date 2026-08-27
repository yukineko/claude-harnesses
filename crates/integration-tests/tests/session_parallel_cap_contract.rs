//! Pins the per-session concurrency cap where it lives in *text*.
//!
//! # What this proves, and what it cannot
//!
//! The cap is enforced deterministically wherever a binary owns the decision:
//! `condukt::schedule::schedule` splits parallel batches at its
//! `max_parallel` argument, and `Config::load` clamps that field through
//! `harness_core::parallel::cap_fanout` so config.toml and the environment can
//! only ever LOWER it. Those guarantees are tested in `condukt` itself.
//!
//! But three fan-outs have no binary in the loop at all — an LLM reads a
//! markdown file and issues N `Task` calls in one message:
//!
//! * `/scout` Phase 2 (one read-only agent per lens),
//! * `/overwatch:continuous-audit` Step 1 (one finder per target crate),
//! * `/specguard:run` and `/specguard:spec-audit` (one auditor per shard).
//!
//! For those, the markdown IS the mechanism, and **this test cannot prove an LLM
//! obeys it** — nothing in a text file can. What it does prove is narrower and
//! still worth having: the unbounded instructions this change removed
//! ("1メッセージで並列起動", "並列で同時に起動してよい", `N` defaulting to 4,
//! sample/panel ceilings of 5) cannot silently come back, and the number in the
//! prose cannot drift away from the number in the code — changing
//! `SESSION_MAX_PARALLEL` turns this red and forces the docs to be re-read.
//!
//! Deterministic enforcement for the skill-side fan-outs (a PreToolUse counter
//! on `Task` that denies the 4th live subagent) is NOT implemented; it is filed
//! rather than assumed, so nobody reads this file as proof of a gate that does
//! not exist.

use harness_core::parallel::SESSION_MAX_PARALLEL;
use std::path::PathBuf;

/// The canonical phrase every fan-out doc must carry, so a reader (human or
/// model) meets the cap at the point of the fan-out rather than in a changelog.
const MARKER: &str = "1 セッションあたりの同時実行上限";

fn repo_file(rel: &str) -> (PathBuf, String) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join(rel);
    let body =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    (path, body)
}

/// Every doc that tells an agent to fan out.
const FANOUT_DOCS: &[&str] = &[
    "crates/condukt/skills/condukt/SKILL.md",
    "crates/flow/skills/flow/SKILL.md",
    "crates/scout/skills/scout/SKILL.md",
    "crates/overwatch/skills/continuous-audit/SKILL.md",
    "crates/specguard/commands/run.md",
    "crates/specguard/commands/spec-audit.md",
];

#[test]
fn the_cap_is_three() {
    // Not a tautology: this is the coupling. The prose below hard-codes the
    // digit (it is written for a reader, not interpolated), so if the constant
    // moves, this assertion is what says "go update the six documents".
    assert_eq!(
        SESSION_MAX_PARALLEL, 3,
        "the session cap changed — update the fan-out docs listed in FANOUT_DOCS \
         (they spell the number out) before changing this assertion"
    );
}

#[test]
fn every_fanout_doc_states_the_session_cap() {
    for rel in FANOUT_DOCS {
        let (path, body) = repo_file(rel);
        assert!(
            body.contains(MARKER),
            "{} fans out subagents but never states the cap ({MARKER:?})",
            path.display()
        );
        assert!(
            body.contains(&SESSION_MAX_PARALLEL.to_string()),
            "{} states the cap phrase without the number",
            path.display()
        );
    }
}

#[test]
fn retired_unbounded_fanout_instructions_do_not_come_back() {
    // Each entry is a verbatim instruction that USED to authorize an unbounded
    // (or over-wide) fan-out. Pinning the exact strings is the point: a future
    // edit that restores one of them is exactly the regression to catch.
    let cases: &[(&str, &str)] = &[
        // scout: "launch them in ONE message" with no ceiling — 5 lenses at once.
        (
            "crates/scout/skills/scout/SKILL.md",
            "**1メッセージで並列起動**",
        ),
        // specguard: "you may launch them all concurrently" — one per shard,
        // and the shard count is data-dependent (unbounded in principle).
        (
            "crates/specguard/commands/run.md",
            "**並列で同時に起動してよい**",
        ),
        (
            "crates/specguard/commands/spec-audit.md",
            "**並列で同時に起動してよい**",
        ),
        // flow: the batch width the driver claims, previously defaulting to 4.
        ("crates/flow/skills/flow/SKILL.md", "無指定なら **4**"),
        // condukt: the consensus fan-out ceiling of 5.
        ("crates/condukt/skills/condukt/SKILL.md", "(既定 3・上限 5)"),
        // condukt: the adversarial panel ceiling of 5.
        (
            "crates/condukt/skills/condukt/SKILL.md",
            "2〜5 にクランプ済み",
        ),
    ];
    for (rel, retired) in cases {
        let (path, body) = repo_file(rel);
        assert!(
            !body.contains(retired),
            "{} re-introduced the unbounded fan-out instruction {retired:?}",
            path.display()
        );
    }
}
