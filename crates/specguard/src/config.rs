//! Configuration model for specguard.
//!
//! Everything project-specific (which directories form an "area", where the
//! canonical specs live, which invariants to check every run, how to invoke the
//! agent) is expressed in a TOML file so the binary itself stays generic. See
//! `specguard.example.toml` for a fully documented sample.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::Path;

/// Top-level config, deserialized from the project's `specguard.toml`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub project: Project,
    #[serde(default)]
    pub agent: AgentConfig,
    #[serde(default)]
    pub scope: ScopeConfig,
    #[serde(default)]
    pub output: OutputConfig,
    #[serde(default)]
    pub prompt: PromptConfig,
    #[serde(default)]
    pub decisions: DecisionsConfig,
    #[serde(default)]
    pub verify: VerifyConfig,
    /// Change-triggered audit areas. An area is in-scope when at least one
    /// changed file (since the baseline) matches one of its globs.
    #[serde(default, rename = "area")]
    pub areas: Vec<Area>,
    /// Invariants checked on every run regardless of the diff.
    #[serde(default, rename = "invariant")]
    pub invariants: Vec<Invariant>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Project {
    pub name: String,
    /// Repo root the audit runs against. Relative to the config file's
    /// directory; defaults to ".".
    #[serde(default = "default_dot")]
    pub root: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentConfig {
    /// Executable to invoke (the read-only auditing agent). The rendered prompt
    /// is delivered on its stdin; its stdout is captured as the report.
    pub command: String,
    /// Arguments passed to `command`.
    pub args: Vec<String>,
}

impl Default for AgentConfig {
    /// Defaults to the Claude Code CLI in read-only `--print` mode with the
    /// read-only guarantee *enforced by the harness*, not just requested in the
    /// prompt: an allowlist grants Read/Grep/Glob plus read-only git, and
    /// mutating tools are explicitly denied. We deliberately do NOT pass
    /// `--permission-mode bypassPermissions`: in `--print` mode any tool outside
    /// the allowlist is auto-denied (there is no one to approve it), so arbitrary
    /// Bash, file writes and network calls cannot succeed even if the model tries
    /// — closing the prompt-injection hole where audited repo content could drive
    /// a destructive command. Override `[agent]` to relax this.
    fn default() -> Self {
        AgentConfig {
            command: "claude".to_string(),
            args: DEFAULT_AGENT_ARGS.iter().map(|s| s.to_string()).collect(),
        }
    }
}

/// The built-in read-only agent argv (single source of truth; sample configs
/// reproduce this only for documentation).
pub const DEFAULT_AGENT_ARGS: &[&str] = &[
    "--print",
    "--allowedTools",
    "Read",
    "Glob",
    "Grep",
    "Bash(git diff *)",
    "Bash(git log *)",
    "Bash(git show *)",
    "Bash(git status *)",
    "--disallowedTools",
    "Edit",
    "Write",
    "NotebookEdit",
    "WebFetch",
];

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScopeConfig {
    /// Explicit baseline ref. When empty, the last recorded ref is used, then
    /// `fallback_ref`. Overridden by `--baseline` / `SPECGUARD_BASELINE_REF`.
    #[serde(default)]
    pub baseline_ref: String,
    /// Baseline used on the very first run (no recorded ref yet).
    #[serde(default = "default_fallback_ref")]
    pub fallback_ref: String,
}

// Manual Default so an entirely-omitted `[scope]` table still yields a usable
// fallback_ref (serde field-level defaults only fire for missing fields of a
// table that is itself present).
impl Default for ScopeConfig {
    fn default() -> Self {
        ScopeConfig {
            baseline_ref: String::new(),
            fallback_ref: default_fallback_ref(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputConfig {
    /// Directory (relative to repo root) for `<date>.md` reports and `.last-ref`.
    #[serde(default = "default_report_dir")]
    pub report_dir: String,
    /// Sentinel file written when an audit finds something needing human review.
    #[serde(default = "default_sentinel")]
    pub sentinel: String,
}

impl Default for OutputConfig {
    fn default() -> Self {
        OutputConfig {
            report_dir: default_report_dir(),
            sentinel: default_sentinel(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct PromptConfig {
    /// Optional path (relative to the config file) to a custom prompt template.
    /// When empty, the embedded default template is used.
    #[serde(default)]
    pub template: String,
    /// Treat the prompt templates as meta-canon (the audit policy) that must be
    /// explicitly ratified. When true, `run` refuses to audit if the prompt has
    /// changed since (or was never) ratified via `specguard accept-prompt`.
    #[serde(default)]
    pub require_ratification: bool,
}

/// Verification gates over the audit's findings (see DESIGN-VERIFY.md). Both
/// default OFF: verification is an explicit opt-in that adds extra agent calls.
/// DESIGN-VERIFY.md §8 recommends enabling BOTH together — `enabled` alone
/// (refute without completeness) biases toward false negatives.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct VerifyConfig {
    /// V1 adversarial verification: re-derive each `needs_user=yes` finding with
    /// an independent skeptic and drop only those refuted by a verbatim quote
    /// (removes false positives; uncertain findings are kept).
    #[serde(default)]
    pub enabled: bool,
    /// V2 completeness critique: a separate agent surfaces verifiable canon rules
    /// the sampling audit never matched against the implementation (false
    /// negatives). Runs independently of `enabled`.
    #[serde(default)]
    pub completeness: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionsConfig {
    /// Directory holding decision records (ADRs) — the "why" behind canon
    /// changes, pinned to a canon commit. Relative paths resolve under the repo
    /// root; an absolute path can point at e.g. an Obsidian vault. The D3 audit
    /// (decision freshness/obsolescence) runs whenever this dir has any `*.md`.
    /// Set to "" to disable decisions entirely.
    #[serde(default = "default_decisions_dir")]
    pub dir: String,
}

impl Default for DecisionsConfig {
    fn default() -> Self {
        DecisionsConfig {
            dir: default_decisions_dir(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Area {
    pub name: String,
    /// Globs (repo-root-relative, `/`-separated) that define the area's files.
    pub globs: Vec<String>,
    /// Canonical spec pointers for this area (file paths or `file:section`).
    /// The agent reads these; their content is never copied into the prompt.
    #[serde(default)]
    pub canon: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Invariant {
    pub name: String,
    /// One-line statement of the rule (e.g. "signing only via signature.py").
    #[serde(default)]
    pub description: String,
    /// Canon pointers backing this invariant.
    #[serde(default)]
    pub canon: Vec<String>,
}

fn default_dot() -> String {
    ".".to_string()
}
fn default_fallback_ref() -> String {
    "HEAD~20".to_string()
}
fn default_report_dir() -> String {
    "reports/spec-audit".to_string()
}
fn default_sentinel() -> String {
    ".specguard-pending".to_string()
}
fn default_decisions_dir() -> String {
    "decisions".to_string()
}

/// Shell metacharacters that, in a NON-default `agent.command`, indicate an
/// attempt to smuggle a shell command through the repo config. `agent.command`
/// is passed to [`std::process::Command::new`] (see `agent.rs`) — there is no
/// shell involved at the spawn site, so these characters are never legitimate in
/// an executable name/path and their presence signals injection. See
/// `SECURITY.md`.
const AGENT_COMMAND_METACHARS: &[char] = &[';', '|', '&', '`', '\n', '\r', '<', '>'];

/// Validate a repo-config-supplied agent command (the trust boundary between
/// `specguard.toml` and process spawning).
///
/// The DEFAULT command (`claude`) is trusted unconditionally. Any NON-default
/// command is rejected if it contains a shell metacharacter (`;`, `|`, `&`,
/// backtick, `$(...)` command substitution, newline, or redirection), because
/// such a value can only be an injection attempt: `agent.command` is spawned
/// directly via `Command::new` with no shell, so a clean executable path never
/// needs them.
fn validate_agent_command(cfg: &AgentConfig) -> Result<()> {
    // The built-in default is trusted unconditionally.
    if cfg.command == AgentConfig::default().command {
        return Ok(());
    }
    if let Some(c) = cfg
        .command
        .chars()
        .find(|c| AGENT_COMMAND_METACHARS.contains(c))
    {
        anyhow::bail!(
            "agent.command {:?} contains shell metacharacter {:?}; \
             a non-default command is spawned directly (no shell) and must be a \
             clean executable name/path (see SECURITY.md)",
            cfg.command,
            c
        );
    }
    // `$(...)` command substitution (the `$` alone is harmless in a path).
    if cfg.command.contains("$(") {
        anyhow::bail!(
            "agent.command {:?} contains `$(` command substitution; \
             a non-default command must be a clean executable name/path (see SECURITY.md)",
            cfg.command
        );
    }
    Ok(())
}

impl Config {
    /// Load and validate a config from a TOML file.
    pub fn load(path: &Path) -> Result<Config> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading config {}", path.display()))?;
        let cfg: Config =
            toml::from_str(&text).with_context(|| format!("parsing config {}", path.display()))?;
        cfg.validate()?;
        Ok(cfg)
    }

    fn validate(&self) -> Result<()> {
        if self.project.name.trim().is_empty() {
            anyhow::bail!("project.name must not be empty");
        }
        if self.agent.command.trim().is_empty() {
            anyhow::bail!("agent.command must not be empty");
        }
        validate_agent_command(&self.agent)?;
        if self.areas.is_empty() && self.invariants.is_empty() {
            anyhow::bail!("config defines no [[area]] and no [[invariant]]; nothing to audit");
        }
        for a in &self.areas {
            if a.globs.is_empty() {
                anyhow::bail!("area '{}' has no globs", a.name);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(command: &str) -> AgentConfig {
        AgentConfig {
            command: command.to_string(),
            args: vec![],
        }
    }

    /// The default command (`claude`) is trusted unconditionally, and a clean
    /// non-default command (letters/digits/dash/underscore/slash/dot/spaces) is
    /// accepted.
    #[test]
    fn agent_command_accepts_default_and_clean() {
        // Default is Ok even though args differ from the built-in default.
        assert!(validate_agent_command(&AgentConfig::default()).is_ok());
        assert!(validate_agent_command(&agent("claude")).is_ok());
        // Clean non-default commands (paths, args-less binaries).
        assert!(validate_agent_command(&agent("/usr/local/bin/claude-audit")).is_ok());
        assert!(validate_agent_command(&agent("./tools/my_agent.sh")).is_ok());
        assert!(validate_agent_command(&agent("uvx some-agent")).is_ok());
    }

    /// A non-default command carrying ANY required shell metacharacter
    /// (`;`, `|`, `&`, backtick, `$(...)`) is rejected.
    #[test]
    fn agent_command_rejects_metachars() {
        for bad in [
            "claude; rm -rf /",
            "claude | tee /tmp/x",
            "claude & curl evil",
            "echo `whoami`",
            "echo $(whoami)",
            "claude > /etc/passwd",
            "claude\nrm -rf /",
        ] {
            assert!(
                validate_agent_command(&agent(bad)).is_err(),
                "expected rejection for {bad:?}"
            );
        }
    }

    /// A bad `agent.command` must make the whole config `validate` (and thus
    /// `Config::load`) return an Err rather than reach the spawn sink.
    #[test]
    fn agent_command_rejection_fails_config_validate() {
        let cfg = Config {
            project: Project {
                name: "p".to_string(),
                root: ".".to_string(),
            },
            agent: agent("claude; rm -rf /"),
            scope: ScopeConfig::default(),
            output: OutputConfig::default(),
            prompt: PromptConfig::default(),
            decisions: DecisionsConfig::default(),
            verify: VerifyConfig::default(),
            areas: vec![Area {
                name: "a".to_string(),
                globs: vec!["src/**".to_string()],
                canon: vec![],
            }],
            invariants: vec![],
        };
        assert!(cfg.validate().is_err());
    }
}
