//! Configuration: defaults, `~/.ctxrot/config.toml`, and env overrides.
//!
//! Env overrides keep compatibility with the Python v1 hook:
//!   GUARD_DISABLE, CLAUDE_CONTEXT_WINDOW, GUARD_LARGE_FILE_BYTES

use std::path::PathBuf;

use serde::Deserialize;

use harness_core::config::{env_bool, env_u64};
// Re-exported so existing `crate::config::expand_tilde` call sites keep working.
pub use harness_core::config::expand_tilde;

#[derive(Debug, Clone)]
pub struct Config {
    pub store_dir: PathBuf,
    pub state_dir: PathBuf,
    pub context_window: u64,
    pub large_file_bytes: u64,
    pub huge_tool_output_bytes: u64,
    /// PostToolUse toolguard: maximum number of "big tool output" nudges to inject
    /// per session before suppressing further ones. The toolguard fights rot by
    /// steering the *next* heavy read, but an uncapped nudge that fires on every
    /// oversized output becomes a rot source itself, so the detector caps its own
    /// repeated advice. Counted across all rot-source keys this session; an
    /// already-nudged key never re-nudges regardless of the cap. 0 disables the
    /// nudge entirely (never advise).
    pub toolguard_nudge_cap: u64,
    /// PreToolUse hard gate: a `Read` of an unbounded (no `limit`) local file at
    /// or above this many bytes is denied, steering the model to a sub-agent.
    /// 0 disables the gate entirely.
    pub gate_file_bytes: u64,
    /// PreToolUse Bash gate (opt-in, default off): deny obviously-unbounded
    /// dumps (`cat huge.log`, `journalctl` with no `-n`, recursive `grep` with no
    /// `-m`, full `tail -n +1`, …) when no downstream bound caps the output.
    /// Heuristic on the command string — conservative to avoid false positives.
    pub gate_bash: bool,
    /// Append one JSONL metrics line per hook event to `<state_dir>/metrics.jsonl`
    /// (budget trajectory, band crossings, note sizes, gate denies). Local only.
    pub metrics: bool,
    /// ascending fractions of the window that trigger escalating advice
    pub bands: Vec<f64>,
    /// Re-anchor (P1): periodically re-surface this session's own Decisions/Open
    /// todos near the end of the window (where attention is strongest) to fight
    /// lost-in-the-middle. Off → never inject the anchor block.
    pub reanchor_enabled: bool,
    /// Minimum band (1-based) at which re-anchor may fire (default 2 ≈ 75%).
    pub reanchor_min_band: usize,
    /// Re-anchor cadence: fire at most once per this many qualifying prompts, so
    /// the block never lands every turn (which would itself accrete rot).
    pub reanchor_every_prompts: u64,
    /// PDO session-anchor re-inject cadence (§4.3): when this session holds a live
    /// `overwatch` lease (a PDO unit: title + done_criteria), re-surface it at most
    /// once per this many qualifying prompts (band ≥ 1). Deliberately SLOWER than
    /// the Decisions re-anchor (the anchor is a single unchanging fact, not growing
    /// project knowledge, so 8 would be excessive). A SEPARATE cooldown key from the
    /// Decisions re-anchor so the two tracks never interfere. Default 12.
    pub anchor_reinject_every: u64,
    /// GC (`ctxrot note prune`): keep at most this many newest notes per project.
    pub keep_notes_per_project: usize,
    /// GC: also protect the newest this-many `distill-*` notes even if they fall
    /// outside `keep_notes_per_project` (distills are higher-value than rescues).
    pub keep_distill_min: usize,
    /// Coalescing: skip a *preemptive* (`band-NN%`) rescue write when this session
    /// already has a rescue note newer than this many seconds. 0 disables.
    pub rescue_coalesce_secs: u64,
    /// Per-turn injection cap (CJK-safe char count): the combined `guard` output
    /// for one prompt is held to at most this many chars. The guard fights rot by
    /// keeping context light, so its OWN injection must be bounded — left
    /// unbounded, large-ref + budget + anchor can stack into a rot source. When
    /// over the cap, blocks are dropped lowest-priority first (anchor → advice →
    /// safety). 0 disables the cap (legacy: inject every block in full).
    pub guard_inject_max_chars: usize,

