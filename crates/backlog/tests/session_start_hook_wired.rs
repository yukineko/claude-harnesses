// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `hooks/hooks.json` — the SessionStart hook must travel WITH the plugin.
//!
//! `src/hooks/session_start.rs` has existed since the crate did, and
//! `.claude-plugin/plugin.json` advertises "a SessionStart hook that surfaces
//! pending work at session open". But the crate shipped no `hooks/hooks.json`:
//! `git log -- crates/backlog/hooks` is empty, so the declaration never
//! existed in-repo. The only wiring was a hand-edited absolute path in the
//! user's `~/.claude/settings.json`:
//!
//!     "command": "/home/<user>/.cargo/bin/backlog session-start"
//!
//! On 2026-08-20 that binary was renamed to `backlog.bak-stale-20260820`, so
//! the hook has exited 127 ("No such file or directory") on every session
//! since — and a SessionStart hook has no exit code and no stderr the agent
//! ever sees, so the banner did not go red, it went DARK. "No pending work"
//! and "the hook never ran" became indistinguishable downstream: the empty
//! banner reads as an empty queue (CLAUDE.md §1/§3), against a queue that
//! `backlog list` shows is not empty.
//!
//! This test pins the structural fix rather than the symptom: the declaration
//! lives in the plugin, resolved through `${CLAUDE_PLUGIN_ROOT}`, so it moves
//! with the rollout and cannot be orphaned by a binary that is renamed,
//! version-bumped, or absent on a fresh clone. It is the same reason every
//! other hook-carrying crate declares its hooks this way (29 of them do).

use std::path::{Path, PathBuf};

fn hooks_json_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("hooks")
        .join("hooks.json")
}

fn load() -> serde_json::Value {
    let path = hooks_json_path();
    let raw = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "crates/backlog/hooks/hooks.json is unreadable ({e}); \
             without it the SessionStart hook is not shipped with the plugin \
             and only an out-of-plugin absolute path can invoke it: {}",
            path.display()
        )
    });
    serde_json::from_str(&raw)
        .unwrap_or_else(|e| panic!("hooks.json is not valid JSON ({e}); a plugin manifest that Claude Code cannot parse installs no hook at all"))
}

/// Every `command` string declared under the given event, flattened across
/// matcher groups.
fn commands_for(doc: &serde_json::Value, event: &str) -> Vec<String> {
    doc.get("hooks")
        .and_then(|h| h.get(event))
        .and_then(|e| e.as_array())
        .map(|groups| {
            groups
                .iter()
                .filter_map(|g| g.get("hooks").and_then(|h| h.as_array()))
                .flatten()
                .filter_map(|h| h.get("command").and_then(|c| c.as_str()))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn session_start_hook_is_declared_in_the_plugin() {
    let doc = load();
    let commands = commands_for(&doc, "SessionStart");
    assert!(
        !commands.is_empty(),
        "hooks.json declares no SessionStart hook, so src/hooks/session_start.rs \
         is dead code from the harness's point of view and plugin.json's claim \
         that this plugin surfaces pending work at session open is false: {doc:#?}"
    );
    assert!(
        commands
            .iter()
            .any(|c| c.contains("session-start") && c.contains("backlog")),
        "no declared SessionStart command invokes `backlog session-start`: {commands:?}"
    );
}

#[test]
fn session_start_command_resolves_through_plugin_root() {
    let doc = load();
    for command in commands_for(&doc, "SessionStart") {
        assert!(
            command.contains("${CLAUDE_PLUGIN_ROOT}"),
            "SessionStart command {command:?} does not resolve through \
             ${{CLAUDE_PLUGIN_ROOT}}. An absolute path outside the plugin is \
             exactly what broke on 2026-08-20: the binary moved and the hook \
             silently exited 127 with nothing to distinguish it from an empty \
             queue."
        );
        assert!(
            !command.contains(".cargo/bin"),
            "SessionStart command {command:?} points into ~/.cargo/bin, which \
             is not part of the plugin and is not refreshed by \
             scripts/rollout-plugins.sh — the hook would run whatever stale \
             build happens to be sitting there, or nothing at all."
        );
    }
}
