// このファイルは丸ごと integration test なので unwrap/expect を許可する
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Pins the fix for the zero-turn sub-agent silent-drop defect in
//! `harness_core::usage::subagent_usage`.
//!
//! A sub-agent that was launched but produced no turn (e.g. a worker that
//! died immediately, or whose file is empty/unparseable) must still appear
//! in the output with `turns: 0` — "launched but produced nothing" must be
//! distinguishable from "does not exist". Before the fix, `subagent_usage`
//! silently `continue`d past any `agent-<id>.jsonl` whose aggregate had zero
//! turns, collapsing that state into the same output as a session with no
//! sub-agents at all.

use harness_core::usage::subagent_usage;
use harness_core::verdict::Determination;

fn known<T>(d: Determination<T>) -> T {
    match d {
        Determination::Known(v) => v,
        Determination::Undetermined(why) => {
            panic!("expected Known, got Undetermined({why:?})")
        }
    }
}

/// (a) A zero-turn agent file (present but empty) must still surface as an
/// entry with `turns: 0`, not be dropped.
#[test]
fn zero_turn_agent_file_is_not_dropped() {
    let base = std::env::temp_dir().join(format!(
        "harness-core-subusage-zeroturn-{}",
        std::process::id()
    ));
    let stem = "sess";
    let sub_dir = base.join(stem).join("subagents");
    std::fs::create_dir_all(&sub_dir).unwrap();
    let main_path = base.join(format!("{stem}.jsonl"));
    std::fs::write(&main_path, "{\"type\":\"user\",\"message\":{}}\n").unwrap();

    // Agent that was launched but produced no assistant turn: the file
    // exists but contains no `type: assistant` line with usage (e.g. it
    // died before emitting a single completed turn).
    std::fs::write(
        sub_dir.join("agent-dead000.jsonl"),
        "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"go\"}}\n",
    )
    .unwrap();
    std::fs::write(
        sub_dir.join("agent-dead000.meta.json"),
        r#"{"agentType":"condukt:condukt-worker","description":"died immediately"}"#,
    )
    .unwrap();

    let subs = known(subagent_usage(main_path.to_str().unwrap()));
    assert_eq!(
        subs.len(),
        1,
        "a launched-but-turnless sub-agent must still appear as one entry, got {subs:?}"
    );
    let dead = &subs[0];
    assert_eq!(dead.agent_id, "dead000");
    assert_eq!(dead.turns, 0);
    assert_eq!(dead.description.as_deref(), Some("died immediately"));

    let _ = std::fs::remove_dir_all(&base);
}

/// (b) A `subagents/` directory that genuinely has no agent files at all
/// must still yield a legitimately empty list — the fix must not turn every
/// scan into a fabricated non-empty result.
#[test]
fn subagents_dir_with_no_files_is_empty() {
    let base = std::env::temp_dir().join(format!(
        "harness-core-subusage-nofiles-{}",
        std::process::id()
    ));
    let stem = "sess";
    let sub_dir = base.join(stem).join("subagents");
    std::fs::create_dir_all(&sub_dir).unwrap();
    let main_path = base.join(format!("{stem}.jsonl"));
    std::fs::write(&main_path, "{\"type\":\"user\",\"message\":{}}\n").unwrap();

    let subs = known(subagent_usage(main_path.to_str().unwrap()));
    assert!(
        subs.is_empty(),
        "an empty subagents/ dir must yield an empty list, not fabricate entries: {subs:?}"
    );

    let _ = std::fs::remove_dir_all(&base);
}
