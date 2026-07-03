//! Runtime configuration: defaults <- ~/.condukt/config.toml <- environment.
//!
//! Everything is generic and project-agnostic. The AEGIS-specific notion of
//! "never parallelize these shared files" lives entirely in `shared_globs`, so
//! a user configures it per project rather than us hardcoding model.py/glossary.

use serde::Deserialize;
use std::path::PathBuf;

// Re-exported so existing `crate::config::expand_tilde` call sites keep working.
pub use harness_core::config::expand_tilde;

pub struct Config {
    /// Where worktrees are created (must be outside the repo).
    pub worktree_base: PathBuf,
    /// Branch to merge completed work back into.
    pub default_branch: String,
    /// Globs that force a touching task to run serially (never in parallel).
    pub shared_globs: Vec<String>,
    /// Soft cap on concurrent workers (advisory; the skill honors it).
    pub max_parallel: usize,
    /// Where run-state files are stored.
    pub state_dir: PathBuf,
    /// Override command for `condukt state test` (None = auto-detect).
    pub test_command: Option<String>,
    /// How many seconds a Running task may be silent before being considered STUCK.
    /// Defaults to 1800 (30 minutes).
    pub stuck_ttl_secs: u64,
    /// Loop feature: command to build artifacts before testing (client/e2e cycles).
    pub build_command: Option<String>,
    /// Loop feature: command to deploy before testing (server/e2e cycles).
    pub deploy_command: Option<String>,
    /// Loop feature: max iterations before the loop gives up. Defaults to 10.
    pub loop_max_iters: usize,
    /// Autonomy mode: when true, the /condukt skill downgrades its human gates
    /// (e.g. the Phase 3 decomposition agreement) to deterministic defaults so the
    /// loop can run with no user approval beyond genuinely-needed information.
    /// Defaults to false (every existing AskUserQuestion still fires — fully
    /// backward compatible). Read by `condukt state autonomy-check`.
    pub autonomous: bool,
    /// Multi-sample self-consistency (cost guard). When true, `condukt consensus
    /// plan` fans a task out into N candidate implementations, verifies each, and
    /// takes a majority vote. OFF by default: N-sample generation is N× the cost,
    /// so it is opt-in per project (or per high-risk task via `--risk high`).
    /// Read by `condukt consensus plan`. Overridable via `CONDUKT_CONSENSUS`.
    pub consensus_enabled: bool,
    /// Fan-out width when consensus is enabled. Defaults to 3; clamped to a
    /// documented ceiling (`consensus::MAX_SAMPLES`) so it can never run away.
    pub consensus_samples: usize,
    /// Agreement threshold below which a task escalates to opus. Defaults to 0.5.
    pub consensus_threshold: f64,
    /// Single-worktree execution mode. When true, the /condukt skill runs ALL
    /// tasks in the main repo working tree instead of creating one isolated
    /// worktree+branch per parallel task. File-conflicting tasks still serialize
    /// (schedule.rs already forces that); non-conflicting tasks run concurrently
    /// in the one tree, each staging ONLY its own `touched_files` (`git add
    /// <files>`, not `-A`) so peers' in-flight edits are never swept into a
    /// commit — and the per-task merge/remove dance (Phase 7) is skipped entirely.
    /// OFF by default (every existing run keeps per-task worktree isolation —
    /// fully backward compatible). Read by `condukt state worktree-mode-check`.
    /// Overridable via `CONDUKT_SINGLE_WORKTREE`.
    pub single_worktree: bool,
    /// Opt-in worker sandboxing. When true, the /condukt skill routes a worker's
    /// build/test commands through the existing docker exec backend
    /// (`verify::launch_in_container`, `--network=none`) instead of running them
    /// directly on the host, adding filesystem + resource isolation. OFF by
    /// default (worker keeps host-direct execution — fully backward compatible).
    /// Read by `condukt sandbox` wiring. Overridable via `CONDUKT_WORKER_SANDBOX`.
    pub worker_sandbox_enabled: bool,
    /// Container image used for sandboxed worker execution (None = the launch
    /// backend's default image). Overridable via `CONDUKT_WORKER_SANDBOX_IMAGE`.
    pub worker_sandbox_image: Option<String>,
    /// `--memory` limit for the sandbox container (e.g. "512m"; None = no cap).
    pub worker_sandbox_memory: Option<String>,
    /// `--cpus` limit for the sandbox container (e.g. "1.5"; None = no cap).
    pub worker_sandbox_cpus: Option<String>,
    /// `--pids-limit` for the sandbox container (e.g. 256; None = no cap).
    pub worker_sandbox_pids_limit: Option<i64>,
}

