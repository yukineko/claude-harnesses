//! Merge/remove the runbook UserPromptSubmit hook in `~/.claude/settings.json`.
//! Idempotent; backs up before any write; preserves foreign hook groups. This is
//! the standalone `cargo install` path — the plugin path uses `hooks/hooks.json`.
//!
//! Settings-file mechanics (load/backup/write/strip) are shared via
//! `harness_core::install`.

use std::path::PathBuf;

use anyhow::Result;
#[cfg(test)]
use serde_json::{json, Value};

const EVENT: &str = "UserPromptSubmit";
const SUB: &str = "inject";
const TIMEOUT_SECS: u64 = 10;

/// Command substrings runbook owns and should replace on (re)install.
const MARKERS: &[&str] = &["runbook"];

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
        .unwrap_or_else(|| "runbook".to_string())
}

/// True if a hook *group* contains any command we own and should replace.
/// Only exercised by tests now; the install/uninstall paths go through
/// `strip_ours`.
#[cfg(test)]
fn is_ours(group: &Value) -> bool {
    harness_core::install::group_matches(group, MARKERS)
}

/// Strip all runbook groups from an event array; returns the cleaned array.
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

    if dry_run {
        println!("--- dry run (settings.json would become) ---");
        println!("{}", serde_json::to_string_pretty(&settings)?);
        return Ok(());
    }
    harness_core::install::write_settings(&settings_path(), &settings)?;
    println!("\nInstalled UserPromptSubmit hook → {bin} {SUB}");
    Ok(())
}

pub fn uninstall(dry_run: bool) -> Result<()> {
    let mut settings = harness_core::install::load_settings(&settings_path())?;
    if !settings.is_object() {
        anyhow::bail!("settings.json is not a JSON object");
    }
    let removed =
        harness_core::install::remove_hooks_from_settings(&mut settings, MARKERS, &[EVENT]);

    if dry_run {
        println!("--- dry run (would remove {removed} runbook group(s)) ---");
        println!("{}", serde_json::to_string_pretty(&settings)?);
        return Ok(());
    }
    harness_core::install::write_settings(&settings_path(), &settings)?;
    println!("removed {removed} runbook hook group(s)");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_ours() {
        let g = json!({"hooks":[{"type":"command","command":"/x/runbook inject"}]});
        assert!(is_ours(&g));
        let other = json!({"hooks":[{"type":"command","command":"playbook inject"}]});
        assert!(!is_ours(&other));
    }

    #[test]
    fn strip_keeps_foreign() {
        let arr = vec![
            json!({"hooks":[{"type":"command","command":"runbook inject"}]}),
            json!({"hooks":[{"type":"command","command":"playbook inject"}]}),
        ];
        assert_eq!(strip_ours(&arr).len(), 1);
    }
}
