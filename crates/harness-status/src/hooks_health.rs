//! Hook binary health check: read `~/.claude/settings.json` and flag any
//! registered hook whose `command` points at a binary that no longer exists
//! on disk (e.g. a plugin re-build/rollout that never ran, or a cache dir that
//! was pruned). Surfaced at SessionStart-adjacent time (the `/status`
//! dashboard, which is the natural per-session HOTL check-in point) so a
//! silently-broken hook doesn't go unnoticed — this is purely observational:
//! it never touches settings.json and never blocks a turn (fail-soft, matches
//! every other harness-status panel).

use std::path::PathBuf;

use harness_core::hook::MissingHookBinary;
use serde::Serialize;

/// The full hook-binary-health report for the current machine.
#[derive(Debug, Serialize)]
pub struct HooksHealthReport {
    pub settings_path: String,
    pub settings_found: bool,
    pub missing: Vec<MissingHookBinary>,
}

/// The default `~/.claude/settings.json` path.
pub fn settings_path() -> PathBuf {
    harness_core::config::home().join(".claude/settings.json")
}

/// Load `~/.claude/settings.json` and report any hook whose command's binary
/// path does not exist. `plugin_root` is passed through to
/// [`harness_core::hook::missing_hook_binaries`] for `${CLAUDE_PLUGIN_ROOT}`
/// resolution — normally `None` here, since this reads hooks belonging to
/// many different plugins (each with its own root), and an unresolvable
/// `${CLAUDE_PLUGIN_ROOT}` reference is correctly left unflagged rather than
/// guessed at. Never panics: a missing/malformed settings.json yields an
/// empty (but well-formed) report.
pub fn read() -> HooksHealthReport {
    read_at(&settings_path())
}

/// Testable core of [`read`]: takes an explicit settings.json path.
pub fn read_at(path: &std::path::Path) -> HooksHealthReport {
    let settings_found = path.exists();
    let settings = harness_core::install::load_settings(path).unwrap_or(serde_json::json!({}));
    let missing = harness_core::hook::missing_hook_binaries(&settings, None);
    HooksHealthReport {
        settings_path: path.display().to_string(),
        settings_found,
        missing,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn read_at_reports_missing_binaries_and_tolerates_absent_file() {
        let tmp = std::env::temp_dir().join(format!("hs-hooks-health-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        let settings = tmp.join("settings.json");

        // Missing file → empty, well-formed report, never panics.
        let r = read_at(&settings);
        assert!(!r.settings_found);
        assert!(r.missing.is_empty());

        // A settings.json with one absent and one present binary path.
        let present = std::env::current_exe()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        fs::write(
            &settings,
            serde_json::json!({
                "hooks": {
                    "SessionStart": [
                        {"hooks": [
                            {"type": "command", "command": "/no/such/bin/ghost session-start"},
                            {"type": "command", "command": format!("{present} watch")}
                        ]}
                    ]
                }
            })
            .to_string(),
        )
        .unwrap();

        let r = read_at(&settings);
        assert!(r.settings_found);
        assert_eq!(r.missing.len(), 1);
        assert_eq!(r.missing[0].event, "SessionStart");
        assert_eq!(r.missing[0].binary_path, "/no/such/bin/ghost");

        let _ = fs::remove_dir_all(&tmp);
    }
}