/// Which test-fix cycle sequence to use for a module type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModuleCycle {
    /// deploy → test
    Server,
    /// build → test
    Client,
    /// build → deploy → test
    E2e,
}

impl ModuleCycle {
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "server" => Some(Self::Server),
            "client" => Some(Self::Client),
            "e2e" => Some(Self::E2e),
            _ => None,
        }
    }

    #[allow(dead_code)]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Server => "server",
            Self::Client => "client",
            Self::E2e => "e2e",
        }
    }
}

#[derive(Default, Deserialize)]
struct FileTestConfig {
    command: Option<String>,
}

#[derive(Default, Deserialize)]
struct FileLoopConfig {
    build_command: Option<String>,
    deploy_command: Option<String>,
    max_iters: Option<usize>,
}

#[derive(Default, Deserialize)]
struct FileConsensusConfig {
    enabled: Option<bool>,
    samples: Option<usize>,
    threshold: Option<f64>,
}

#[derive(Default, Deserialize)]
struct FileWorkerConfig {
    sandbox_enabled: Option<bool>,
    docker_image: Option<String>,
    memory_limit: Option<String>,
    cpus: Option<String>,
    pids_limit: Option<i64>,
}

#[derive(Default, Deserialize)]
struct FileConfig {
    worktree_base: Option<String>,
    default_branch: Option<String>,
    shared_globs: Option<Vec<String>>,
    max_parallel: Option<usize>,
    state_dir: Option<String>,
    test: Option<FileTestConfig>,
    stuck_ttl_secs: Option<u64>,
    autonomous: Option<bool>,
    #[serde(rename = "loop")]
    loop_cfg: Option<FileLoopConfig>,
    consensus: Option<FileConsensusConfig>,
    single_worktree: Option<bool>,
    worker: Option<FileWorkerConfig>,
}

/// Parse the `CONDUKT_AUTONOMOUS` env override. Accepts common truthy/falsy
/// spellings so the switch can be forced either way from the environment
/// (overriding config.toml). Unrecognized values leave config untouched.
fn parse_autonomous_env(raw: &str) -> Option<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

/// `~/.condukt` (falls back to `./.condukt` if there is no home dir). Thin
/// wrapper over the shared base-dir resolution.
pub fn base_dir() -> PathBuf {
    harness_core::config::base_dir("condukt")
}