    // ---- load gate rules (feature ①: rule-based allow/deny) -------------------
    /// Glob patterns whose matching `Read` targets are ALWAYS denied, regardless
    /// of size — "never load these into main context" (logs, vendored dirs,
    /// minified blobs, secrets). Takes precedence over `load_allow` and the size
    /// gate. Empty → no rule denies (only the size gate applies).
    pub load_deny: Vec<String>,
    /// Glob patterns whose matching `Read` targets BYPASS the size gate —
    /// "explicitly trusted, load even if large". Applied only when `load_deny`
    /// did not match. Empty → nothing is force-allowed.
    pub load_allow: Vec<String>,
    /// Whether a `load_deny` match denies even when the `Read` carries an explicit
    /// `limit` (a bounded slice). Default true: a deny rule means "keep this out
    /// of context entirely", so even a slice is refused. false → bounded slices
    /// of denied files are let through.
    pub load_deny_even_with_limit: bool,

    // ---- auto-injection control (feature ③) ----------------------------------
    /// Master switch for the SessionStart carryover injection (`restore`).
    /// false → never inject prior-session carryover.
    pub restore_enabled: bool,
    /// Include the Decisions section in the carryover.
    pub inject_decisions: bool,
    /// Include the Open-todos section in the carryover.
    pub inject_todos: bool,
    /// Append the project's pinned loadset items (as pointers) to the carryover.
    pub inject_pinned: bool,

    // ---- async LLM distill on compaction (feature ④) -------------------------
    /// On by default: when a `/compact` (manual or auto) fires PreCompact, after
    /// the deterministic rescue note lands, spawn a DETACHED background `claude -p`
    /// that distills the full pre-compaction transcript into a high-quality
    /// `distill-*` note. Set `false` (or `CTXROT_DISTILL_ON_COMPACT=0`) to disable
    /// — it spends a model call per compaction.
    /// The next `guard` (UserPromptSubmit) re-injects it so the post-compact
    /// in-session context recovers (PreCompact/PostCompact can't inject). Runs on
    /// the same auth as the session (subscription; no API key).
    pub distill_on_compact: bool,
    /// The headless command used for the async distill. Receives the distill
    /// prompt on stdin and must print ONLY the note markdown on stdout. Default
    /// `"claude -p"`. A value with shell metachars is run via `sh -c`.
    pub distill_cmd: String,
    /// Hard wall-clock cap (seconds) for the background `claude -p` distill. On
    /// timeout the child is killed and the deterministic rescue note (already on
    /// disk) stands as the safety net. The detached worker, not the 10s hook,
    /// bears this wait — so it can be generous.
    pub distill_timeout_secs: u64,
    /// On by default: proactively fire the SAME background `claude -p` distill when
    /// real usage first crosses into the *top* band (≈90%+ of `context_window`,
    /// i.e. the 200k danger line) — without waiting for a `/compact`. Hooks can't
    /// trigger compaction, so this is how "auto-distill at 200k" is realized: the
    /// heavy history is externalized to a `distill-*` note now, and the next
    /// `guard` re-injects the summary so main context trends toward "要約＋リンク".
    /// Actual token release still needs `/compact` (hooks can't evict live tokens).
    /// Set `false` (or `CTXROT_AUTO_DISTILL_ON_BAND=0`) to disable — it spends a
    /// model call per upward crossing into the danger band. Gated independently of
    /// `distill_on_compact` so the on-compact path can stay on while this is off.
    pub auto_distill_on_band: bool,

    // ---- Stop-hook auto-compact (feature ⑤) ------------------------------------
    /// Master switch for the Stop-hook-driven auto-compact trigger.
    /// When true, the `ctxrot stop` handler returns `{"decision":"block"}` when
    /// the budget-meter usage crosses into a new band at/above
    /// `auto_compact_at_percentage`, asking Claude to run `/compact`. Requires
    /// `ctxrot stop` to be wired in hooks.json. The block is bounded (once per
    /// band crossing) so it can never permanently trap a turn.
    /// Default false (opt-in). env: `CTXROT_AUTO_COMPACT=1`
    pub auto_compact_enabled: bool,
    /// Fraction (0.0–1.0) at which the Stop hook triggers the auto-compact nudge.
    /// SEMANTIC (changed in 0.5.0): interpreted against ctxrot's OWN budget meter
    /// — `est_tokens / context_window`, the same estimate `ctxrot guard` /
    /// `ctxrot usage` band from (which can read >100%) — NOT the raw model-window
    /// `context_window.used_percentage`. So 0.90 means "90 % of the configured
    /// budget", which fires far earlier (and correctly) versus the true ~1M
    /// window. Default 0.90. Only meaningful when `auto_compact_enabled` is true.
    /// env: `CTXROT_AUTO_COMPACT_AT_PERCENTAGE`
    pub auto_compact_at_percentage: f64,

