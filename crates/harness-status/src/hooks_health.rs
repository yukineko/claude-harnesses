//! Hook binary health check: read `~/.claude/settings.json` and flag any
//! registered hook whose `command` target is missing, not executable, or
//! could not be checked at all (e.g. a plugin re-build/rollout that never
//! ran, a pruned cache dir, a 0644 launcher, a relative script path).
//! Surfaced at SessionStart-adjacent time (the `/status` dashboard, which is
//! the natural per-session HOTL check-in point) so a silently-broken hook
//! doesn't go unnoticed. It never touches settings.json and never blocks a
//! turn, but its output IS a verdict to its readers (a human and the
//! SessionStart context): "all present" is printed only when every command
//! hook was checked and found present ([`HooksHealthReport::all_present`]);
//! an unverifiable target or an unreadable settings.json is shown as such,
//! never folded into "present" (CLAUDE.md §1/§3).

use std::path::PathBuf;

use harness_core::hook::{HookTargetStatus, MissingHookBinary};
use serde::Serialize;

/// A registered hook whose target the checker could not verify.
#[derive(Debug, Serialize)]
pub struct UnverifiableHook {
    pub event: String,
    pub command: String,
    pub reason: String,
}

/// The full hook-binary-health report for the current machine.
#[derive(Debug, Serialize)]
pub struct HooksHealthReport {
    pub settings_path: String,
    pub settings_found: bool,
    /// settings.json exists but could not be read or parsed: no hook was
    /// checked.
    pub settings_error: Option<String>,
    pub missing: Vec<MissingHookBinary>,
    /// Targets that exist but cannot be exec'd (same shape as `missing`).
    pub not_executable: Vec<MissingHookBinary>,
    pub unverifiable: Vec<UnverifiableHook>,
}

impl HooksHealthReport {
    /// True only when settings.json was read and every command hook in it
    /// was checked and is present.
    pub fn all_present(&self) -> bool {
        self.settings_found
            && self.settings_error.is_none()
            && self.missing.is_empty()
            && self.not_executable.is_empty()
            && self.unverifiable.is_empty()
    }

    /// One `⚠` line per problem (settings error, missing, not executable,
    /// unverifiable), without indentation. Empty iff nothing to warn about.
    pub fn warning_lines(&self) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(e) = &self.settings_error {
            out.push(format!(
                "⚠ settings.json unreadable ({}): {e} — hook health could not be checked",
                self.settings_path
            ));
        }
        for m in &self.missing {
            out.push(format!(
                "⚠ missing hook binary: {} ({}): {}",
                m.event, m.binary_path, m.command
            ));
        }
        for m in &self.not_executable {
            out.push(format!(
                "⚠ non-executable hook binary: {} ({}): {}",
                m.event, m.binary_path, m.command
            ));
        }
        for u in &self.unverifiable {
            out.push(format!(
                "⚠ unverifiable hook target: {} ({}): {}",
                u.event, u.reason, u.command
            ));
        }
        out
    }
}

/// The default `~/.claude/settings.json` path.
pub fn settings_path() -> PathBuf {
    harness_core::config::home().join(".claude/settings.json")
}

/// Load `~/.claude/settings.json` and classify every command hook's target
/// via [`harness_core::hook::hook_target_findings`]. No plugin root is
/// supplied (this file holds hooks of many plugins), so a
/// `${CLAUDE_PLUGIN_ROOT}` reference is reported as unverifiable rather than
/// guessed at. Never panics: an absent settings.json yields
/// `settings_found: false`; an unreadable/unparseable one yields
/// `settings_error`.
pub fn read() -> HooksHealthReport {
    read_at(&settings_path())
}

/// Testable core of [`read`]: takes an explicit settings.json path.
pub fn read_at(path: &std::path::Path) -> HooksHealthReport {
    let settings_found = path.exists();
    let mut report = HooksHealthReport {
        settings_path: path.display().to_string(),
        settings_found,
        settings_error: None,
        missing: Vec::new(),
        not_executable: Vec::new(),
        unverifiable: Vec::new(),
    };
    let settings = match harness_core::install::load_settings(path) {
        Ok(s) => s,
        Err(e) => {
            report.settings_error = Some(format!("{e:#}"));
            return report;
        }
    };
    for f in harness_core::hook::hook_target_findings(&settings, None) {
        match f.status {
            HookTargetStatus::Present => {}
            HookTargetStatus::Missing { binary_path } => report.missing.push(MissingHookBinary {
                event: f.event,
                command: f.command,
                binary_path,
            }),
            HookTargetStatus::NotExecutable { binary_path } => {
                report.not_executable.push(MissingHookBinary {
                    event: f.event,
                    command: f.command,
                    binary_path,
                })
            }
            HookTargetStatus::Unverifiable { reason } => {
                report.unverifiable.push(UnverifiableHook {
                    event: f.event,
                    command: f.command,
                    reason,
                })
            }
        }
    }
    report
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

    #[test]
    fn unparseable_settings_is_an_error_not_all_present() {
        let tmp = std::env::temp_dir().join(format!("hs-hooks-health-bad-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        let settings = tmp.join("settings.json");
        fs::write(&settings, "{ not json").unwrap();

        let r = read_at(&settings);
        assert!(r.settings_found);
        assert!(r.settings_error.is_some(), "{r:?}");
        assert!(!r.all_present());
        assert!(r.warning_lines()[0].contains("could not be checked"));

        let _ = fs::remove_dir_all(&tmp);
    }
}
