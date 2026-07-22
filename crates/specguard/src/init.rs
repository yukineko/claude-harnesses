//! `specguard init`: scaffold specguard into a target repo.
//!
//! Two artifacts: a starter `specguard.toml` (copied from the documented
//! example) and a Claude Code SessionStart hook in `.claude/settings.json` that
//! surfaces the pending sentinel at the top of each session (the Human-on-the-
//! loop trigger). Both steps are idempotent: re-running `init` never duplicates
//! the hook, and an existing config is left untouched unless `--force`.

use anyhow::{Context, Result};
use std::path::Path;

/// The starter config is the documented example, embedded so the binary can
/// scaffold without the source tree present.
const EXAMPLE_CONFIG: &str = include_str!("../specguard.example.toml");

/// Substring identifying our hook, used to detect a prior install.
const HOOK_MARKER: &str = "specguard pending";

/// The SessionStart command: delegate to `specguard pending`, which resolves the
/// sentinel from `[output].sentinel` (so a custom path works) and prints an
/// active fix-offer block when something is pending. `|| true` keeps the session
/// starting even if the binary isn't on PATH in the hook's shell.
const HOOK_COMMAND: &str = "specguard pending 2>/dev/null || true";

/// Run `specguard init` for the config at `config_path` (its parent dir is the
/// repo root the hook is installed into).
pub fn run(config_path: &Path, force: bool) -> Result<()> {
    let target_dir = config_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));

    scaffold_config(config_path, force)?;
    install_hook(target_dir)?;

    println!("specguard: init 完了");
    println!("  次の手順:");
    println!(
        "    1. {} を編集 ([[area]]/[[invariant]]/canon を対象リポジトリに合わせる)",
        config_path.display()
    );
    println!("    2. `specguard run` で監査を実行");
    println!("    3. needs_user の指摘に対応したら `specguard ack`");
    Ok(())
}

fn scaffold_config(config_path: &Path, force: bool) -> Result<()> {
    if config_path.exists() && !force {
        println!(
            "specguard: {} は既に存在 — skip (--force で上書き)",
            config_path.display()
        );
        return Ok(());
    }
    if let Some(dir) = config_path.parent() {
        if !dir.as_os_str().is_empty() {
            std::fs::create_dir_all(dir).ok();
        }
    }
    std::fs::write(config_path, EXAMPLE_CONFIG)
        .with_context(|| format!("writing {}", config_path.display()))?;
    println!("specguard: wrote {}", config_path.display());
    Ok(())
}

fn install_hook(target_dir: &Path) -> Result<()> {
    let claude_dir = target_dir.join(".claude");
    std::fs::create_dir_all(&claude_dir)
        .with_context(|| format!("creating {}", claude_dir.display()))?;
    let settings_path = claude_dir.join("settings.json");

    let mut root: serde_json::Value = if settings_path.exists() {
        let text = std::fs::read_to_string(&settings_path)
            .with_context(|| format!("reading {}", settings_path.display()))?;
        serde_json::from_str(&text)
            .with_context(|| format!("parsing {} (must be valid JSON)", settings_path.display()))?
    } else {
        serde_json::json!({})
    };

    if !root.is_object() {
        anyhow::bail!("{} is not a JSON object", settings_path.display());
    }
    if hook_present(&root) {
        println!(
            "specguard: SessionStart hook は設定済み — skip ({})",
            settings_path.display()
        );
        return Ok(());
    }

    let group = serde_json::json!({
        "matcher": "startup|resume",
        "hooks": [
            { "type": "command", "command": HOOK_COMMAND, "shell": "bash" }
        ]
    });

    // Merge into hooks.SessionStart without disturbing any existing settings.
    // Each of the three casts below used to be `.unwrap()`. The shape was
    // already checked upstream in every case, but an unwrap turns a
    // "settings.json is not what we assumed" into a panic mid-write; bailing
    // names the file and leaves it untouched.
    let hooks = root
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("{} is not a JSON object", settings_path.display()))?
        .entry("hooks")
        .or_insert_with(|| serde_json::json!({}));
    if !hooks.is_object() {
        anyhow::bail!("`hooks` in {} is not an object", settings_path.display());
    }
    let session_start = hooks
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("`hooks` in {} is not an object", settings_path.display()))?
        .entry("SessionStart")
        .or_insert_with(|| serde_json::json!([]));
    if !session_start.is_array() {
        anyhow::bail!(
            "`hooks.SessionStart` in {} is not an array",
            settings_path.display()
        );
    }
    session_start
        .as_array_mut()
        .ok_or_else(|| {
            anyhow::anyhow!(
                "`hooks.SessionStart` in {} is not an array",
                settings_path.display()
            )
        })?
        .push(group);

    let pretty = serde_json::to_string_pretty(&root).context("serializing settings.json")?;
    std::fs::write(&settings_path, format!("{pretty}\n"))
        .with_context(|| format!("writing {}", settings_path.display()))?;
    println!(
        "specguard: SessionStart hook を追加 ({})",
        settings_path.display()
    );
    Ok(())
}

/// True if any SessionStart hook command already references our sentinel.
fn hook_present(root: &serde_json::Value) -> bool {
    let Some(groups) = root
        .get("hooks")
        .and_then(|h| h.get("SessionStart"))
        .and_then(|s| s.as_array())
    else {
        return false;
    };
    groups.iter().any(|g| {
        g.get("hooks")
            .and_then(|h| h.as_array())
            .is_some_and(|hooks| {
                hooks.iter().any(|h| {
                    h.get("command")
                        .and_then(|c| c.as_str())
                        .is_some_and(|c| c.contains(HOOK_MARKER))
                })
            })
    })
}
