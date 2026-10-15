#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Phase 0-next in SKILL.md must pick work with `backlog next` (which only hands out
//! rows that carry a reproduced repro and fails closed when the store cannot load),
//! never `backlog list --status pending` (which shows repro-less rows and, wrapped in
//! `2>/dev/null || true`, turns a store-load error into an empty backlog).

fn skill_md() -> String {
    let path = std::env::var("CONDUKT_SKILL_MD").unwrap_or_else(|_| {
        concat!(env!("CARGO_MANIFEST_DIR"), "/skills/condukt/SKILL.md").to_string()
    });
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {path}: {e}"))
}

#[test]
fn phase0_next_picks_work_with_backlog_next_and_keeps_errors() {
    let text = skill_md();
    let lines: Vec<&str> = text.lines().collect();
    let marker = lines
        .iter()
        .position(|l| l.contains("# 1. バックログを確認"))
        .expect("Phase 0-next marker line '# 1. バックログを確認' not found in SKILL.md");

    let start = (0..marker)
        .rev()
        .find(|&i| lines[i].trim_start().starts_with("```"))
        .expect("no opening bash fence before the Phase 0-next marker");
    let end = (marker + 1..lines.len())
        .find(|&i| lines[i].trim_start().starts_with("```"))
        .expect("no closing fence after the Phase 0-next marker");
    let block: Vec<&str> = lines[start + 1..end].to_vec();

    assert!(
        block.iter().any(|l| l.contains("backlog next")),
        "Phase 0-next block must invoke `backlog next`; block was:\n{}",
        block.join("\n")
    );

    let list_lines: Vec<&&str> = block
        .iter()
        .filter(|l| l.contains("backlog list"))
        .collect();
    assert!(
        list_lines.is_empty(),
        "Phase 0-next block must not invoke `backlog list` (shows repro-less rows); offending lines: {list_lines:?}"
    );

    let backlog_lines: Vec<&&str> = block.iter().filter(|l| l.contains("backlog ")).collect();
    for l in &backlog_lines {
        assert!(
            !l.contains("2>/dev/null"),
            "backlog line must not discard stderr (`2>/dev/null`): {l}"
        );
        assert!(
            !l.contains("|| true"),
            "backlog line must not swallow failure (`|| true`): {l}"
        );
    }
}
