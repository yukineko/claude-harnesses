// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `hooks/hooks.json` — the record-store sync hook must travel WITH the plugin.
//!
//! `cmd_sync` (src/main.rs) pulls the episode/playbook store from `sync_repo`
//! and commits+pushes local appends back. `docs/zenn-fugu-router.md` claimed
//! "以後は Stop hook が自動で sync するようになっている", but the crate's
//! `hooks/hooks.json` declared only `UserPromptSubmit`. The sole wiring was a
//! hand-orphaned absolute path in the user's `~/.claude/settings.json`:
//!
//!     "command": "/home/<user>/.cargo/bin/fugu-router sync"
//!
//! Measured 2026-08-26: that path does not exist (the build was renamed to
//! `fugu-router.bak-stale-20260820`), so the hook exits 127 every session. It
//! did not go red, it went DARK — a SessionEnd hook's exit code and stderr
//! reach neither the agent nor the user, so "sync ran and had nothing to do"
//! and "sync never ran" became indistinguishable (CLAUDE.md §1/§3). The
//! damage was silent and cumulative: the last `fugu-router sync` commit in
//! `~/.fugu-router/record-repo` is 2026-07-23 09:57:38 +0900, while
//! `episodes.jsonl`/`playbooks.jsonl` in that same repo have kept growing as
//! uncommitted, unpushed working-tree modifications ever since — every episode
//! recorded in ~34 days existed only on this machine.
//!
//! This test pins the structural fix rather than the symptom: the declaration
//! lives in the plugin and resolves through `${CLAUDE_PLUGIN_ROOT}`, so it
//! travels with `scripts/rollout-plugins.sh` and cannot be orphaned by a
//! binary that is renamed, version-bumped, or absent on a fresh clone. It is
//! the same fix, for the same reason, as `crates/backlog/tests/
//! session_start_hook_wired.rs`.

use std::path::{Path, PathBuf};
use std::process::Command;

fn hooks_json_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("hooks")
        .join("hooks.json")
}

