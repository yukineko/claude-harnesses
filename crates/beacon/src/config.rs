//! Configuration: project `beacon.toml` (preferred) over a home-level
//! `~/.beacon/config.toml` over built-in defaults. The first file that exists
//! wins (the layers are not merged), matching the rest of the toolkit.
//!
//! Secrets (webhook URLs) may also come from the environment so they need not
//! be committed: `BEACON_SLACK_WEBHOOK` and `BEACON_WEBHOOK` override the file.

use std::path::{Path, PathBuf};

use serde::Deserialize;

// Re-exported so existing `crate::config::expand_tilde` call sites keep working.
pub use harness_core::config::expand_tilde;

#[derive(Debug, Clone)]
pub struct Config {
    pub enabled: bool,
    /// Notify when a turn finishes (Stop hook).
    pub on_stop: bool,
    /// Notify when Claude Code raises a Notification (waiting for input/permission).
    pub on_notification: bool,
    /// On Stop, append a short tail of Claude's last message to the body.
    pub include_snippet: bool,
    /// Max characters of that snippet.
    pub snippet_chars: usize,

    // --- channels ---
    /// macOS `osascript` / Linux `notify-send` desktop notification.
    pub desktop: bool,
    /// macOS notification sound name (e.g. "Glass", "Ping"); None = silent.
    pub sound: Option<String>,
    /// Slack incoming-webhook URL (env `BEACON_SLACK_WEBHOOK` overrides).
    pub slack_webhook: Option<String>,
    /// Generic webhook URL that receives a JSON payload (env `BEACON_WEBHOOK`).
    pub webhook: Option<String>,
    /// Escape-hatch command run via `sh -c` with BEACON_* env vars set.
    pub command: Option<String>,

    /// Append each notification to `<state_dir>/log.jsonl`.
    pub log: bool,
    pub state_dir: PathBuf,
}

#[derive(Debug, Default, Deserialize)]
struct FileConfig {
    enabled: Option<bool>,
    on_stop: Option<bool>,
    on_notification: Option<bool>,
    include_snippet: Option<bool>,
    snippet_chars: Option<usize>,
    desktop: Option<bool>,
    sound: Option<String>,
    slack_webhook: Option<String>,
    webhook: Option<String>,
    command: Option<String>,
    log: Option<bool>,
    state_dir: Option<String>,
}

pub fn base_dir() -> PathBuf {
    harness_core::config::base_dir("beacon")
}

/// Treat empty strings as "unset" so a blank TOML value disables the channel.
fn non_empty(s: Option<String>) -> Option<String> {
    s.filter(|v| !v.trim().is_empty())
}

fn env_non_empty(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.trim().is_empty())
}

impl Default for Config {
    fn default() -> Self {
        Config {
            enabled: true,
            on_stop: true,
            on_notification: true,
            include_snippet: true,
            snippet_chars: 160,
            desktop: true,
            sound: None,
            slack_webhook: None,
            webhook: None,
            command: None,
            log: true,
            state_dir: base_dir().join("state"),
        }
    }
}

impl Config {
    pub fn project_path(root: &Path) -> PathBuf {
        root.join("beacon.toml")
    }

    pub fn home_path() -> PathBuf {
        base_dir().join("config.toml")
    }