    // ---- parent→child Read handoff (`hooks::handoffguard`) ---------------------
    /// Master switch. When true: `Read`s at/above `handoff_min_bytes` are cached
    /// per-session, and a later `Task` this SAME session dispatches has its
    /// prompt enriched with cached content for any resolved path it mentions.
    /// Never crosses sessions (see `hooks::handoffguard` module docs on scope).
    pub handoff_enabled: bool,
    /// A `Read` whose content is at least this many bytes is worth caching (below
    /// this, re-reading is cheap enough that the cache isn't worth the disk IO).
    pub handoff_min_bytes: u64,
    /// Cap on a single cached entry's content (chars), independent of the source
    /// file's real size — bounds the cache's per-entry footprint.
    pub handoff_max_entry_bytes: u64,
    /// Cap on total injected content (chars) for one `Task` dispatch, across all
    /// matched cached files — bounds how much a single subagent prompt can grow.
    pub handoff_max_inject_bytes: u64,
    /// Ring-buffer cap: once the handoff cache file exceeds this many JSONL
    /// lines (across all sessions), the oldest lines are dropped. 0 disables the
    /// cap (unbounded growth — not recommended).
    pub handoff_cache_lines: usize,
}

/// On-disk form (`~/.ctxrot/config.toml`); every field optional.
#[derive(Debug, Default, Deserialize)]
struct FileConfig {
    store_dir: Option<String>,
    state_dir: Option<String>,
    context_window: Option<u64>,
    large_file_bytes: Option<u64>,
    huge_tool_output_bytes: Option<u64>,
    gate_file_bytes: Option<u64>,
    gate_bash: Option<bool>,
    metrics: Option<bool>,
    bands: Option<Vec<f64>>,
    reanchor_enabled: Option<bool>,
    reanchor_min_band: Option<usize>,
    reanchor_every_prompts: Option<u64>,
    anchor_reinject_every: Option<u64>,
    keep_notes_per_project: Option<usize>,
    keep_distill_min: Option<usize>,
    rescue_coalesce_secs: Option<u64>,
    guard_inject_max_chars: Option<usize>,
    load_deny: Option<Vec<String>>,
    load_allow: Option<Vec<String>>,
    load_deny_even_with_limit: Option<bool>,
    restore_enabled: Option<bool>,
    inject_decisions: Option<bool>,
    inject_todos: Option<bool>,
    inject_pinned: Option<bool>,
    distill_on_compact: Option<bool>,
    distill_cmd: Option<String>,
    distill_timeout_secs: Option<u64>,
    auto_distill_on_band: Option<bool>,
    auto_compact_enabled: Option<bool>,
    auto_compact_at_percentage: Option<f64>,
    toolguard_nudge_cap: Option<u64>,
    handoff_enabled: Option<bool>,
    handoff_min_bytes: Option<u64>,
    handoff_max_entry_bytes: Option<u64>,
    handoff_max_inject_bytes: Option<u64>,
    handoff_cache_lines: Option<usize>,
}

/// The `~/.ctxrot` base directory.
fn base_dir() -> PathBuf {
    harness_core::config::base_dir("ctxrot")
}

/// Parse a comma-separated env var into a trimmed, non-empty list, or None when
/// unset/blank. Used for the glob rule lists (`CTXROT_LOAD_DENY/ALLOW`).
fn env_list(key: &str) -> Option<Vec<String>> {
    let raw = std::env::var(key).ok()?;
    let items: Vec<String> = raw
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect();
    if items.is_empty() {
        None
    } else {
        Some(items)
    }
}