fn load() -> serde_json::Value {
    let path = hooks_json_path();
    let raw = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "crates/fugu-router/hooks/hooks.json is unreadable ({e}); \
             without it no hook ships with the plugin and only an \
             out-of-plugin absolute path can invoke sync: {}",
            path.display()
        )
    });
    serde_json::from_str(&raw).unwrap_or_else(|e| {
        panic!(
            "hooks.json is not valid JSON ({e}); a plugin manifest that \
             Claude Code cannot parse installs no hook at all"
        )
    })
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
fn every_declared_command_resolves_through_plugin_root() {
    let doc = load();
    let events = doc
        .get("hooks")
        .and_then(|h| h.as_object())
        .map(|m| m.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    assert!(!events.is_empty(), "hooks.json declares no events at all");

    for event in events {
        for command in commands_for(&doc, &event) {
            for part in command.split(';').map(str::trim).filter(|p| !p.is_empty()) {
                assert!(
                    part.starts_with("${CLAUDE_PLUGIN_ROOT}/bin/fugu-router"),
                    "{event} segment {part:?} of {command:?} does not resolve through \
                     ${{CLAUDE_PLUGIN_ROOT}}"
                );
            }
            assert!(
                command.contains("${CLAUDE_PLUGIN_ROOT}"),
                "{event} command {command:?} does not resolve through \
                 ${{CLAUDE_PLUGIN_ROOT}}. An absolute path outside the plugin \
                 is exactly what broke here: the binary moved and the hook \
                 exited 127 with nothing downstream to distinguish that from a \
                 sync that had no work to do."
            );
            assert!(
                !command.contains(".cargo/bin"),
                "{event} command {command:?} points into ~/.cargo/bin, which is \
                 not part of the plugin and is not refreshed by \
                 scripts/rollout-plugins.sh — the hook would run whatever stale \
                 build happens to be sitting there, or nothing at all."
            );
        }
    }
}

/// A wire pointing at a subcommand the binary does not have is dead in exactly
/// the same way as a wire pointing at a binary that does not exist — and it
/// fails just as silently. (Measured 2026-08-26: `settings.json` still carried
/// `autoflow session-start`, a subcommand deleted in autoflow 0.1.22.)
#[test]
fn every_declared_subcommand_exists_in_the_binary() {
    let doc = load();
    let exe = env!("CARGO_BIN_EXE_fugu-router");
    let events = doc
        .get("hooks")
        .and_then(|h| h.as_object())
        .map(|m| m.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default();

    let mut checked = 0usize;
    for event in events {
        for command in commands_for(&doc, &event) {
            // A command string may chain several commands with `;`
            // (`harvest; sync`). Validate every one, not just the first.
            for part in command.split(';').map(str::trim).filter(|p| !p.is_empty()) {
                // `${CLAUDE_PLUGIN_ROOT}/bin/fugu-router <sub> [args...]`
                let sub = part
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or_else(|| panic!("{event} command {part:?} passes no subcommand"));
                let out = Command::new(exe)
                    .args([sub, "--help"])
                    .output()
                    .unwrap_or_else(|e| panic!("could not run {exe} {sub} --help: {e}"));
                assert!(
                    out.status.success(),
                    "{event} declares `fugu-router {sub}`, but `{sub} --help` exited \
                     {:?}. The wire names a subcommand this binary does not have, so \
                     the hook is dead: {}",
                    out.status.code(),
                    String::from_utf8_lossy(&out.stderr)
                );
                checked += 1;
            }
        }
    }
    assert!(checked > 0, "no declared commands were checked");
}

/// SessionEnd must run `harvest` (turn merges into episodes) BEFORE `sync`
/// (push the store), sequentially, from ONE hook command: separate hooks run
/// in parallel and sync could push a store harvest has not yet appended to.
///
/// Re-anchored 2026-10-25. The proxy used to be the literal command string
/// `harvest; sync`. That foreground chain blew Claude Code's ~1.5 s SessionEnd
/// budget and was cancelled mid-sync (`tests/session_end_budget.rs`), so the
/// hook is now `session-end-sync`, which hands both steps to one detached worker.
/// The property is unchanged and is now observed on the real path: the single
/// declared SessionEnd command is run, and the worker's log must show the
/// harvest step starting before the sync step, with each step started exactly
/// once.
#[test]
fn session_end_runs_harvest_before_sync_in_one_command() {
    let doc = load();
    let commands = commands_for(&doc, "SessionEnd");
    assert_eq!(
        commands.len(),
        1,
        "SessionEnd must declare exactly one command, so harvest and sync cannot \
         run as parallel hooks: {commands:?}"
    );
    let cmd = &commands[0];

    let root = std::env::temp_dir().join(format!("fugu-sessionend-order-{}", std::process::id()));
    if root.exists() {
        std::fs::remove_dir_all(&root).unwrap();
    }
    let home = root.join("home");
    let plugin = root.join("plugin");
    let cwd = root.join("cwd");
    std::fs::create_dir_all(home.join(".fugu-router")).unwrap();
    std::fs::create_dir_all(plugin.join("bin")).unwrap();
    std::fs::create_dir_all(&cwd).unwrap();
    std::os::unix::fs::symlink(
        env!("CARGO_BIN_EXE_fugu-router"),
        plugin.join("bin").join("fugu-router"),
    )
    .unwrap();

    let out = Command::new("sh")
        .args(["-c", cmd])
        .env("HOME", &home)
        .env("CLAUDE_PLUGIN_ROOT", &plugin)
        .env("GIT_CEILING_DIRECTORIES", &root)
        .env_remove("FUGU_ROUTER_DISABLED")
        .current_dir(&cwd)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "SessionEnd command {cmd:?} exited {:?}: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );

    // Wait for the detached worker to finish (its log says `done`).
    let log_path = home.join(".fugu-router").join("session-end.log");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let log = loop {
        let log = std::fs::read_to_string(&log_path).unwrap_or_default();
        if log.contains("session-end: done") {
            break log;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the SessionEnd worker did not finish within 20 s; log so far:\n{log}"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    let harvest_at = log.find("session-end: step harvest");
    let sync_at = log.find("session-end: step sync");
    assert!(
        matches!((harvest_at, sync_at), (Some(h), Some(s)) if h < s),
        "harvest must precede sync in the single SessionEnd command {cmd:?}; worker log:\n{log}"
    );
    assert_eq!(
        log.matches("session-end: step harvest").count(),
        1,
        "log:\n{log}"
    );
    assert_eq!(
        log.matches("session-end: step sync").count(),
        1,
        "log:\n{log}"
    );
}
