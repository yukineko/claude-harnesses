#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! backlog 60118c20: the continuous-audit SKILL's model-diversity rule
//! hardcodes stale `claude-3-5-*` model IDs instead of stating the rule as
//! "a different model than the finder". The current family has no 3-5 ids, so
//! the rule as written cannot be followed.

const SKILL_MD: &str = include_str!("../skills/continuous-audit/SKILL.md");

#[test]
#[ignore = "backlog 60118c20: open defect, remove ignore when fixed"]
fn continuous_audit_skill_does_not_hardcode_claude_3_5_model_ids() {
    let hits: Vec<(usize, &str)> = SKILL_MD
        .lines()
        .enumerate()
        .filter(|(_, l)| l.contains("claude-3-5-"))
        .map(|(i, l)| (i + 1, l.trim()))
        .collect();
    assert!(
        hits.is_empty(),
        "continuous-audit SKILL.md still names stale claude-3-5-* model ids: {hits:?}"
    );
}