impl Config {
    pub fn load() -> Self {
        let base = base_dir();
        let mut cfg = Config {
            worktree_base: base.join("worktrees"),
            default_branch: "main".to_string(),
            shared_globs: Vec::new(),
            max_parallel: 4,
            state_dir: base.join("state"),
            test_command: None,
            stuck_ttl_secs: 1800,
            build_command: None,
            deploy_command: None,
            loop_max_iters: 10,
            autonomous: false,
            consensus_enabled: false,
            consensus_samples: crate::consensus::DEFAULT_SAMPLES,
            consensus_threshold: crate::consensus::DEFAULT_THRESHOLD,
            single_worktree: false,
            worker_sandbox_enabled: false,
            worker_sandbox_image: None,
            worker_sandbox_memory: None,
            worker_sandbox_cpus: None,
            worker_sandbox_pids_limit: None,
        };

        if let Ok(txt) = std::fs::read_to_string(base.join("config.toml")) {
            if let Ok(fc) = toml::from_str::<FileConfig>(&txt) {
                if let Some(v) = fc.worktree_base {
                    cfg.worktree_base = expand_tilde(&v);
                }
                if let Some(v) = fc.default_branch {
                    cfg.default_branch = v;
                }
                if let Some(v) = fc.shared_globs {
                    cfg.shared_globs = v;
                }
                if let Some(v) = fc.max_parallel {
                    cfg.max_parallel = v;
                }
                if let Some(v) = fc.state_dir {
                    cfg.state_dir = expand_tilde(&v);
                }
                if let Some(t) = fc.test {
                    cfg.test_command = t.command;
                }
                if let Some(v) = fc.stuck_ttl_secs {
                    cfg.stuck_ttl_secs = v;
                }
                if let Some(v) = fc.autonomous {
                    cfg.autonomous = v;
                }
                if let Some(lc) = fc.loop_cfg {
                    if let Some(v) = lc.build_command {
                        cfg.build_command = Some(v);
                    }
                    if let Some(v) = lc.deploy_command {
                        cfg.deploy_command = Some(v);
                    }
                    if let Some(v) = lc.max_iters {
                        cfg.loop_max_iters = v;
                    }
                }
                if let Some(cc) = fc.consensus {
                    if let Some(v) = cc.enabled {
                        cfg.consensus_enabled = v;
                    }
                    if let Some(v) = cc.samples {
                        cfg.consensus_samples = v;
                    }
                    if let Some(v) = cc.threshold {
                        cfg.consensus_threshold = v;
                    }
                }
                if let Some(v) = fc.single_worktree {
                    cfg.single_worktree = v;
                }
                if let Some(w) = fc.worker {
                    if let Some(v) = w.sandbox_enabled {
                        cfg.worker_sandbox_enabled = v;
                    }
                    if let Some(v) = w.docker_image {
                        cfg.worker_sandbox_image = Some(v);
                    }
                    if let Some(v) = w.memory_limit {
                        cfg.worker_sandbox_memory = Some(v);
                    }
                    if let Some(v) = w.cpus {
                        cfg.worker_sandbox_cpus = Some(v);
                    }
                    if let Some(v) = w.pids_limit {
                        cfg.worker_sandbox_pids_limit = Some(v);
                    }
                }
            }
        }

        if let Ok(v) = std::env::var("CONDUKT_WORKTREE_BASE") {
            cfg.worktree_base = expand_tilde(&v);
        }
        if let Ok(v) = std::env::var("CONDUKT_DEFAULT_BRANCH") {
            cfg.default_branch = v;
        }
        if let Ok(v) = std::env::var("CONDUKT_MAX_PARALLEL") {
            if let Ok(n) = v.parse() {
                cfg.max_parallel = n;
            }
        }
        if let Ok(v) = std::env::var("CONDUKT_STUCK_TTL_SECS") {
            if let Ok(n) = v.parse() {
                cfg.stuck_ttl_secs = n;
            }
        }
        if let Ok(v) = std::env::var("CONDUKT_AUTONOMOUS") {
            if let Some(b) = parse_autonomous_env(&v) {
                cfg.autonomous = b;
            }
        }
        // Reuses the generic truthy/falsy parser (despite its name) to force the
        // self-consistency switch from the environment, overriding config.toml.
        if let Ok(v) = std::env::var("CONDUKT_CONSENSUS") {
            if let Some(b) = parse_autonomous_env(&v) {
                cfg.consensus_enabled = b;
            }
        }
        // Reuses the generic truthy/falsy parser to force single-worktree mode
        // from the environment, overriding config.toml.
        if let Ok(v) = std::env::var("CONDUKT_SINGLE_WORKTREE") {
            if let Some(b) = parse_autonomous_env(&v) {
                cfg.single_worktree = b;
            }
        }
        // Reuses the generic truthy/falsy parser to force worker sandboxing from
        // the environment, overriding config.toml.
        if let Ok(v) = std::env::var("CONDUKT_WORKER_SANDBOX") {
            if let Some(b) = parse_autonomous_env(&v) {
                cfg.worker_sandbox_enabled = b;
            }
        }
        if let Ok(v) = std::env::var("CONDUKT_WORKER_SANDBOX_IMAGE") {
            if !v.trim().is_empty() {
                cfg.worker_sandbox_image = Some(v);
            }
        }
        cfg
    }