impl Default for Config {
    fn default() -> Self {
        let base = base_dir();
        Config {
            store_dir: base.join("store"),
            state_dir: base.join("state"),
            context_window: 200_000,
            large_file_bytes: 50_000,
            huge_tool_output_bytes: 50_000,
            toolguard_nudge_cap: 3,
            gate_file_bytes: 1_000_000,
            gate_bash: false,
            metrics: true,
            bands: vec![0.50, 0.75, 0.90],
            reanchor_enabled: true,
            reanchor_min_band: 2,
            reanchor_every_prompts: 8,
            anchor_reinject_every: 12,
            keep_notes_per_project: 30,
            keep_distill_min: 10,
            rescue_coalesce_secs: 120,
            guard_inject_max_chars: 1200,
            load_deny: Vec::new(),
            load_allow: Vec::new(),
            load_deny_even_with_limit: true,
            restore_enabled: true,
            inject_decisions: true,
            inject_todos: true,
            inject_pinned: true,
            distill_on_compact: true,
            distill_cmd: "claude -p".to_string(),
            distill_timeout_secs: 180,
            auto_distill_on_band: true,
            auto_compact_enabled: false,
            auto_compact_at_percentage: 0.90,
            handoff_enabled: true,
            handoff_min_bytes: 20_000,
            handoff_max_entry_bytes: 200_000,
            handoff_max_inject_bytes: 200_000,
            handoff_cache_lines: 50,
        }
    }
}

impl Config {
    pub fn config_path() -> PathBuf {
        base_dir().join("config.toml")
    }

