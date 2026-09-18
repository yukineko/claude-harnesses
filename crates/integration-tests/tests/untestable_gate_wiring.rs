//! Pins the §2 "cannot test it ⇒ ask a human" gate as *wired* in every skill
//! that routes human gates through `condukt policy answer`.
//!
//! # What this proves, and what it does not
//!
//! The mechanism has existed since condukt 0.7.108: `policy::decide_untestable`
//! plus the `--untestable` flag clamp `auto → escalate`, and
//! `crates/condukt/tests/autonomy_invariant.rs` checks that at the binary
//! boundary (`policy_answer_untestable_clamps_auto_to_escalate_never_self_answers`,
//! `policy_answer_untestable_beats_approval`). What was missing was the *wiring*:
//! `/flow` declared the gate, `/condukt` and `/scout` did not, so an untestable
//! decision reached in either of those skills would be framed as an ordinary
//! `policy answer` call, land on `decide`, and be **self-answered** — exactly
//! the CLAUDE.md §2 hole ("テスト不能なので判断で通した") the flag exists to close.
//! Backlog `44d5af11`.
//!
//! `SKILL.md` is a prompt, not enforcement, so — like
//! `flow_skill_queue_contract.rs` next door — **this test cannot prove an LLM
//! reading it will behave correctly.** What it does prove is narrower and still
//! worth having: the declaration cannot silently disappear again, and the gate
//! cannot be quietly down-clamped by pairing it with `--approval`.

use std::path::PathBuf;

/// Every skill that routes human gates through `condukt policy answer`.
/// `(crate dir, skill dir)` — the path is `crates/<crate>/skills/<skill>/SKILL.md`.
const GATED_SKILLS: &[(&str, &str)] =
    &[("flow", "flow"), ("condukt", "condukt"), ("scout", "scout")];

fn skill(crate_dir: &str, skill_dir: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join(crate_dir)
        .join("skills")
        .join(skill_dir)
        .join("SKILL.md");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn lines_with(md: &str, needle: &str) -> Vec<String> {
    md.lines()
        .filter(|l| l.contains(needle))
        .map(|l| l.trim().to_string())
        .collect()
}

/// Every gated skill must actually name the flag. A skill that talks about
/// `policy answer` but never mentions `--untestable` has no wiring at all.
#[test]
fn every_gated_skill_wires_the_untestable_flag() {
    for (c, s) in GATED_SKILLS {
        let md = skill(c, s);
        assert!(
            md.contains("--untestable"),
            "crates/{c}/skills/{s}/SKILL.md routes gates through `condukt policy \
             answer` but never mentions --untestable: an untestable decision \
             there would fall through to `decide` and be self-answered (CLAUDE.md §2)"
        );
    }
}

/// The flag must not be a bare token: the skill has to say WHICH situation it
/// is for, in the vocabulary CLAUDE.md §2 uses, or a reader cannot tell when to
/// reach for it.
#[test]
fn every_gated_skill_names_the_rule_the_flag_implements() {
    for (c, s) in GATED_SKILLS {
        let md = skill(c, s);
        assert!(
            md.contains("測れない") || md.contains("テストが書けない") || md.contains("テスト不能"),
            "crates/{c}/skills/{s}/SKILL.md names --untestable but never states the \
             §2 situation it is for (測れない / テストが書けない / テスト不能)"
        );
    }
}

/// The §2 gate is a *judgement* gate, never a permission gate. `--approval` is
/// the one downward clamp (`escalate → auto`), so pairing the two on a single
/// invocation is precisely the self-answer §2 forbids. The binary already
/// refuses to let `--approval` win (`policy_answer_untestable_beats_approval`),
/// but a skill that instructs the pairing is still teaching the wrong thing.
#[test]
fn the_untestable_gate_is_never_paired_with_approval() {
    for (c, s) in GATED_SKILLS {
        let md = skill(c, s);
        for line in lines_with(&md, "--untestable") {
            assert!(
                !line.contains("--approval"),
                "crates/{c}/skills/{s}/SKILL.md pairs the §2 judgement gate with the \
                 permission-gate clamp on one line: {line}"
            );
        }
    }
}

/// Control. Without this, the three assertions above would all pass against a
/// SKILL.md that had been emptied to a stub — `contains` is satisfied by any
/// text. Anchor them to the file still being the gate-routing skill it claims.
#[test]
fn control_gated_skills_still_route_through_policy_answer() {
    for (c, s) in GATED_SKILLS {
        let md = skill(c, s);
        assert!(
            md.contains("condukt policy answer"),
            "control: crates/{c}/skills/{s}/SKILL.md no longer routes gates through \
             `condukt policy answer`; the assertions above are measuring nothing"
        );
        assert!(
            md.contains("escalate"),
            "control: crates/{c}/skills/{s}/SKILL.md no longer describes the escalate \
             verdict the untestable clamp resolves to"
        );
    }
}