    /// Global kill switch for the hooks (`CONDUKT_DISABLE=1`).
    pub fn disabled() -> bool {
        std::env::var("CONDUKT_DISABLE")
            .map(|v| v == "1")
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn module_cycle_roundtrip() {
        for (s, expected) in [
            ("server", ModuleCycle::Server),
            ("client", ModuleCycle::Client),
            ("e2e", ModuleCycle::E2e),
        ] {
            let got = ModuleCycle::from_str(s).expect("should parse");
            assert_eq!(got, expected);
            assert_eq!(got.as_str(), s);
        }
    }

    #[test]
    fn module_cycle_unknown_returns_none() {
        assert!(ModuleCycle::from_str("unknown").is_none());
        assert!(ModuleCycle::from_str("").is_none());
    }

    #[test]
    fn loop_config_parses_from_toml() {
        let toml = r#"
[loop]
build_command = "npm run build"
deploy_command = "kubectl rollout restart deployment/api"
max_iters = 5
"#;
        let fc: FileConfig = toml::from_str(toml).expect("should parse");
        let lc = fc.loop_cfg.expect("loop section present");
        assert_eq!(lc.build_command.as_deref(), Some("npm run build"));
        assert_eq!(
            lc.deploy_command.as_deref(),
            Some("kubectl rollout restart deployment/api")
        );
        assert_eq!(lc.max_iters, Some(5));
    }

    #[test]
    fn loop_config_defaults_when_absent() {
        let toml = "";
        let fc: FileConfig = toml::from_str(toml).expect("should parse");
        assert!(fc.loop_cfg.is_none());
    }

    #[test]
    fn autonomous_parses_from_toml() {
        let fc: FileConfig = toml::from_str("autonomous = true").expect("should parse");
        assert_eq!(fc.autonomous, Some(true));
    }

    #[test]
    fn autonomous_defaults_none_when_absent() {
        let fc: FileConfig = toml::from_str("").expect("should parse");
        assert_eq!(fc.autonomous, None);
    }

    #[test]
    fn single_worktree_parses_from_toml() {
        let fc: FileConfig = toml::from_str("single_worktree = true").expect("should parse");
        assert_eq!(fc.single_worktree, Some(true));
    }

    #[test]
    fn single_worktree_defaults_none_when_absent() {
        let fc: FileConfig = toml::from_str("").expect("should parse");
        assert_eq!(fc.single_worktree, None);
    }

    #[test]
    fn worker_config_parses_from_toml() {
        let toml = r#"
[worker]
sandbox_enabled = true
docker_image = "alpine:3.20"
memory_limit = "512m"
cpus = "1.5"
pids_limit = 256
"#;
        let fc: FileConfig = toml::from_str(toml).expect("should parse");
        let w = fc.worker.expect("worker section present");
        assert_eq!(w.sandbox_enabled, Some(true));
        assert_eq!(w.docker_image.as_deref(), Some("alpine:3.20"));
        assert_eq!(w.memory_limit.as_deref(), Some("512m"));
        assert_eq!(w.cpus.as_deref(), Some("1.5"));
        assert_eq!(w.pids_limit, Some(256));
    }

    #[test]
    fn worker_config_defaults_none_when_absent() {
        let fc: FileConfig = toml::from_str("").expect("should parse");
        assert!(fc.worker.is_none());
    }

    #[test]
    fn worker_sandbox_env_uses_truthy_parser() {
        // CONDUKT_WORKER_SANDBOX is resolved through the shared truthy/falsy
        // parser (same mechanism as CONDUKT_AUTONOMOUS / CONDUKT_SINGLE_WORKTREE).
        assert_eq!(parse_autonomous_env("1"), Some(true));
        assert_eq!(parse_autonomous_env("on"), Some(true));
        assert_eq!(parse_autonomous_env("off"), Some(false));
        assert_eq!(parse_autonomous_env("maybe"), None);
    }

    #[test]
    fn autonomous_env_truthy_and_falsy() {
        for v in ["1", "true", "TRUE", "yes", "on", " True "] {
            assert_eq!(parse_autonomous_env(v), Some(true), "{v:?} should be true");
        }
        for v in ["0", "false", "No", "off"] {
            assert_eq!(
                parse_autonomous_env(v),
                Some(false),
                "{v:?} should be false"
            );
        }
        for v in ["", "maybe", "2", "enabled"] {
            assert_eq!(parse_autonomous_env(v), None, "{v:?} should be None");
        }
    }
}