    /// Load config from disk (if present) layered over defaults, then apply env
    /// overrides. Any read/parse error silently falls back to defaults.
    pub fn load() -> Self {
        let mut cfg = Config::default();

        if let Ok(text) = std::fs::read_to_string(Self::config_path()) {
            if let Ok(fc) = toml::from_str::<FileConfig>(&text) {
                if let Some(v) = fc.store_dir {
                    cfg.store_dir = expand_tilde(&v);
                }
                if let Some(v) = fc.state_dir {
                    cfg.state_dir = expand_tilde(&v);
                }
                if let Some(v) = fc.context_window {
                    cfg.context_window = v;
                }
                if let Some(v) = fc.large_file_bytes {
                    cfg.large_file_bytes = v;
                }
                if let Some(v) = fc.huge_tool_output_bytes {
                    cfg.huge_tool_output_bytes = v;
                }
                if let Some(v) = fc.toolguard_nudge_cap {
                    cfg.toolguard_nudge_cap = v;
                }
                if let Some(v) = fc.gate_file_bytes {
                    cfg.gate_file_bytes = v;
                }
                if let Some(v) = fc.gate_bash {
                    cfg.gate_bash = v;
                }
                if let Some(v) = fc.metrics {
                    cfg.metrics = v;
                }
                if let Some(v) = fc.bands {
                    if !v.is_empty() {
                        cfg.bands = v;
                    }
                }
                if let Some(v) = fc.reanchor_enabled {
                    cfg.reanchor_enabled = v;
                }
                if let Some(v) = fc.reanchor_min_band {
                    cfg.reanchor_min_band = v;
                }
                if let Some(v) = fc.reanchor_every_prompts {
                    cfg.reanchor_every_prompts = v;
                }
                if let Some(v) = fc.anchor_reinject_every {
                    cfg.anchor_reinject_every = v;
                }
                if let Some(v) = fc.keep_notes_per_project {
                    cfg.keep_notes_per_project = v;
                }
                if let Some(v) = fc.keep_distill_min {
                    cfg.keep_distill_min = v;
                }
                if let Some(v) = fc.rescue_coalesce_secs {
                    cfg.rescue_coalesce_secs = v;
                }
                if let Some(v) = fc.guard_inject_max_chars {
                    cfg.guard_inject_max_chars = v;
                }
                if let Some(v) = fc.load_deny {
                    cfg.load_deny = v;
                }
                if let Some(v) = fc.load_allow {
                    cfg.load_allow = v;
                }
                if let Some(v) = fc.load_deny_even_with_limit {
                    cfg.load_deny_even_with_limit = v;
                }
                if let Some(v) = fc.restore_enabled {
                    cfg.restore_enabled = v;
                }
                if let Some(v) = fc.inject_decisions {
                    cfg.inject_decisions = v;
                }
                if let Some(v) = fc.inject_todos {
                    cfg.inject_todos = v;
                }
                if let Some(v) = fc.inject_pinned {
                    cfg.inject_pinned = v;
                }
                if let Some(v) = fc.distill_on_compact {
                    cfg.distill_on_compact = v;
                }
                if let Some(v) = fc.distill_cmd {
                    cfg.distill_cmd = v;
                }
                if let Some(v) = fc.distill_timeout_secs {
                    cfg.distill_timeout_secs = v;
                }
                if let Some(v) = fc.auto_distill_on_band {
                    cfg.auto_distill_on_band = v;
                }
                if let Some(v) = fc.auto_compact_enabled {
                    cfg.auto_compact_enabled = v;
                }
                if let Some(v) = fc.auto_compact_at_percentage {
                    cfg.auto_compact_at_percentage = v;
                }
                if let Some(v) = fc.handoff_enabled {
                    cfg.handoff_enabled = v;
                }
                if let Some(v) = fc.handoff_min_bytes {
                    cfg.handoff_min_bytes = v;
                }
                if let Some(v) = fc.handoff_max_entry_bytes {
                    cfg.handoff_max_entry_bytes = v;
                }
                if let Some(v) = fc.handoff_max_inject_bytes {
                    cfg.handoff_max_inject_bytes = v;
                }
                if let Some(v) = fc.handoff_cache_lines {
                    cfg.handoff_cache_lines = v;
                }
            }
        }

        // env overrides (v1 compatibility)
        if let Some(v) = env_u64("CLAUDE_CONTEXT_WINDOW") {
            cfg.context_window = v;
        }
        if let Some(v) = env_u64("GUARD_LARGE_FILE_BYTES") {
            cfg.large_file_bytes = v;
        }
        if let Some(v) = env_u64("GUARD_GATE_FILE_BYTES") {
            cfg.gate_file_bytes = v;
        }
        if let Some(v) = env_bool("GUARD_METRICS") {
            cfg.metrics = v;
        }
        if let Some(v) = env_bool("GUARD_GATE_BASH") {
            cfg.gate_bash = v;
        }
        if let Some(v) = env_u64("GUARD_INJECT_MAX_CHARS") {
            cfg.guard_inject_max_chars = v as usize;
        }
        if let Some(v) = env_u64("CTXROT_TOOLGUARD_NUDGE_CAP") {
            cfg.toolguard_nudge_cap = v;
        }
        if let Some(v) = env_list("CTXROT_LOAD_DENY") {
            cfg.load_deny = v;
        }
        if let Some(v) = env_list("CTXROT_LOAD_ALLOW") {
            cfg.load_allow = v;
        }
        if let Some(v) = env_bool("CTXROT_RESTORE_DISABLE") {
            // Convenience kill-switch: CTXROT_RESTORE_DISABLE=1 turns carryover off.
            cfg.restore_enabled = !v;
        }
        if let Some(v) = env_bool("CTXROT_DISTILL_ON_COMPACT") {
            cfg.distill_on_compact = v;
        }
        if let Ok(v) = std::env::var("CTXROT_DISTILL_CMD") {
            if !v.trim().is_empty() {
                cfg.distill_cmd = v;
            }
        }
        if let Some(v) = env_u64("CTXROT_DISTILL_TIMEOUT_SECS") {
            cfg.distill_timeout_secs = v;
        }
        if let Some(v) = env_bool("CTXROT_AUTO_DISTILL_ON_BAND") {
            cfg.auto_distill_on_band = v;
        }
        if let Some(v) = env_bool("CTXROT_AUTO_COMPACT") {
            cfg.auto_compact_enabled = v;
        }
        if let Ok(v) = std::env::var("CTXROT_AUTO_COMPACT_AT_PERCENTAGE") {
            if let Ok(f) = v.trim().parse::<f64>() {
                cfg.auto_compact_at_percentage = f;
            }
        }
        if let Some(v) = env_bool("CTXROT_HANDOFF_ENABLE") {
            cfg.handoff_enabled = v;
        }
        if let Some(v) = env_u64("CTXROT_HANDOFF_MIN_BYTES") {
            cfg.handoff_min_bytes = v;
        }
        if let Some(v) = env_u64("CTXROT_HANDOFF_MAX_ENTRY_BYTES") {
            cfg.handoff_max_entry_bytes = v;
        }
        if let Some(v) = env_u64("CTXROT_HANDOFF_MAX_INJECT_BYTES") {
            cfg.handoff_max_inject_bytes = v;
        }
        if let Some(v) = env_u64("CTXROT_HANDOFF_CACHE_LINES") {
            cfg.handoff_cache_lines = v as usize;
        }

        // bands must be ascending and within (0,1]; sanitize defensively
        cfg.bands.retain(|b| *b > 0.0 && *b <= 1.0);
        cfg.bands
            .sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        if cfg.bands.is_empty() {
            cfg.bands = vec![0.50, 0.75, 0.90];
        }
        if cfg.context_window == 0 {
            cfg.context_window = 200_000;
        }
        // Re-anchor needs a sane band floor (≥1) and a non-zero cadence, else it
        // would fire on every band-0 prompt / every turn.
        if cfg.reanchor_min_band == 0 {
            cfg.reanchor_min_band = 1;
        }
        if cfg.reanchor_every_prompts == 0 {
            cfg.reanchor_every_prompts = 8;
        }
        // The PDO session-anchor re-inject cadence must be non-zero, else it would
        // fire every qualifying prompt (the anchor is a stable fact, not per-turn).
        if cfg.anchor_reinject_every == 0 {
            cfg.anchor_reinject_every = 12;
        }
        // A zero/blank distill command or timeout would make the async distill a
        // silent no-op; fall back to sane defaults so opting in actually runs.
        if cfg.distill_cmd.trim().is_empty() {
            cfg.distill_cmd = "claude -p".to_string();
        }
        if cfg.distill_timeout_secs == 0 {
            cfg.distill_timeout_secs = 180;
        }
        cfg
    }

