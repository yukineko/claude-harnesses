//! Merge/remove the donegate Stop and SubagentStop hooks in `~/.claude/settings.json`.
//!
//! Idempotent: existing donegate groups are stripped before re-adding, so
//! running install twice is safe. The file is backed up before any write.

use std::path::PathBuf;

use anyhow::Result;
#[cfg(test)]
use serde_json::{json, Value};

const EVENT: &str = "Stop";
const SUB: &str = "gate";
/// The SubagentStop hook judges the roots a stopping subagent wrote
/// (`subagent` module); the Stop gate leaves an in-flight subagent's root to it.
const SUB_EVENT: &str = "SubagentStop";
const SUB_SUB: &str = "subagent-gate";
/// Generous timeout — this hook runs the project's build/test suite.
const TIMEOUT_SECS: u64 = 600;

fn settings_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".claude")
        .join("settings.json")
}

fn binary_path() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.to_str().map(String::from))
        .unwrap_or_else(|| "donegate".to_string())
}

/// Command substring donegate owns. Settings-file mechanics
/// (load/backup/write/strip) are shared via `harness_core::install`.
const MARKERS: &[&str] = &["donegate"];

/// True if a hook group contains a command referencing donegate.
/// Only exercised by tests now; the install/uninstall paths go through
/// `strip_ours`.
#[cfg(test)]
fn is_ours(group: &Value) -> bool {
    harness_core::install::group_matches(group, MARKERS)
}

#[cfg(test)]
fn strip_ours(arr: &[Value]) -> Vec<Value> {
    harness_core::install::strip_matching(arr, MARKERS)
}

pub fn install(dry_run: bool) -> Result<()> {
    let mut settings = harness_core::install::load_settings(&settings_path())?;
    let bin = binary_path();
    if !settings.is_object() {
        anyhow::bail!("settings.json is not a JSON object");
    }
    harness_core::install::push_group(
        &mut settings,
        MARKERS,
        EVENT,
        harness_core::install::command_group(&format!("{bin} {SUB}"), TIMEOUT_SECS),
    )?;
    harness_core::install::push_group(
        &mut settings,
        MARKERS,
        SUB_EVENT,
        harness_core::install::command_group(&format!("{bin} {SUB_SUB}"), TIMEOUT_SECS),
    )?;

    if dry_run {
        println!("--- dry run (settings.json would become) ---");
        println!("{}", serde_json::to_string_pretty(&settings)?);
        return Ok(());
    }
    harness_core::install::write_settings(&settings_path(), &settings)?;
    println!("\nInstalled Stop hook → {bin} {SUB}");
    println!("Installed SubagentStop hook → {bin} {SUB_SUB}");
    println!("Add at least one [[check]] to ./donegate.toml (or ~/.donegate/config.toml).");
    Ok(())
}

pub fn uninstall(dry_run: bool) -> Result<()> {
    let mut settings = harness_core::install::load_settings(&settings_path())?;
    if !settings.is_object() {
        anyhow::bail!("settings.json is not a JSON object");
    }
    let removed = harness_core::install::remove_hooks_from_settings(
        &mut settings,
        MARKERS,
        &[EVENT, SUB_EVENT],
    );

    if dry_run {
        println!("--- dry run (would remove {removed} donegate group(s)) ---");
        println!("{}", serde_json::to_string_pretty(&settings)?);
        return Ok(());
    }
    harness_core::install::write_settings(&settings_path(), &settings)?;
    println!("removed {removed} donegate hook group(s)");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_ours() {
        let g = json!({"hooks":[{"type":"command","command":"/x/donegate gate"}]});
        assert!(is_ours(&g));
        let other = json!({"hooks":[{"type":"command","command":"prettier --write"}]});
        assert!(!is_ours(&other));
    }

    #[test]
    fn strip_keeps_foreign() {
        let arr = vec![
            json!({"hooks":[{"type":"command","command":"donegate gate"}]}),
            json!({"hooks":[{"type":"command","command":"my-other-hook"}]}),
        ];
        assert_eq!(strip_ours(&arr).len(), 1);
    }
}
