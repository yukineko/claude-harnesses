// 統合テスト: テスト内の unwrap/expect/panic は意図的な assert（production 側は workspace の deny のまま）。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Claim-reproduction test for the overwatch hooks.json routing, split out of
//! `cli_contract.rs` (file-growth cap). Claim "rd": `overwatch status` runs
//! only at SessionStart; every other route only runs the `presence` recorder
//! (incl. SubagentStop and PostToolUse `Agent|Task`, backlog afd18dc0).

use serde_json::{json, Value};
use std::path::Path;

#[test]
fn claim_rd_hooks_json_status_only_at_session_start() {
    let hooks: Value = serde_json::from_str(
        &std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("hooks/hooks.json"))
            .unwrap(),
    )
    .unwrap();
    let events = hooks["hooks"].as_object().unwrap();
    // The named property: `overwatch status` runs ONLY at SessionStart (the
    // Stop status hook was dropped in 0.2.105). Every other route runs only
    // the `presence` recorder. SubagentStop and the PostToolUse `Agent|Task`
    // group exist since backlog afd18dc0 (subagent registry RECORD side).
    let mut names: Vec<&String> = events.keys().collect();
    names.sort();
    assert_eq!(
        names,
        ["PostToolUse", "SessionEnd", "SessionStart", "SubagentStop"]
    );
    let status = json!("${CLAUDE_PLUGIN_ROOT}/bin/overwatch status");
    let presence = json!("${CLAUDE_PLUGIN_ROOT}/bin/overwatch presence");
    // (event, matcher of each group (None = no matcher), expected commands of
    // that group in order). Exact: no extra group, no extra command.
    type Group<'a> = (Option<&'a str>, Vec<&'a Value>);
    let expected: [(&str, Vec<Group>); 4] = [
        ("SessionStart", vec![(None, vec![&presence, &status])]),
        (
            "PostToolUse",
            vec![
                (Some("Edit|Write|MultiEdit|NotebookEdit"), vec![&presence]),
                (Some("Agent|Task"), vec![&presence]),
            ],
        ),
        ("SubagentStop", vec![(None, vec![&presence])]),
        ("SessionEnd", vec![(None, vec![&presence])]),
    ];
    let mut status_events = vec![];
    for (ev, want_groups) in expected {
        let groups = events[ev].as_array().unwrap();
        assert_eq!(groups.len(), want_groups.len(), "{ev}");
        for (g, (matcher, want)) in groups.iter().zip(want_groups) {
            assert_eq!(g.get("matcher").and_then(Value::as_str), matcher, "{ev}");
            let hs = g["hooks"].as_array().unwrap();
            assert_eq!(hs.len(), want.len(), "{ev}");
            for (h, w) in hs.iter().zip(want) {
                assert_eq!(h["type"], json!("command"));
                assert_eq!(&h["command"], w, "{ev}");
                assert_eq!(h["timeout"], json!(5));
                if h["command"] == status {
                    status_events.push(ev);
                }
            }
        }
    }
    assert_eq!(
        status_events,
        ["SessionStart"],
        "status only at SessionStart"
    );
}