    pub fn load(root: &Path) -> Self {
        let mut cfg = Config::default();
        let project = Config::project_path(root);
        let (chosen, from_project) = if project.exists() {
            (Some(project.clone()), true)
        } else {
            let h = Config::home_path();
            (h.exists().then_some(h), false)
        };
        if let Some(path) = chosen {
            if let Ok(text) = std::fs::read_to_string(&path) {
                if let Ok(fc) = toml::from_str::<FileConfig>(&text) {
                    if let Some(v) = fc.enabled {
                        cfg.enabled = v;
                    }
                    if let Some(v) = fc.on_stop {
                        cfg.on_stop = v;
                    }
                    if let Some(v) = fc.on_notification {
                        cfg.on_notification = v;
                    }
                    if let Some(v) = fc.include_snippet {
                        cfg.include_snippet = v;
                    }
                    if let Some(v) = fc.snippet_chars {
                        cfg.snippet_chars = v;
                    }
                    if let Some(v) = fc.desktop {
                        cfg.desktop = v;
                    }
                    cfg.sound = non_empty(fc.sound);
                    cfg.slack_webhook = non_empty(fc.slack_webhook);
                    cfg.webhook = non_empty(fc.webhook);
                    cfg.command = non_empty(fc.command);
                    if let Some(v) = fc.log {
                        cfg.log = v;
                    }
                    if let Some(v) = fc.state_dir {
                        cfg.state_dir = expand_tilde(&v);
                    }
                }
            }
        }
        // Workspace-trust gate: the escape-hatch `command` is run via `sh -c`, so
        // a project-local `beacon.toml` shipped by an untrusted repository must not
        // be allowed to execute it. Drop the project-sourced command unless the
        // project root is trusted: explicitly listed, HARNESS_TRUST_ALL set, or a
        // linked git worktree of a trusted main worktree (`trust::resolve`, the
        // same worktree-inheritance rule donegate uses).
        // Built-in channels (desktop/slack/webhook) and the home/default config
        // are unaffected — only the project-derived command is gated.
        if from_project && cfg.command.is_some() && !harness_core::trust::resolve(root).is_trusted()
        {
            eprintln!(
                "beacon: {} is not trusted; ignoring its command. Run 'beacon trust' to enable.",
                project.display()
            );
            cfg.command = None;
        }
        // Environment overrides for secrets win over the file.
        if let Some(v) = env_non_empty("BEACON_SLACK_WEBHOOK") {
            cfg.slack_webhook = Some(v);
        }
        if let Some(v) = env_non_empty("BEACON_WEBHOOK") {
            cfg.webhook = Some(v);
        }
        // sanitize
        cfg.snippet_chars = cfg.snippet_chars.clamp(20, 1000);
        cfg
    }

    pub fn disabled_env() -> bool {
        std::env::var("BEACON_DISABLE")
            .map(|v| !v.is_empty() && v != "0")
            .unwrap_or(false)
    }

    /// True when at least one delivery channel is configured.
    pub fn any_channel(&self) -> bool {
        self.desktop
            || self.slack_webhook.is_some()
            || self.webhook.is_some()
            || self.command.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Tests below mutate process-wide env (HOME / HARNESS_TRUST_ALL), so they must
    // not run concurrently — serialize them through a single test with a guard.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn write(path: &Path, body: &str) {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p).unwrap();
        }
        std::fs::write(path, body).unwrap();
    }

    #[test]
    fn project_command_is_gated_behind_workspace_trust() {
        let _guard = ENV_LOCK.lock().unwrap();

        // Isolate the home dir so the shared trust list and home config can't leak
        // in from the real environment.
        let home = tempfile::tempdir().unwrap();
        let prev_home = std::env::var_os("HOME");
        let prev_trust_all = std::env::var_os("HARNESS_TRUST_ALL");
        std::env::set_var("HOME", home.path());
        std::env::remove_var("HARNESS_TRUST_ALL");

        // A project dir shipping a beacon.toml with an escape-hatch command.
        let project = tempfile::tempdir().unwrap();
        write(
            &Config::project_path(project.path()),
            "command = \"echo pwned\"\ndesktop = true\n",
        );

        // 1) Untrusted project: the command must be dropped, but other project
        //    settings (here `desktop`) still apply.
        let cfg = Config::load(project.path());
        assert_eq!(
            cfg.command, None,
            "untrusted project command must be dropped"
        );
        assert!(cfg.desktop, "non-command project settings still apply");

        // 2) HARNESS_TRUST_ALL trusts every project → command is honored.
        std::env::set_var("HARNESS_TRUST_ALL", "1");
        let cfg = Config::load(project.path());
        assert_eq!(cfg.command.as_deref(), Some("echo pwned"));
        std::env::remove_var("HARNESS_TRUST_ALL");

        // 3) Explicitly trusting the project root also honors the command.
        harness_core::trust::add(project.path()).unwrap();
        let cfg = Config::load(project.path());
        assert_eq!(cfg.command.as_deref(), Some("echo pwned"));
        harness_core::trust::remove(project.path()).unwrap();

        // 4) A home-level command needs no trust (home config is trusted).
        let no_project = tempfile::tempdir().unwrap();
        write(&Config::home_path(), "command = \"echo home\"\n");
        let cfg = Config::load(no_project.path());
        assert_eq!(
            cfg.command.as_deref(),
            Some("echo home"),
            "home-sourced command is honored without trust"
        );

        // restore env
        match prev_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        match prev_trust_all {
            Some(v) => std::env::set_var("HARNESS_TRUST_ALL", v),
            None => std::env::remove_var("HARNESS_TRUST_ALL"),
        }
    }
}
