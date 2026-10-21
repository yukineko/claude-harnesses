//! Merge/remove the reviewgate Stop hook in `~/.claude/settings.json`.
//!
//! Idempotent: existing reviewgate groups are stripped before re-adding, so
//! running install twice is safe. The file is backed up before any write.

use std::path::{Path, PathBuf};

use anyhow::Result;
#[cfg(test)]
use serde_json::{json, Value};

const EVENT: &str = "Stop";
const SUB: &str = "review";
/// Generous timeout — subprocess mode runs a full reviewer pass.
const TIMEOUT_SECS: u64 = 600;

/// `~/.claude/settings.json`, with home resolved by the shared
/// [`harness_core::config::home`] policy. An undeterminable home (on unix:
/// `$HOME` unset or empty) is an error (backlog 12657c5e): falling back to
/// `./` wrote `./.claude/settings.json` under the cwd, where Claude Code never
/// reads it, and still reported "Installed Stop hook".
fn settings_path() -> Result<PathBuf> {
    let home = harness_core::config::home();
    if home == Path::new(harness_core::config::UNDETERMINED_HOME) {
        anyhow::bail!(
            "cannot determine the home directory ($HOME is unset or empty), so the \
             location of ~/.claude/settings.json is unknown; refusing to write \
             anything. Set HOME and re-run."
        );
    }
    Ok(home.join(".claude").join("settings.json"))
}

fn binary_path() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.to_str().map(String::from))
        .unwrap_or_else(|| "reviewgate".to_string())
}

/// Command substrings reviewgate owns. Settings-file mechanics
/// (load/backup/write/strip) are shared via `harness_core::install`.
const MARKERS: &[&str] = &["reviewgate"];

/// True if a hook group contains a command we own and should replace. Only
/// exercised by tests now; the install/uninstall paths go through `strip_ours`.
#[cfg(test)]
fn is_ours(group: &Value) -> bool {
    harness_core::install::group_matches(group, MARKERS)
}

/// Strip all reviewgate groups from an event array; returns the cleaned array.
#[cfg(test)]
fn strip_ours(arr: &[Value]) -> Vec<Value> {
    harness_core::install::strip_matching(arr, MARKERS)
}

pub fn install(dry_run: bool) -> Result<()> {
    let mut settings = harness_core::install::load_settings(&settings_path()?)?;
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
    harness_core::install::write_settings(&settings_path()?, &settings)?;
    println!("\nInstalled Stop hook → {bin} {SUB}");
    println!("Tune ./reviewgate.toml (or ~/.reviewgate/config.toml) — `reviewgate init` writes a starter.");
    Ok(())
}

pub fn uninstall(dry_run: bool) -> Result<()> {
    let mut settings = harness_core::install::load_settings(&settings_path()?)?;
    if !settings.is_object() {
        anyhow::bail!("settings.json is not a JSON object");
    }
    let removed =
        harness_core::install::remove_hooks_from_settings(&mut settings, MARKERS, &[EVENT]);

    if dry_run {
        println!("--- dry run (would remove {removed} reviewgate group(s)) ---");
        println!("{}", serde_json::to_string_pretty(&settings)?);
        return Ok(());
    }
    harness_core::install::write_settings(&settings_path()?, &settings)?;
    println!("removed {removed} reviewgate hook group(s)");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_ours() {
        let g = json!({"hooks":[{"type":"command","command":"/x/reviewgate review"}]});
        assert!(is_ours(&g));
        let other = json!({"hooks":[{"type":"command","command":"prettier --write"}]});
        assert!(!is_ours(&other));
    }

    #[test]
    fn strip_keeps_foreign() {
        let arr = vec![
            json!({"hooks":[{"type":"command","command":"reviewgate review"}]}),
            json!({"hooks":[{"type":"command","command":"my-other-hook"}]}),
        ];
        assert_eq!(strip_ours(&arr).len(), 1);
    }
}