    /// Hook is globally disabled via env.
    pub fn disabled() -> bool {
        std::env::var("GUARD_DISABLE")
            .map(|v| !v.is_empty())
            .unwrap_or(false)
    }

    /// Band index (1-based) for a usage fraction; 0 means "below the lowest band".
    pub fn band_for(&self, frac: f64) -> usize {
        let mut band = 0;
        for (i, lo) in self.bands.iter().enumerate() {
            if frac >= *lo {
                band = i + 1;
            }
        }
        band
    }
}

/// Serializes tests that mutate the process-global `HOME` env var — used by
/// `main.rs`'s violation-emission tests, which need a controlled `$HOME`
/// because `overwatch::store`'s violation log resolves its storage root off
/// it. `cargo test` runs a crate's unit tests on multiple threads by default;
/// two such tests racing without a shared lock corrupts each other's view of
/// "where does the store live" (same failure mode documented on
/// donegate's/reviewgate's identically-named lock, crates/donegate/src/config.rs).
#[cfg(test)]
pub(crate) static HOME_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distill_on_compact_defaults_on() {
        // Regression guard: async LLM distill on compaction is ON by default.
        // Flipping this back to false silently strips post-compact distill quality
        // for every user who hasn't written a config.toml, so pin it here.
        let cfg = Config::default();
        assert!(cfg.distill_on_compact);
        assert_eq!(cfg.distill_cmd, "claude -p");
        assert_eq!(cfg.distill_timeout_secs, 180);
    }

    #[test]
    fn toolguard_nudge_cap_defaults_nonzero() {
        // Regression guard: the per-session toolguard nudge cap defaults to a
        // sane non-zero value so repeated big-output nudges are bounded out of the
        // box. A default of 0 would read as "never nudge" and silently disable the
        // toolguard's steering for every user without a config.toml.
        let cfg = Config::default();
        assert_eq!(cfg.toolguard_nudge_cap, 3);
        assert!(cfg.toolguard_nudge_cap > 0);
    }

    #[test]
    fn auto_distill_on_band_defaults_on() {
        // Regression guard: proactive band-3 (≈200k) auto-distill is ON by default,
        // so "auto-distill at 200k" works without a config.toml. Flipping this to
        // false silently removes the proactive externalize-before-compact behavior.
        assert!(Config::default().auto_distill_on_band);
    }

    #[test]
    fn auto_compact_defaults() {
        // auto-compact Stop-hook nudge is OFF by default (opt-in) so existing users
        // are not surprised by a new block-on-stop behaviour after upgrade.
        let cfg = Config::default();
        assert!(!cfg.auto_compact_enabled);
        assert!((cfg.auto_compact_at_percentage - 0.90).abs() < f64::EPSILON);
    }

    #[test]
    fn auto_compact_env_override() {
        // CTXROT_AUTO_COMPACT_AT_PERCENTAGE overrides the threshold.
        std::env::set_var("CTXROT_AUTO_COMPACT_AT_PERCENTAGE", "0.75");
        let cfg = Config::load();
        std::env::remove_var("CTXROT_AUTO_COMPACT_AT_PERCENTAGE");
        assert!((cfg.auto_compact_at_percentage - 0.75).abs() < f64::EPSILON);
    }
}
