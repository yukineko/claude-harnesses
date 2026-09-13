// このファイルは丸ごと integration test なので unwrap/expect を許可する
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Pins the fix for the zero-turn sub-agent silent-drop defect in
//! `harness_core::usage::subagent_usage`.
//!
//! A sub-agent that was launched but produced no turn — its
//! `agent-<id>.jsonl` is present but carries no `type: assistant` line with
//! a `usage` block, whether because the file holds only non-assistant lines,
//! is genuinely 0 bytes, or holds only unparseable lines — must still appear
//! in the output with `turns: 0`. "Launched but produced nothing" must be
//! distinguishable from "does not exist". Before the fix, `subagent_usage`
//! silently `continue`d past any `agent-<id>.jsonl` whose aggregate had zero
//! turns, collapsing every one of those cases into the same output as a
//! session with no sub-agents at all. Each case below is pinned separately
//! because a fix can special-case one shape (e.g. "skip only 0-byte files")
//! without covering the others — see `zero_byte_agent_file_is_not_dropped`.

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

/// (a) A zero-turn agent file (present, non-empty, but with no
/// `type: assistant` / `usage` line — e.g. it only ever logged the initial
/// user turn before dying) must still surface as an entry with `turns: 0`,
/// not be dropped.
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

/// (c) A genuinely 0-byte `agent-<id>.jsonl` — a sub-agent launched whose
/// transcript file was created but never got a single byte written before it
/// died — must ALSO surface as an entry with `turns: 0`, not be dropped.
/// This is a distinct code path from (a): a fix that skips only empty files
/// (`text.trim().is_empty()`) would pass (a) while still dropping this case,
/// so this must be pinned on its own rather than folded into (a).
#[test]
fn zero_byte_agent_file_is_not_dropped() {
    let base = std::env::temp_dir().join(format!(
        "harness-core-subusage-zerobyte-{}",
        std::process::id()
    ));
    let stem = "sess";
    let sub_dir = base.join(stem).join("subagents");
    std::fs::create_dir_all(&sub_dir).unwrap();
    let main_path = base.join(format!("{stem}.jsonl"));
    std::fs::write(&main_path, "{\"type\":\"user\",\"message\":{}}\n").unwrap();

    // Genuinely empty file: 0 bytes, not even a trailing newline.
    std::fs::write(sub_dir.join("agent-empty00.jsonl"), "").unwrap();
    std::fs::write(
        sub_dir.join("agent-empty00.meta.json"),
        r#"{"agentType":"condukt:condukt-worker","description":"never wrote a byte"}"#,
    )
    .unwrap();

    let subs = known(subagent_usage(main_path.to_str().unwrap()));
    assert_eq!(
        subs.len(),
        1,
        "a launched sub-agent whose transcript is 0 bytes must still appear \
         as one entry, got {subs:?}"
    );
    let empty = &subs[0];
    assert_eq!(empty.agent_id, "empty00");
    assert_eq!(empty.turns, 0);
    assert_eq!(empty.description.as_deref(), Some("never wrote a byte"));

    let _ = std::fs::remove_dir_all(&base);
}

/// (d) An `agent-<id>.jsonl` that has bytes but none of them parse as JSON
/// lines must likewise surface as `turns: 0`, not be dropped — a per-line
/// parse failure inside `ingest` is skipped, it does not abort the file.
#[test]
fn unparseable_agent_file_is_not_dropped() {
    let base = std::env::temp_dir().join(format!(
        "harness-core-subusage-unparseable-{}",
        std::process::id()
    ));
    let stem = "sess";
    let sub_dir = base.join(stem).join("subagents");
    std::fs::create_dir_all(&sub_dir).unwrap();
    let main_path = base.join(format!("{stem}.jsonl"));
    std::fs::write(&main_path, "{\"type\":\"user\",\"message\":{}}\n").unwrap();

    // Not valid JSON at all (e.g. a truncated write mid-line).
    std::fs::write(sub_dir.join("agent-junk0000.jsonl"), "not json at all\n").unwrap();

    let subs = known(subagent_usage(main_path.to_str().unwrap()));
    assert_eq!(
        subs.len(),
        1,
        "a sub-agent file with only unparseable lines must still appear as \
         one entry, got {subs:?}"
    );
    let junk = &subs[0];
    assert_eq!(junk.agent_id, "junk0000");
    assert_eq!(junk.turns, 0);

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
