// テスト内の unwrap/expect は意図的な assert であって fail-open ではないので許可する。
// production 側は workspace の [workspace.lints.clippy] で deny のまま。
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]
//! specguard — project-agnostic spec/implementation drift audit harness.
//!
//! Reads a TOML config describing a project's canon (areas, canon pointers,
//! invariants), resolves a change-triggered scope from `git diff`, renders a
//! read-only audit prompt, drives an LLM agent to produce findings, and
//! persists a report plus a sentinel when something needs human review.
//!
//! The judgment lives in the agent (it reads the live canon and quotes it
//! verbatim); this binary is the deterministic harness around it.

mod agent;
mod auditmap;
mod config;
mod decision;
mod init;
mod parse;
mod prompt;
mod ratify;
mod report;
mod scope;
mod similarity;
mod specmap;
mod testaudit;
mod verify;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use config::Config;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// Exit codes (stable contract for schedulers/hooks). These are specguard's own
/// and must stay disjoint from anything the agent might return: an agent failure
/// always maps to [`EXIT_AGENT_FAILED`] (its real code goes to stderr) rather
/// than being propagated raw, so a caller can never confuse "agent exited 3"
/// with "no marker".
const EXIT_OK: u8 = 0;
const EXIT_USAGE: u8 = 2;
const EXIT_NO_MARKER: u8 = 3;
const EXIT_AGENT_FAILED: u8 = 4;
/// The prompt (meta-canon) is unratified or changed since ratification, and
/// `[prompt].require_ratification` is on — a human must `accept-prompt` first.
const EXIT_UNRATIFIED: u8 = 5;
/// `specguard ack` was called but no new commit was found since the sentinel
/// was raised. Pass `--force` to override.
const EXIT_NO_FIX_COMMIT: u8 = 6;
/// `specguard testaudit` found one or more tests that are not being run.
const EXIT_TESTAUDIT_FINDINGS: u8 = 7;
/// `specguard testaudit` could NOT determine the answer — a directory or `.rs`
/// file that exists but is unreadable made the scan incomplete. Distinct from
/// [`EXIT_TESTAUDIT_FINDINGS`] (a real finding) and never [`EXIT_OK`]: an
/// incomplete scan must fail closed (cannot-determine → RED), never masquerade
/// as "no skipped tests". Twin doctrine to [`EXIT_AGENT_FAILED`].
const EXIT_TESTAUDIT_UNDETERMINED: u8 = 8;

#[derive(Parser)]
#[command(
    name = "specguard",
    version,
    about = "Spec/implementation drift audit harness"
)]
struct Cli {
    /// Path to the config file.
    #[arg(short, long, default_value = "specguard.toml", global = true)]
    config: PathBuf,

    /// Override the baseline ref (also via SPECGUARD_BASELINE_REF).
    #[arg(short, long, global = true)]
    baseline: Option<String>,

    /// Override the run date (YYYY-MM-DD); default is today (also SPECGUARD_NOW).
    #[arg(long, global = true)]
    date: Option<String>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Run the full audit (default).
    Run,
    /// Print the resolved scope only (no agent call).
    Scope,
    /// Print the rendered per-shard prompts only (no agent call).
    Prompt {
        /// Emit a machine-readable JSON envelope ({project, baseline, head, date,
        /// marker, shards:[{label, prompt}]}) for subscription-native orchestration
        /// (Claude Code plugin) instead of the human debug view.
        #[arg(long)]
        json: bool,
    },
    /// Ingest pre-collected per-shard agent outputs (JSON on stdin, or `--from`)
    /// and run the parse→report→sentinel pipeline WITHOUT spawning agents. This is
    /// the subscription-native counterpart to `run`: the Claude Code plugin gets
    /// shard prompts via `prompt --json`, dispatches each to a read-only in-session
    /// subagent (billed to the host subscription, no nested `claude --print`), then
    /// feeds the outputs back here. Same exit codes as `run`.
    Ingest {
        /// Read the outputs JSON from a file instead of stdin.
        #[arg(long)]
        from: Option<PathBuf>,
    },
    /// Print the active fix-offer block if a sentinel is pending (for the
    /// SessionStart hook). Resolves the sentinel path from `[output].sentinel`,
    /// so a custom path still works. Never fails the session: any error (missing
    /// config etc.) prints nothing and exits 0.
    Pending,
    /// Clear the sentinel after a human has handled the pending findings.
    ///
    /// By default, requires at least one new git commit since the sentinel was
    /// raised (proving a fix was made). Use `--force` to bypass that check.
    Ack {
        /// Skip the "no fix commit found" guard and clear the sentinel anyway.
        #[arg(long)]
        force: bool,
    },
    /// Scaffold specguard into a repo: starter config + Claude Code SessionStart
    /// hook. Idempotent; existing config kept unless `--force`.
    Init {
        /// Overwrite an existing config file.
        #[arg(long)]
        force: bool,
    },
    /// Pre-task spec briefing (read-only): before you start a task, summarize the
    /// canon rules and invariants it touches — to prevent drift before it happens
    /// (the front-line counterpart to `run`'s post-hoc audit).
    Brief {
        /// The task you're about to start (free text).
        task: String,
        /// Print the rendered briefing prompt only (no agent); used by the plugin
        /// to dispatch it to a read-only subagent.
        #[arg(long)]
        prompt: bool,
    },
    /// Scaffold a decision record (ADR) pinned to the current canon commit.
    Decide {
        /// Title of the decision (becomes part of the record id).
        title: String,
        /// Overwrite an existing record with the same id.
        #[arg(long)]
        force: bool,
    },
    /// Scan the repository for tests that are implemented but not being run:
    /// #[ignore]'d tests, cfg-gated tests that are always excluded, .rs files
    /// with tests that are not `mod`-declared by a parent, and integration-test
    /// files. Exits 0 when clean, 7 when findings are found.
    #[command(name = "testaudit")]
    TestAudit {
        /// Emit machine-readable JSON ({findings:[{kind,file,name,reason}]})
        /// instead of human text.
        #[arg(long)]
        json: bool,
    },
    /// Ratify the prompt templates (meta-canon) after reviewing them: contract-
    /// check, then pin the current version with a rationale. Required before a
    /// gated `run` when `[prompt].require_ratification = true`.
    AcceptPrompt {
        /// Why this prompt version is accepted (recorded in the lock).
        #[arg(short = 'm', long = "reason")]
        reason: String,
    },
    /// Maintain the independent file→spec mapping store (see `specmap.rs`). This
    /// persisted map (source file → spec-doc + status) is reusable by future
    /// features (spec-audit, drift-map) and is NOT tied to the drift workflow.
    Map {
        #[command(subcommand)]
        action: MapAction,
    },
    /// Map-driven CORRECTNESS audit (read-only). Distinct from the drift audit:
    /// drift checks whether spec and impl AGREE (consistency); `audit` checks
    /// whether the implementation and the spec are actually RIGHT — spec
    /// soundness, implementation correctness, and test adequacy — scoped by the
    /// persisted spec-map store (`specmap.rs`). It emits deterministic structural
    /// findings (undocumented / dangling reference / untested) plus per-entry LLM
    /// audit shards; it fixes nothing (Human-on-the-loop). `--json` emits the
    /// same envelope shape as `prompt --json` so the outputs can be fed back
    /// through `specguard ingest`.
    Audit {
        /// Emit the machine-readable JSON envelope ({project, baseline, head,
        /// date, marker, shards:[{label, prompt}]}) for the plugin to dispatch to
        /// read-only subagents (then feed back via `ingest`), instead of the
        /// human summary.
        #[arg(long)]
        json: bool,
        /// Restrict the audit to entries whose key, spec_doc, any impl/test path,
        /// or api route contains this (case-insensitive) substring — to scope to a
        /// specific command/crate/API (e.g. `--filter drift-map`,
        /// `--filter crates/specguard`, `--filter /health`). Omitted → whole map.
        #[arg(long)]
        filter: Option<String>,
    },
}

#[derive(Subcommand)]
enum MapAction {
    /// Create the map if absent, then reconcile it against the full baseline
    /// window (`baseline_ref` else `fallback_ref`, ignoring the recorded
    /// `.last-ref`) — a from-scratch seed of the map.
    Build,
    /// Incrementally reconcile the existing map against the resolved baseline
    /// (`--baseline`/env override > `baseline_ref` > recorded `.last-ref` >
    /// `fallback_ref`), the same precedence the audit uses.
    Sync,
    /// Print the current map. `--json` emits the machine-readable store.
    List {
        /// Emit the map as JSON instead of human-readable text.
        #[arg(long)]
        json: bool,
        /// Restrict the listing to entries whose key, spec_doc, any impl/test
        /// path, or api route contains this (case-insensitive) substring — to
        /// scope to a specific command/crate/API (e.g. `--filter drift-map`,
        /// `--filter crates/specguard`, `--filter /health`), sharing the same
        /// targeting predicate as `audit --filter`. Omitted → whole map.
        #[arg(long)]
        filter: Option<String>,
    },
    /// Attach a spec-doc to matching entries and mark them `tracked` — the
    /// resolution for mapped source files that now have an authored spec.
    /// `selector` is an exact entry key or a glob (e.g. `crates/foo/src/**`), so
    /// one crate-level spec-doc can resolve every per-file entry under it.
    SetSpec {
        /// Exact entry key or glob selecting the entries to attach the doc to.
        selector: String,
        /// Spec-doc path (repo-root-relative) to record on each matched entry.
        doc: String,
    },
    /// Mark matching entries `tracked` (reviewed; no authored spec needed).
    /// `selector` is an exact entry key or a glob. Use for entries whose
    /// `changed` status has been reviewed and reflects no genuine spec drift.
    Resolve {
        /// Exact entry key or glob selecting the entries to mark tracked.
        selector: String,
    },
    /// Remove entries whose key matches the configured `[map].exclude` globs —
    /// the non-spec-bearing paths (lockfiles, manifests, generated artifacts,
    /// docs). Idempotent. `build`/`sync` also apply exclusion, so this mainly
    /// cleans a map seeded before `exclude` was configured.
    Prune,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(&cli) {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            eprintln!("specguard: error: {e:#}");
            ExitCode::from(EXIT_USAGE)
        }
    }
}

struct Loaded {
    cfg: Config,
    repo_root: PathBuf,
    template: String,
    date: String,
}

/// Load config + resolve repo root, template and date (shared by all commands).
fn load(cli: &Cli) -> Result<Loaded> {
    let cfg = Config::load(&cli.config)?;
    let config_dir = cli
        .config
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let repo_root = canonicalize(&config_dir.join(&cfg.project.root))
        .with_context(|| "resolving project.root")?;
    let template = load_template(&cfg, config_dir)?;
    let date = resolve_date(cli.date.clone());
    Ok(Loaded {
        cfg,
        repo_root,
        template,
        date,
    })
}

fn run(cli: &Cli) -> Result<u8> {
    // `init` runs before config load: the config may not exist yet.
    if let Some(Command::Init { force }) = &cli.command {
        init::run(&cli.config, *force)?;
        return Ok(EXIT_OK);
    }

    // `pending` is the SessionStart hook entry point: best-effort, never errors.
    if let Some(Command::Pending) = &cli.command {
        return Ok(pending(cli));
    }

    let l = load(cli)?;
    let paths = report::paths(&l.cfg, &l.repo_root, &l.date);

    // `ack` only touches the sentinel; no scope/agent work needed.
    if let Some(Command::Ack { force }) = cli.command {
        return ack(&paths, &l.repo_root, force);
    }

    // `testaudit` scans for tests not being run; no agent work.
    if let Some(Command::TestAudit { json }) = cli.command {
        return run_testaudit(&l.repo_root, json);
    }

    // `decide` scaffolds a decision record pinned to the current canon commit.
    if let Some(Command::Decide { title, force }) = &cli.command {
        return decide(&l, title, *force);
    }

    // `accept-prompt` ratifies the prompt (meta-canon): contract-check + pin.
    if let Some(Command::AcceptPrompt { reason }) = &cli.command {
        return accept_prompt(&l, reason);
    }

    // `brief` is a read-only pre-task briefing; no scope/git resolution needed.
    if let Some(Command::Brief { task, prompt }) = &cli.command {
        return brief(&l, task, *prompt);
    }

    // `map` maintains the independent file→spec mapping store; it resolves the
    // baseline the same way the audit does but does no agent/scope work.
    if let Some(Command::Map { action }) = &cli.command {
        return run_map(cli, &l, action);
    }

    // `audit` is the read-only, map-driven CORRECTNESS audit. It consumes the
    // spec-map store (not the git-diff scope) and resolves the baseline the same
    // way the audit does, but does no agent work here — it emits shards/findings.
    if let Some(Command::Audit { json, filter }) = &cli.command {
        return run_audit(cli, &l, &paths, *json, filter.as_deref().unwrap_or(""));
    }

    let last_ref = report::read_last_ref(&paths);
    let override_ref = cli
        .baseline
        .clone()
        .or_else(|| std::env::var("SPECGUARD_BASELINE_REF").ok());

    let scope = scope::resolve(
        &l.cfg,
        &l.repo_root,
        override_ref.as_deref(),
        last_ref.as_deref(),
    )?;
    let shards = prompt::shards(&l.cfg, &scope);

    match cli.command {
        Some(Command::Scope) => {
            print_scope(&l.cfg, &scope);
            return Ok(EXIT_OK);
        }
        Some(Command::Prompt { json }) => {
            // The ratification gate guards the gateway to dispatch: refuse to emit
            // machine-readable prompts (which the plugin hands to subagents) until
            // the prompt (meta-canon) is ratified, just as `run` refuses before it
            // spawns. The human debug view is not gated (it audits nothing).
            if json {
                if let Some(code) = ratification_block(&l)? {
                    return Ok(code);
                }
                emit_prompt_json(&l, &scope, &shards)?;
            } else {
                print_prompts(&l, &scope, &shards);
            }
            return Ok(EXIT_OK);
        }
        Some(Command::Ingest { ref from }) => {
            if let Some(code) = ratification_block(&l)? {
                return Ok(code);
            }
            let outs = read_ingest(from.as_deref(), &l, &scope, &shards)?;
            return finish(&l, &scope, &shards, &paths, outs);
        }
        Some(Command::Ack { .. })
        | Some(Command::Pending)
        | Some(Command::Brief { .. })
        | Some(Command::Init { .. })
        | Some(Command::Decide { .. })
        | Some(Command::AcceptPrompt { .. })
        | Some(Command::Map { .. })
        | Some(Command::Audit { .. })
        | Some(Command::TestAudit { .. }) => unreachable!("handled above"),
        Some(Command::Run) | None => {}
    }

    // Run (default): ratify, render every shard, dispatch to the agent, finish.
    if let Some(code) = ratification_block(&l)? {
        return Ok(code);
    }

    // Content-hash memoization: a shard whose input files (its canon + matched
    // files — see `scope::shard_input_files`) are byte-identical to the last
    // clean (no-findings) audit is skipped outright: no agent call, synthesized
    // as a clean result. First run (no stored fingerprints yet) has nothing to
    // compare against, so every shard audits exactly as it did before this
    // feature existed — backward-compatible by construction.
    let fp_path = fingerprint_state_path(&paths);
    let stored_fp = read_fingerprints(&fp_path);
    let cache = classify_cache(&l.cfg, &scope, &shards, &l.repo_root, &stored_fp);
    let cached_count = cache.iter().filter(|c| c.cached).count();
    if cached_count > 0 {
        println!("specguard: cached: {cached_count} shards skipped");
    }

    if !shards.is_empty() {
        eprintln!(
            "specguard: auditing {} (baseline {}, {} shard(s): {} area(s) + {} invariant + {} decision)",
            l.cfg.project.name,
            scope.baseline,
            shards.len(),
            scope.in_scope.len(),
            if l.cfg.invariants.is_empty() { 0 } else { 1 },
            if scope.decision_files.is_empty() { 0 } else { 1 },
        );
    }

    let shard_prompts: Vec<agent::ShardPrompt> = shards
        .iter()
        .zip(cache.iter())
        .filter(|(_, c)| !c.cached)
        .map(|(&sh, _)| agent::ShardPrompt {
            label: prompt::shard_label(&l.cfg, &scope, sh),
            prompt: render_shard_prompt(&l, &scope, sh),
        })
        .collect();

    let mut run_outs = if shard_prompts.is_empty() {
        Vec::new()
    } else {
        agent::run_shards(&l.cfg.agent, &l.repo_root, shard_prompts)
    }
    .into_iter();

    // Rebuild in the ORIGINAL shard order: `finish`/`verify::apply` treat
    // `shards` and `outs` as index-aligned (agent::run_shards preserves the
    // dispatch order of `shard_prompts`, which itself preserves the relative
    // order of the non-cached subset of `shards`).
    // Written as a loop rather than `.map().collect()` so running out of agent
    // outputs can bail instead of panicking. The alignment is an invariant, but
    // if it ever breaks, silently rebuilding a SHORT `outs` would attribute
    // findings to the wrong shard — and a shard that got someone else's (empty)
    // output reads downstream as "that shard was clean". Refusing is the only
    // answer that does not manufacture a false all-clear.
    let mut outs: Vec<agent::ShardOutput> = Vec::with_capacity(cache.len());
    for c in cache.iter() {
        if c.cached {
            outs.push(cached_shard_output(&c.label));
        } else {
            match run_outs.next() {
                Some(o) => outs.push(o),
                None => anyhow::bail!(
                    "shard/output misalignment: ran out of agent outputs while rebuilding \
                     shard order at label {:?} ({} shard(s) total). Refusing to continue — \
                     a short rebuild would report unaudited shards as clean.",
                    c.label,
                    cache.len()
                ),
            }
        }
    }

    // Progressive scope escalation (t4 Part B): re-dispatch (exactly once) any
    // area shard that signalled insufficient context, with a widened map. A
    // no-op unless the feature is enabled and something was flagged.
    let outs = escalate_outputs(&l, &scope, &shards, outs);

    finish(&l, &scope, &shards, &paths, outs)
}

/// True when the relevant-file map + escalation feature (t4) is opted in via
/// `SPECGUARD_RELEVANT_MAP` (`1`/`true`/`yes`/`on`). Default OFF: with it unset
/// the harness renders and dispatches exactly as it did before this feature
/// existed, so every existing test/behavior is unchanged (and `fugu-router` is
/// never invoked).
fn relevant_map_enabled() -> bool {
    matches!(
        std::env::var("SPECGUARD_RELEVANT_MAP")
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str(),
        "1" | "true" | "yes" | "on"
    )
}

/// Render one shard's prompt, prepending the bounded relevant-file map for AREA
/// shards when the feature is enabled (t4 Part A). Invariants/decisions shards,
/// and the feature-off path, render exactly as `prompt::render_shard`.
fn render_shard_prompt(l: &Loaded, scope: &scope::Scope, sh: prompt::Shard) -> String {
    if relevant_map_enabled() {
        if let prompt::Shard::Area(_) = sh {
            let query = scope::shard_query(&l.cfg, scope, sh);
            let map = scope::relevant_file_map(
                &l.cfg,
                scope,
                sh,
                &l.repo_root,
                &query,
                scope::CODE_INDEX_K,
            );
            return prompt::render_shard_with_map(
                &l.template,
                &l.cfg,
                scope,
                sh,
                &l.date,
                &map,
                false,
            );
        }
    }
    prompt::render_shard(&l.template, &l.cfg, scope, sh, &l.date)
}

/// Area shards (indices into `shards`/`outs`) whose audit output signals
/// insufficient context (t4 Part B). Only AREA shards are widenable — the
/// invariants/decisions shards have no adjacent scope to expand — and only a
/// clean-exit (code 0) shard is eligible (a crashed agent is handled by
/// `finish`'s failure path instead).
fn insufficient_context_shards(
    shards: &[prompt::Shard],
    outs: &[agent::ShardOutput],
) -> Vec<usize> {
    shards
        .iter()
        .zip(outs.iter())
        .enumerate()
        .filter(|(_, (sh, o))| {
            matches!(sh, prompt::Shard::Area(_))
                && o.out.code == 0
                && prompt::signals_insufficient_context(&o.out.stdout)
        })
        .map(|(i, _)| i)
        .collect()
}

/// Pure escalation core (dispatch injected → unit-testable): re-dispatch each
/// `flagged` shard EXACTLY once with a widened prompt and replace its output in
/// place; untouched shards keep their first-pass output. `widen` builds the
/// re-dispatch prompt for a shard index; `run` dispatches the batch once (real
/// agents in prod, a fake in tests). `run` is `FnOnce`, so a second escalation
/// pass is structurally impossible.
fn escalate_core(
    mut outs: Vec<agent::ShardOutput>,
    flagged: &[usize],
    widen: impl Fn(usize) -> agent::ShardPrompt,
    run: impl FnOnce(Vec<agent::ShardPrompt>) -> Vec<agent::ShardOutput>,
) -> Vec<agent::ShardOutput> {
    if flagged.is_empty() {
        return outs;
    }
    let prompts: Vec<agent::ShardPrompt> = flagged.iter().map(|&i| widen(i)).collect();
    let widened = run(prompts);
    for (o, &i) in widened.into_iter().zip(flagged.iter()) {
        outs[i] = o;
    }
    outs
}

/// Wire progressive escalation (t4 Part B) into a run: for any AREA shard whose
/// first-pass output signalled insufficient context, re-render it with a WIDENED
/// relevant-file map (a larger code-index `k`) and re-dispatch that one shard
/// exactly once. Gated by the feature flag; a no-op when disabled or nothing was
/// flagged, so today's behavior is unchanged.
fn escalate_outputs(
    l: &Loaded,
    scope: &scope::Scope,
    shards: &[prompt::Shard],
    outs: Vec<agent::ShardOutput>,
) -> Vec<agent::ShardOutput> {
    if !relevant_map_enabled() {
        return outs;
    }
    let flagged = insufficient_context_shards(shards, &outs);
    if flagged.is_empty() {
        return outs;
    }
    eprintln!(
        "specguard: {} shard(s) signalled insufficient context; re-auditing once with a widened relevant-file map",
        flagged.len()
    );
    let widen = |i: usize| -> agent::ShardPrompt {
        let sh = shards[i];
        let query = scope::shard_query(&l.cfg, scope, sh);
        let map = scope::relevant_file_map(
            &l.cfg,
            scope,
            sh,
            &l.repo_root,
            &query,
            scope::CODE_INDEX_K_WIDENED,
        );
        agent::ShardPrompt {
            label: prompt::shard_label(&l.cfg, scope, sh),
            prompt: prompt::render_shard_with_map(
                &l.template,
                &l.cfg,
                scope,
                sh,
                &l.date,
                &map,
                true,
            ),
        }
    };
    escalate_core(outs, &flagged, widen, |prompts| {
        agent::run_shards(&l.cfg.agent, &l.repo_root, prompts)
    })
}

/// Per-shard cache status for this run: whether its current content
/// fingerprint (see [`scope::fingerprint_files`] over [`scope::shard_input_files`])
/// matches what was stored from the last clean audit. Pure aside from reading
/// file bytes off disk, so it's directly unit-testable without spawning any
/// agent or CLI process (see the `tests` module: both the skip path, unchanged
/// input, and the re-audit path, changed input).
struct ShardCache {
    label: String,
    cached: bool,
}

fn classify_cache(
    cfg: &Config,
    scope: &scope::Scope,
    shards: &[prompt::Shard],
    repo_root: &Path,
    stored: &std::collections::HashMap<String, String>,
) -> Vec<ShardCache> {
    shards
        .iter()
        .map(|&sh| {
            let label = prompt::shard_label(cfg, scope, sh);
            let files = scope::shard_input_files(cfg, scope, sh);
            let fp = scope::fingerprint_files(repo_root, &files);
            let cached = stored.get(&label) == Some(&fp);
            ShardCache { label, cached }
        })
        .collect()
}

/// Synthesize a clean agent output for a cached (skipped) shard: a valid
/// marker with `needs_user: no`, so it flows through `finish`'s
/// parse/merge/sentinel logic exactly like a freshly-audited clean shard.
fn cached_shard_output(label: &str) -> agent::ShardOutput {
    agent::ShardOutput {
        label: label.to_string(),
        out: agent::AgentOutput {
            stdout: format!(
                "(cached — 入力 (canon + 変更ファイル) が前回の green 監査から変化していないためスキップ)\n\n{}\nneeds_user: no\nsummary: (cached; unchanged since last audit)\n",
                parse::MARKER
            ),
            stderr: String::new(),
            code: 0,
        },
    }
}

/// Path to the persisted per-shard content fingerprints — a sibling of
/// `.last-ref` in the same report-state directory (no change to
/// `report::Paths`/`report.rs`'s public surface needed).
fn fingerprint_state_path(paths: &report::Paths) -> PathBuf {
    paths.last_ref.with_file_name(".shard-fingerprints")
}

/// Read the persisted fingerprints (`label\tfingerprint` per line). Absent or
/// unparseable file -> empty map, meaning nothing is cached (first run audits
/// every shard exactly as it did before this feature existed).
fn read_fingerprints(path: &Path) -> std::collections::HashMap<String, String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return std::collections::HashMap::new();
    };
    text.lines()
        .filter_map(|line| line.split_once('\t'))
        .map(|(label, fp)| (label.to_string(), fp.to_string()))
        .collect()
}

/// Write the fingerprints back (sorted by label for a stable diff).
fn write_fingerprints(path: &Path, map: &std::collections::HashMap<String, String>) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let mut entries: Vec<(&String, &String)> = map.iter().collect();
    entries.sort_by(|a, b| a.0.cmp(b.0));
    let body: String = entries
        .iter()
        .map(|(label, fp)| format!("{label}\t{fp}\n"))
        .collect();
    std::fs::write(path, body).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// Persist a fresh content fingerprint for each audited shard that came back
/// clean (`needs_user == false`) this run — the memoization idiom: a green
/// shard's fingerprint becomes the "last known good" input, so an unchanged
/// re-run of `run` skips it outright next time (see [`classify_cache`] /
/// [`cached_shard_output`]). A shard that still has findings has its stale
/// entry dropped instead, so it keeps getting re-audited (never silently
/// skipped) until it goes clean — mirroring the existing baseline-hold
/// semantics for findings. `shards`/`parsed` are index-aligned: `verify::apply`
/// only ever REPLACES entries in place (refute) or APPENDS extra completeness
/// entries after them (never reorders/removes), so `zip` naturally covers
/// exactly the original shards and ignores any appended completeness entries.
fn persist_fingerprints(
    l: &Loaded,
    scope: &scope::Scope,
    shards: &[prompt::Shard],
    paths: &report::Paths,
    parsed: &[(String, parse::Parsed)],
) {
    let fp_path = fingerprint_state_path(paths);
    let mut stored = read_fingerprints(&fp_path);
    let mut changed = false;
    for (&sh, (label, p)) in shards.iter().zip(parsed.iter()) {
        if p.needs_user {
            if stored.remove(label).is_some() {
                changed = true;
            }
            continue;
        }
        let files = scope::shard_input_files(&l.cfg, scope, sh);
        let fp = scope::fingerprint_files(&l.repo_root, &files);
        if stored.get(label) != Some(&fp) {
            stored.insert(label.clone(), fp);
            changed = true;
        }
    }
    if changed {
        if let Err(e) = write_fingerprints(&fp_path, &stored) {
            eprintln!(
                "specguard: WARN could not persist shard fingerprints ({e:#}); memoization skipped next run"
            );
        }
    }
}

/// The ratification gate as a reusable guard. Returns `Ok(Some(EXIT_UNRATIFIED))`
/// when `[prompt].require_ratification` is on and the prompt (meta-canon) is
/// unratified or has drifted since ratification — the caller should return that
/// code rather than audit. `Ok(None)` means auditing may proceed.
fn ratification_block(l: &Loaded) -> Result<Option<u8>> {
    if !l.cfg.prompt.require_ratification {
        return Ok(None);
    }
    let hashes = current_hashes(l);
    match ratify::read_lock(&l.repo_root) {
        None => {
            eprintln!(
                "specguard: prompt (メタ正典) が未批准です。内容を確認し、\n  `specguard accept-prompt -m \"理由\"` で批准してください。"
            );
            Ok(Some(EXIT_UNRATIFIED))
        }
        Some(lock) => {
            let drift = ratify::drifted(
                &lock,
                &hashes,
                l.cfg.verify.enabled,
                l.cfg.verify.completeness,
            );
            if !drift.is_empty() {
                // Graded (staged) triage: a drifted template that is precedented
                // (deterministically close to its ratified precedent) is auto-
                // ratified; only novel/large deviations still require a human. When
                // graded is off this whole block is skipped and the binary gate
                // stands (drift -> human).
                if l.cfg.prompt.graded {
                    let texts = current_texts(l);
                    match ratify::triage_drift(
                        &drift,
                        &lock.corpus,
                        &texts,
                        l.cfg.prompt.graded_threshold,
                    ) {
                        ratify::Triage::Precedented => {
                            // Similarity/polarity-based triage alone does not
                            // guarantee contract compliance: `cfg.prompt.template`
                            // is externally replaceable, so a change judged
                            // "precedented" could still be missing required
                            // placeholders. Run the same contract check
                            // `accept_prompt` runs before EVER auto-ratifying;
                            // a violation must not be waved through just because
                            // it is textually similar to its precedent. Fall back
                            // to the human path, same as `Triage::Novel`.
                            let violations = contract_violations(l);
                            if let Some(msg) = format_contract_violations(&violations) {
                                eprintln!(
                                    "specguard: prompt (メタ正典) の変更は precedented だが契約に矛盾 (必須 placeholder 不足) のため自動批准を拒否:\n{msg}\n  内容を確認し、合意できるなら `specguard accept-prompt -m \"理由\"` で批准してください。"
                                );
                                return Ok(Some(EXIT_UNRATIFIED));
                            }
                            // Re-pin the lock to the precedented change: an
                            // auto-ratify records the new texts as the fresh
                            // precedent, with a machine-authored reason. Only the
                            // live policy surface is pinned (mirrors accept_prompt).
                            let head = scope::current_head(&l.repo_root)
                                .unwrap_or_else(|_| "UNKNOWN".to_string());
                            let mut re_h = hashes;
                            let mut re_t = texts;
                            ratify::mask_inactive(
                                &mut re_h,
                                l.cfg.verify.enabled,
                                l.cfg.verify.completeness,
                            );
                            ratify::mask_inactive(
                                &mut re_t,
                                l.cfg.verify.enabled,
                                l.cfg.verify.completeness,
                            );
                            let reason = format!(
                                "auto-ratified (graded): precedented change to {} (similarity >= {})",
                                drift.join(", "),
                                l.cfg.prompt.graded_threshold
                            );
                            ratify::write_lock(
                                &l.repo_root,
                                &re_h,
                                &re_t,
                                &head,
                                &l.date,
                                &reason,
                            )?;
                            eprintln!(
                                "specguard: prompt (メタ正典) の変更を自動批准 (graded, precedented): {}",
                                drift.join(", ")
                            );
                            return Ok(None);
                        }
                        ratify::Triage::Novel(novel) => {
                            eprintln!(
                                "specguard: prompt (メタ正典) に novel な未批准変更があります: {}\n  (graded: 類似度 < {} のため人間の批准が必要)\n  内容を確認し、合意できるなら `specguard accept-prompt -m \"理由\"` で再批准してください。",
                                novel.join(", "),
                                l.cfg.prompt.graded_threshold
                            );
                            return Ok(Some(EXIT_UNRATIFIED));
                        }
                    }
                }
                eprintln!(
                    "specguard: prompt (メタ正典) に未批准の変更があります: {}\n  内容を確認し、合意できるなら `specguard accept-prompt -m \"理由\"` で再批准してください。",
                    drift.join(", ")
                );
                return Ok(Some(EXIT_UNRATIFIED));
            }
            // Surface which ratified policy version is in force.
            eprintln!(
                "specguard: prompt 批准済み (date {}, canon {}) 理由: {}",
                lock.date, lock.canon_commit, lock.reason
            );
            Ok(None)
        }
    }
}

/// Shared tail of `run` and `ingest`: given the per-shard agent outputs (already
/// collected — by spawning for `run`, from JSON for `ingest`), parse them, run the
/// optional verification gates, merge the report, and update sentinel/baseline.
/// When there are no shards, `outs` is ignored and the empty-scope progress path
/// runs instead. Returns the same exit codes `run` always has.
fn finish(
    l: &Loaded,
    scope: &scope::Scope,
    shards: &[prompt::Shard],
    paths: &report::Paths,
    outs: Vec<agent::ShardOutput>,
) -> Result<u8> {
    // CA-specguard-005: if current_head() errors here (e.g. git unavailable)
    // while a sentinel is about to be RAISED, the fallback must be a value
    // that `report::has_new_commits` can never satisfy without `--force` —
    // NOT an arbitrary placeholder that would equal-check away as soon as
    // git recovers and `ack` runs against a real HEAD. See
    // `report::POISONED_RAISED_AT` for the fail-closed contract.
    let head = scope::current_head(&l.repo_root)
        .unwrap_or_else(|_| report::POISONED_RAISED_AT.to_string());

    // Nothing in scope and no invariants: record progress without an agent call.
    if shards.is_empty() {
        let body = format!(
            "# {} 仕様↔実装 整合監査 {}\n\n## スコープ\n- baseline: {}\n- canon commit (HEAD): {}\n- in-scope 領域: なし / 不変条件: なし / 決定ログ: なし\n\n## findings\n監査対象なし。\n",
            l.cfg.project.name, l.date, scope.baseline, head
        );
        report::write_report(paths, &body)?;
        if report::sentinel_pending(paths) {
            println!(
                "specguard: 監査対象なし。ただし未処理 sentinel ({}) があるため baseline を据え置く (`specguard ack` で解除)",
                paths.sentinel.display()
            );
        } else {
            report::advance_baseline(paths, &head)?;
            println!(
                "specguard: 監査対象なし (report: {})",
                paths.report.display()
            );
        }
        return Ok(EXIT_OK);
    }

    // B: any shard whose agent failed -> a single EXIT_AGENT_FAILED, with each
    // real exit code on stderr. Never propagate raw (would collide with 2/3).
    let failed: Vec<&agent::ShardOutput> = outs.iter().filter(|o| o.out.code != 0).collect();
    if !failed.is_empty() {
        for f in &failed {
            eprintln!(
                "specguard: shard '{}' agent exited with code {}\n--- agent stderr ---\n{}",
                f.label,
                f.out.code,
                f.out.stderr.trim_end()
            );
        }
        return Ok(EXIT_AGENT_FAILED);
    }

    let parsed: Vec<(String, parse::Parsed)> = outs
        .iter()
        .map(|o| (o.label.clone(), parse::parse(&o.out.stdout)))
        .collect();

    // If any shard omitted the marker, the audit is incomplete: save the merged
    // report for inspection but do NOT advance the baseline, raise a sentinel, or
    // run verification (which needs complete audit output to refine).
    let missing: Vec<&str> = parsed
        .iter()
        .filter(|(_, p)| !p.marker_found)
        .map(|(l, _)| l.as_str())
        .collect();
    if !missing.is_empty() {
        let merged = merge_report(&l.cfg, scope, &l.date, &head, &parsed);
        if let Some(dir) = paths.report.parent() {
            std::fs::create_dir_all(dir).ok();
        }
        std::fs::write(&paths.report, &merged)
            .with_context(|| format!("writing report {}", paths.report.display()))?;
        eprintln!(
            "specguard: WARN marker '{}' missing in shard(s): {}; saved {} but cannot assess findings",
            parse::MARKER,
            missing.join(", "),
            paths.report.display()
        );
        return Ok(EXIT_NO_MARKER);
    }

    // Verification gates (opt-in): refute false positives (V1) and surface missed
    // rules (V2). A pure transform over the parsed shards so the merge/sentinel
    // logic below is unchanged. See DESIGN-VERIFY.md.
    let parsed = if l.cfg.verify.enabled || l.cfg.verify.completeness {
        verify::apply(&l.cfg, &l.repo_root, scope, shards, &l.date, parsed)
    } else {
        parsed
    };

    // Content-hash memoization: persist a fresh fingerprint for every shard
    // that came back clean this run, so an unchanged re-run of `run` skips it
    // (see `classify_cache` / `cached_shard_output`); a shard that still has
    // findings has its stale entry dropped instead of re-persisted.
    persist_fingerprints(l, scope, shards, paths, &parsed);

    let merged = merge_report(&l.cfg, scope, &l.date, &head, &parsed);
    report::write_report(paths, &merged)?;

    let report_rel = format!("{}/{}.md", l.cfg.output.report_dir, l.date);
    let needs_user = parsed.iter().any(|(_, p)| p.needs_user);
    if needs_user {
        // Findings: raise the sentinel and HOLD the baseline. Not advancing keeps
        // the unfixed drift in the next run's diff so it is re-detected; a human
        // releases it with `specguard ack` once handled.
        let summary = merge_summary(&parsed);
        report::write_sentinel(paths, &l.date, &report_rel, &summary, &head)?;
        println!(
            "specguard: 修正候補あり -> {} (report: {}); baseline は据え置き (ack するまで再検出)",
            paths.sentinel.display(),
            paths.report.display()
        );
    } else if report::sentinel_pending(paths) {
        // Clean this run, but a prior run's sentinel is still unhandled: keep the
        // baseline put so its drift stays in scope. Leave the sentinel untouched.
        println!(
            "specguard: 修正候補なし。ただし未処理 sentinel ({}) が残るため baseline を据え置く (`specguard ack` で解除)",
            paths.sentinel.display()
        );
    } else {
        // Fully clean: advance the baseline.
        report::advance_baseline(paths, &head)?;
        println!(
            "specguard: 修正候補なし (report: {})",
            paths.report.display()
        );
    }
    Ok(EXIT_OK)
}

/// Machine-readable envelope for `prompt --json`: enough for the plugin to
/// dispatch each shard to a read-only subagent and label the results.
#[derive(serde::Serialize)]
struct PromptJson<'a> {
    project: &'a str,
    baseline: &'a str,
    head: String,
    date: &'a str,
    /// The marker each shard's report must end with (so the orchestrator can
    /// remind the subagent, and reject output that lacks it).
    marker: &'a str,
    shards: Vec<ShardJson>,
}

#[derive(serde::Serialize)]
struct ShardJson {
    label: String,
    prompt: String,
}

/// Input envelope for `ingest`: the per-shard outputs the plugin collected from
/// its subagents. `stdout` is the subagent's report (must carry the marker);
/// `code` is its exit status (default 0 = success); `stderr` is optional context.
#[derive(serde::Deserialize)]
struct IngestJson {
    shards: Vec<IngestShard>,
}

#[derive(serde::Deserialize)]
struct IngestShard {
    label: String,
    #[serde(default)]
    stdout: String,
    #[serde(default)]
    stderr: String,
    #[serde(default)]
    code: i32,
}

/// Emit the JSON envelope of rendered shard prompts (for `prompt --json`).
fn emit_prompt_json(l: &Loaded, scope: &scope::Scope, shards: &[prompt::Shard]) -> Result<()> {
    let head = scope::current_head(&l.repo_root).unwrap_or_else(|_| "UNKNOWN".to_string());
    let shards_json: Vec<ShardJson> = shards
        .iter()
        .map(|&sh| ShardJson {
            label: prompt::shard_label(&l.cfg, scope, sh),
            prompt: render_shard_prompt(l, scope, sh),
        })
        .collect();
    let env = PromptJson {
        project: &l.cfg.project.name,
        baseline: &scope.baseline,
        head,
        date: &l.date,
        marker: parse::MARKER,
        shards: shards_json,
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&env).context("serializing prompt JSON")?
    );
    Ok(())
}

/// Read the `ingest` input (stdin or `--from`) and align its outputs to the
/// freshly resolved shards by label. A shard with no matching output becomes an
/// agent failure (`code = -1`) so `finish` flags it like any other failed shard —
/// the plugin must return every shard it was handed.
fn read_ingest(
    from: Option<&Path>,
    l: &Loaded,
    scope: &scope::Scope,
    shards: &[prompt::Shard],
) -> Result<Vec<agent::ShardOutput>> {
    let raw = match from {
        Some(p) => std::fs::read_to_string(p)
            .with_context(|| format!("reading ingest file {}", p.display()))?,
        None => {
            use std::io::Read;
            let mut s = String::new();
            std::io::stdin()
                .read_to_string(&mut s)
                .context("reading ingest JSON from stdin")?;
            s
        }
    };
    let parsed: IngestJson = serde_json::from_str(&raw).context("parsing ingest JSON")?;
    let mut by_label: std::collections::HashMap<String, IngestShard> = parsed
        .shards
        .into_iter()
        .map(|s| (s.label.clone(), s))
        .collect();
    let outs = shards
        .iter()
        .map(|&sh| {
            let label = prompt::shard_label(&l.cfg, scope, sh);
            match by_label.remove(&label) {
                Some(s) => agent::ShardOutput {
                    label,
                    out: agent::AgentOutput {
                        stdout: s.stdout,
                        stderr: s.stderr,
                        code: s.code,
                    },
                },
                None => agent::ShardOutput {
                    out: agent::AgentOutput {
                        stdout: String::new(),
                        stderr: format!("no output provided for shard '{label}' in ingest input"),
                        code: -1,
                    },
                    label,
                },
            }
        })
        .collect();
    Ok(outs)
}

/// Pre-task spec briefing (read-only). Renders the briefing prompt over every
/// configured area + the invariants and either prints it (`--prompt`, for the
/// plugin to dispatch to a subagent) or runs the configured agent once and prints
/// its brief. Produces no report/sentinel — it is advisory, drift-prevention.
fn brief(l: &Loaded, task: &str, prompt_only: bool) -> Result<u8> {
    if task.trim().is_empty() {
        anyhow::bail!("brief には着手するタスクの説明が必要です (例: specguard brief \"...\")");
    }
    let rendered = prompt::render_brief(prompt::BRIEF_TEMPLATE, &l.cfg, task, &l.date);
    if prompt_only {
        print!("{rendered}");
        return Ok(EXIT_OK);
    }
    let shard = agent::ShardPrompt {
        label: "brief".to_string(),
        prompt: rendered,
    };
    let outs = agent::run_shards(&l.cfg.agent, &l.repo_root, vec![shard]);
    let o = &outs[0];
    if o.out.code != 0 {
        eprintln!(
            "specguard: brief agent exited with code {}\n--- agent stderr ---\n{}",
            o.out.code,
            o.out.stderr.trim_end()
        );
        return Ok(EXIT_AGENT_FAILED);
    }
    print!("{}", o.out.stdout);
    Ok(EXIT_OK)
}

/// SessionStart hook entry point: if a sentinel is pending, print an active
/// fix-offer block so the host agent surfaces it (read the report, then ask the
/// human whether to fix). Resolves the sentinel path from config, so a custom
/// `[output].sentinel` still works (the old hook hardcoded `.specguard-pending`).
/// Best-effort: any failure (no config, unreadable sentinel) prints nothing and
/// exits 0, so it can never block a session from starting.
fn pending(cli: &Cli) -> u8 {
    let Ok(l) = load(cli) else {
        return EXIT_OK;
    };
    let paths = report::paths(&l.cfg, &l.repo_root, &l.date);
    if !report::sentinel_pending(&paths) {
        return EXIT_OK;
    }
    let body = std::fs::read_to_string(&paths.sentinel).unwrap_or_default();
    let field = |key: &str| -> String {
        body.lines()
            .find_map(|line| line.strip_prefix(key).map(|v| v.trim().to_string()))
            .unwrap_or_default()
    };
    let report = field("report:");
    let summary = field("summary:");

    println!("⚠ specguard: 未処理の仕様ドリフト指摘があります (Human-on-the-loop)。");
    if !report.is_empty() {
        println!("report: {report}");
    }
    if !summary.is_empty() {
        println!("summary: {summary}");
    }
    println!();
    println!("対応方針: まず report (上記パス) を Read して指摘内容を把握し、`AskUserQuestion` で");
    println!("次の3択を人間に提示せよ (人間が選ぶまで勝手に修正・ack しないこと):");
    println!("  1. 別タスクで修正に着手 — 各 finding を B(コード修正)/C(doc 更新) に分類し、修正後 `specguard ack`");
    println!("  2. 後で — sentinel を残す (次セッションで再提示)");
    println!("  3. 不要 — `specguard ack` で sentinel を解除");
    EXIT_OK
}

/// Clear the sentinel (C). Idempotent: succeeds whether or not one was present.
fn run_testaudit(repo_root: &std::path::Path, json: bool) -> Result<u8> {
    let findings = match testaudit::scan_repo(repo_root) {
        Ok(f) => f,
        // The scan could not read part of the tree (a dir/file that EXISTS but
        // is unreadable, or an incomplete listing). That is cannot-determine,
        // NOT "no skipped tests" — fail closed with a dedicated exit code rather
        // than pass GREEN on a subtree we never read. The error goes to stderr;
        // `--json` still emits nothing on stdout (no findings array to trust).
        Err(e) => {
            eprintln!("specguard testaudit: cannot determine — scan incomplete: {e:#}");
            eprintln!(
                "  refusing to report GREEN on a tree that could not be fully read (fail closed)"
            );
            return Ok(EXIT_TESTAUDIT_UNDETERMINED);
        }
    };
    if findings.is_empty() {
        if json {
            println!("{{\"findings\":[]}}");
        } else {
            println!("specguard testaudit: no issues found");
        }
        return Ok(EXIT_OK);
    }
    if json {
        #[derive(serde::Serialize)]
        struct Out<'a> {
            findings: Vec<FindingOut<'a>>,
        }
        #[derive(serde::Serialize)]
        struct FindingOut<'a> {
            kind: &'a str,
            file: &'a str,
            name: &'a str,
            reason: &'a str,
        }
        let out = Out {
            findings: findings
                .iter()
                .map(|f| FindingOut {
                    kind: f.kind.as_str(),
                    file: &f.file,
                    name: &f.name,
                    reason: &f.reason,
                })
                .collect(),
        };
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else {
        println!("specguard testaudit: {} finding(s)\n", findings.len());
        for f in &findings {
            println!(
                "  [{}] {}:{} — {}",
                f.kind.as_str(),
                f.file,
                f.name,
                f.reason
            );
        }
    }
    Ok(EXIT_TESTAUDIT_FINDINGS)
}

fn ack(paths: &report::Paths, repo_root: &std::path::Path, force: bool) -> Result<u8> {
    // If the sentinel exists, check whether a fix commit was made since it was raised.
    if !force && paths.sentinel.exists() {
        let content = std::fs::read_to_string(&paths.sentinel).unwrap_or_default();
        if let Some(raised_at) = report::sentinel_raised_at(&content) {
            // CA-specguard-003: current_head() errors (e.g. git unavailable) must
            // not be swallowed into "" — has_new_commits("<hash>", "") reads as a
            // genuine HEAD change and would let ack clear the sentinel without
            // ever having verified a fix commit. Fail closed instead.
            let current = match scope::current_head(repo_root) {
                Ok(head) => head,
                Err(e) => {
                    eprintln!("specguard: HEAD を解決できないため安全側で ack を拒否 ({e})");
                    eprintln!("  修正をコミットしてから `specguard ack` を実行するか、意図的に解除するなら `specguard ack --force`");
                    return Ok(EXIT_NO_FIX_COMMIT);
                }
            };
            if !report::has_new_commits(&raised_at, &current) {
                eprintln!(
                    "specguard: 修正コミットが見当たらない (raised_at: {raised_at}, HEAD: {current})"
                );
                eprintln!("  修正をコミットしてから `specguard ack` を実行するか、意図的に解除するなら `specguard ack --force`");
                return Ok(EXIT_NO_FIX_COMMIT);
            }
        }
    }
    match std::fs::remove_file(&paths.sentinel) {
        Ok(()) => println!(
            "specguard: sentinel をクリアした ({})",
            paths.sentinel.display()
        ),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            println!("specguard: sentinel は無い ({})", paths.sentinel.display());
        }
        Err(e) => {
            return Err(anyhow::Error::new(e))
                .with_context(|| format!("removing sentinel {}", paths.sentinel.display()));
        }
    }
    Ok(EXIT_OK)
}

/// Scaffold a decision record (ADR) pinned to the current canon commit.
fn decide(l: &Loaded, title: &str, force: bool) -> Result<u8> {
    let head = scope::current_head(&l.repo_root).unwrap_or_else(|_| "UNKNOWN".to_string());
    let id = format!("{}-{}", l.date, decision::slug(title));
    let path = decision::scaffold(
        &l.repo_root,
        &l.cfg.decisions.dir,
        &id,
        title,
        &l.date,
        &head,
        force,
    )?;
    println!("specguard: 決定ログを生成 -> {}", path.display());
    println!("  canon commit に pin 済み (canon_commit: {head})");
    println!("  次: `canon:` に支配する canon ポインタ、`drivers:` に反証可能な理由、`review_when:` を記入");
    Ok(EXIT_OK)
}

/// Maintain the independent file→spec mapping store. `build` seeds the map from
/// the full baseline window (create-if-absent + sync), `sync` reconciles it
/// incrementally, and `list` prints it. `sync`'s incremental window is anchored
/// on the map's own persisted `last_synced` ref — NOT the unrelated `specguard
/// run` audit `.last-ref` (a separate baseline tracker that may not exist at
/// all for map-only usage, which previously made `sync` silently fall back to
/// `fallback_ref` and rescan a much wider window than "since last sync").
fn run_map(cli: &Cli, l: &Loaded, action: &MapAction) -> Result<u8> {
    let map_path = l.repo_root.join(&l.cfg.map.path);
    let spec_dir = &l.cfg.map.spec_doc_dir;

    match action {
        MapAction::List { json, filter } => {
            let map = specmap::SpecMap::load(&map_path)?;
            // Scope the listing through the shared entry-match predicate (the
            // same one `audit --filter` uses). An absent/blank filter matches
            // every entry, so the unfiltered listing is unchanged.
            let filter = filter.as_deref().unwrap_or("");
            let map = filter_map(&map, filter);
            if *json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&map).context("serializing spec map JSON")?
                );
            } else {
                print_map(&map, &map_path);
            }
            Ok(EXIT_OK)
        }
        MapAction::Build | MapAction::Sync => {
            let override_ref = cli
                .baseline
                .clone()
                .or_else(|| std::env::var("SPECGUARD_BASELINE_REF").ok());
            let exclude = specmap::compile_globs(&l.cfg.map.exclude)?;
            let mut map = match action {
                MapAction::Build => specmap::SpecMap::load_or_init(&map_path)?,
                _ => specmap::SpecMap::load(&map_path)?,
            };
            // `build` seeds from the full window (ignore any recorded ref so the
            // whole `baseline_ref`/`fallback_ref` history reflects into a fresh
            // map); `sync` anchors on the map's own `last_synced` (the ref this
            // exact map was last reconciled against), not the audit's `.last-ref`.
            let last_ref = match action {
                MapAction::Build => None,
                _ => Some(map.last_synced.as_str()).filter(|r| !r.trim().is_empty()),
            };
            let baseline = scope::resolve_baseline(&l.cfg, override_ref.as_deref(), last_ref);
            let head = scope::current_head(&l.repo_root).unwrap_or_else(|_| "HEAD".to_string());

            map.sync(&l.repo_root, &baseline, spec_dir, &head, &exclude)?;
            let pruned = map.prune_excluded(&exclude);
            map.save(&map_path)?;
            println!(
                "specguard map: {} -> {} ({} entr{}, baseline {}{})",
                match action {
                    MapAction::Build => "built",
                    _ => "synced",
                },
                map_path.display(),
                map.len(),
                if map.len() == 1 { "y" } else { "ies" },
                baseline,
                if pruned.is_empty() {
                    String::new()
                } else {
                    format!(", {} excluded pruned", pruned.len())
                },
            );
            Ok(EXIT_OK)
        }
        MapAction::SetSpec { selector, doc } => {
            let mut map = specmap::SpecMap::load(&map_path)?;
            let touched = map.set_spec(selector, doc)?;
            map.save(&map_path)?;
            if touched.is_empty() {
                println!("specguard map: no entry matched '{selector}' (nothing set)");
            } else {
                println!(
                    "specguard map: set spec_doc={doc} + tracked on {} entr{} matching '{selector}'",
                    touched.len(),
                    if touched.len() == 1 { "y" } else { "ies" },
                );
            }
            Ok(EXIT_OK)
        }
        MapAction::Resolve { selector } => {
            let mut map = specmap::SpecMap::load(&map_path)?;
            let touched = map.resolve(selector)?;
            map.save(&map_path)?;
            if touched.is_empty() {
                println!("specguard map: no entry matched '{selector}' (nothing resolved)");
            } else {
                println!(
                    "specguard map: marked {} entr{} tracked matching '{selector}'",
                    touched.len(),
                    if touched.len() == 1 { "y" } else { "ies" },
                );
            }
            Ok(EXIT_OK)
        }
        MapAction::Prune => {
            let exclude = specmap::compile_globs(&l.cfg.map.exclude)?;
            let mut map = specmap::SpecMap::load(&map_path)?;
            let pruned = map.prune_excluded(&exclude);
            map.save(&map_path)?;
            println!(
                "specguard map: pruned {} excluded entr{} ({} remaining)",
                pruned.len(),
                if pruned.len() == 1 { "y" } else { "ies" },
                map.len(),
            );
            Ok(EXIT_OK)
        }
    }
}

/// Map-driven CORRECTNESS audit (read-only). Loads the persisted spec-map store
/// (built by `map build`/`sync`), computes deterministic structural findings
/// (undocumented / dangling reference / untested) and per-entry LLM audit
/// shards, and either emits the machine-readable envelope (`--json`, same shape
/// as `prompt --json`, ingest-compatible) or a human summary. It fixes nothing
/// (Human-on-the-loop) and spawns no agent here — dispatch is the plugin's job.
/// The baseline is resolved the same way the audit does (override > baseline_ref
/// > recorded last-ref > fallback), purely for provenance in the envelope.
fn run_audit(cli: &Cli, l: &Loaded, paths: &report::Paths, json: bool, filter: &str) -> Result<u8> {
    let map_path = l.repo_root.join(&l.cfg.map.path);
    let map = specmap::SpecMap::load(&map_path)?;

    let override_ref = cli
        .baseline
        .clone()
        .or_else(|| std::env::var("SPECGUARD_BASELINE_REF").ok());
    let last_ref = report::read_last_ref(paths);
    let baseline = scope::resolve_baseline(&l.cfg, override_ref.as_deref(), last_ref.as_deref());
    let head = scope::current_head(&l.repo_root).unwrap_or_else(|_| "UNKNOWN".to_string());

    let envelope = auditmap::build_envelope(
        &l.cfg,
        &map,
        &l.repo_root,
        &baseline,
        &head,
        &l.date,
        filter,
    );

    // The deterministic, filter-scoped structural findings. Computed once here so
    // both the `--json` and human paths emit the same fleet violations before
    // returning.
    let findings = auditmap::scan_map_filtered(&map, &l.repo_root, filter);

    // Fleet-level correlated-error signal: record each structural finding as an
    // overwatch violation so recurring spec/impl drift across tasks/sessions can
    // be escalated as systemic. STRICTLY FAIL-SOFT — this must never change the
    // audit's exit code, its report/envelope contents, or the finding set, and
    // must never panic if the store is unwritable. A clean (no-findings) run
    // emits nothing.
    emit_audit_violations(&l.repo_root, &findings);

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&envelope).context("serializing audit envelope JSON")?
        );
        return Ok(EXIT_OK);
    }

    // Human summary: the structural findings (deterministic, filter-scoped) + how
    // many audit shards would be dispatched. Read-only; nothing is written.
    let scope_note = if filter.trim().is_empty() {
        String::new()
    } else {
        format!(" (filter: {})", filter.trim())
    };
    println!(
        "specguard audit: {} entr{} mapped, {} audit shard(s), {} structural finding(s){} [{}]",
        map.len(),
        if map.len() == 1 { "y" } else { "ies" },
        envelope.shards.len(),
        findings.len(),
        scope_note,
        map_path.display()
    );
    if map.is_empty() {
        println!(
            "  (map is empty — run `specguard map build` first to populate spec↔impl↔test entries)"
        );
    }
    for f in &findings {
        println!("  [{}] {} — {}", f.kind.as_str(), f.key, f.detail);
    }
    if findings.is_empty() && !map.is_empty() {
        println!("  (no structural gaps; run the shards for the correctness audit)");
    }
    Ok(EXIT_OK)
}

/// Emit one overwatch fleet-violation per structural finding, FAIL-SOFT.
///
/// For each [`auditmap::StructuralFinding`] we build a
/// [`overwatch::violation::ViolationEvent`] with `source = Specguard`,
/// `drift_kind = finding.kind` (the stable token from
/// [`auditmap::StructuralKind::as_str`]) and `symbol = finding.key` (the spec/impl
/// map-entry key). The signature that overwatch normalizes therefore reads
/// `specguard:<kind>:<symbol>` — stable across tasks/sessions so recurrence can be
/// aggregated. `task_key` is a stable per-entry key (`<kind>:<symbol>`),
/// `session_id` comes from `CLAUDE_CODE_SESSION_ID` (empty when unset), and the
/// timestamp is [`overwatch::store::now`].
///
/// This is a pure side-signal for fleet correlation: it MUST NOT change the audit
/// exit code, the report/envelope contents, or the finding set, and MUST NOT
/// panic. `append_violation` returning `Err` (e.g. an unwritable store) is
/// silently ignored — the audit proceeds unchanged. A clean (empty `findings`)
/// run emits nothing.
fn emit_audit_violations(repo_root: &Path, findings: &[auditmap::StructuralFinding]) {
    if findings.is_empty() {
        return;
    }
    let session_id = std::env::var("CLAUDE_CODE_SESSION_ID").unwrap_or_default();
    let ts = overwatch::store::now();
    for f in findings {
        let kind = f.kind.as_str();
        let symbol = f.key.as_str();
        let task_key = format!("{kind}:{symbol}");
        let raw = overwatch::violation::RawViolation {
            drift_kind: Some(kind),
            symbol: Some(symbol),
            ..Default::default()
        };
        let event = overwatch::violation::build_event(
            overwatch::violation::ViolationSource::Specguard,
            &raw,
            task_key,
            session_id.clone(),
            ts,
            Some(f.detail.clone()),
        );
        // Fail-soft: an unwritable store must not affect the audit at all.
        if let Some(event) = event {
            let _ = overwatch::store::append_violation(repo_root, &event);
        }
    }
}

/// Human-readable dump of the map for `specguard map list`.
/// A view of `map` keeping only the entries matching `filter` via the shared
/// [`specmap::entry_matches`] predicate (an empty/blank filter keeps every
/// entry, so the result equals the input). `last_synced` is preserved so the
/// filtered listing still reports provenance. Used by `map list --filter` for
/// both the human and `--json` output.
fn filter_map(map: &specmap::SpecMap, filter: &str) -> specmap::SpecMap {
    specmap::SpecMap {
        last_synced: map.last_synced.clone(),
        entries: map
            .entries
            .iter()
            .filter(|(_, e)| specmap::entry_matches(e, filter))
            .map(|(k, e)| (k.clone(), e.clone()))
            .collect(),
    }
}

fn print_map(map: &specmap::SpecMap, map_path: &Path) {
    if map.is_empty() {
        println!("specguard map: (empty) [{}]", map_path.display());
        return;
    }
    println!(
        "specguard map: {} entr{} (last synced: {}) [{}]",
        map.len(),
        if map.len() == 1 { "y" } else { "ies" },
        if map.last_synced.is_empty() {
            "(never)"
        } else {
            &map.last_synced
        },
        map_path.display()
    );
    for (key, entry) in &map.entries {
        let spec = entry.spec_doc.as_deref().unwrap_or("(no spec-doc)");
        println!(
            "  [{}] {} ({}) -> {} | impl:{} test:{} client:{}",
            entry.status.as_str(),
            key,
            match entry.kind {
                specmap::EntryKind::Feature => "feature",
                specmap::EntryKind::Endpoint => "endpoint",
            },
            spec,
            entry.impl_files.len(),
            entry.test_files.len(),
            entry.client_refs.len(),
        );
    }
}

/// Contract judgment shared by every ratify path (human `accept_prompt` AND the
/// graded auto-ratify branch of [`ratification_block`]): the templates must not
/// contradict the render/parse contract (required placeholders present). The
/// policy/constitution part is the human's responsibility, recorded as the
/// rationale — this only checks the mechanical contract. Verify templates are
/// contract-checked only when their gate is active (consent is scoped to the
/// live policy surface; see [`ratify::drifted`]). Returns one entry per checked
/// template with its (possibly empty) list of missing placeholders; the caller
/// decides what "any non-empty" means for its path.
fn contract_violations(l: &Loaded) -> Vec<(&'static str, Vec<&'static str>)> {
    let mut violations: Vec<(&'static str, Vec<&'static str>)> = vec![
        (
            "audit-prompt",
            prompt::missing_placeholders(&l.template, prompt::AUDIT_PLACEHOLDERS),
        ),
        (
            "decisions-prompt",
            prompt::missing_placeholders(
                prompt::DECISIONS_TEMPLATE,
                prompt::DECISIONS_PLACEHOLDERS,
            ),
        ),
    ];
    if l.cfg.verify.enabled {
        violations.push((
            "refute-prompt",
            prompt::missing_placeholders(prompt::REFUTE_TEMPLATE, prompt::REFUTE_PLACEHOLDERS),
        ));
    }
    if l.cfg.verify.completeness {
        violations.push((
            "completeness-prompt",
            prompt::missing_placeholders(
                prompt::COMPLETENESS_TEMPLATE,
                prompt::COMPLETENESS_PLACEHOLDERS,
            ),
        ));
    }
    violations
}

/// Render `violations` (as returned by [`contract_violations`]) into the
/// human-facing refusal message, or `None` if there are no violations.
fn format_contract_violations(violations: &[(&'static str, Vec<&'static str>)]) -> Option<String> {
    if !violations.iter().any(|(_, m)| !m.is_empty()) {
        return None;
    }
    let mut msg = String::from("prompt が契約に矛盾 (必須 placeholder 不足); 批准を拒否:");
    for (name, miss) in violations {
        if !miss.is_empty() {
            msg.push_str(&format!("\n  {name}: {}", miss.join(", ")));
        }
    }
    Some(msg)
}

/// Ratify the prompt templates (meta-canon): contract-check then pin the
/// current version with a rationale. This is the consent ceremony — it confers
/// canon authority on the prompt version, recorded against the canon commit.
fn accept_prompt(l: &Loaded, reason: &str) -> Result<u8> {
    if reason.trim().is_empty() {
        anyhow::bail!("批准には理由が必要です (-m \"...\")");
    }
    let violations = contract_violations(l);
    if let Some(msg) = format_contract_violations(&violations) {
        anyhow::bail!(msg);
    }

    let head = scope::current_head(&l.repo_root).unwrap_or_else(|_| "UNKNOWN".to_string());
    // Pin only the live policy: a gate that is off leaves its slot empty, so
    // turning it on later registers as drift and demands a fresh ratification.
    let mut hashes = current_hashes(l);
    ratify::mask_inactive(&mut hashes, l.cfg.verify.enabled, l.cfg.verify.completeness);
    // Pin the ratified texts (graded-gate precedent corpus) for exactly the same
    // live surface as the hashes — an inactive gate contributes no precedent.
    let mut texts = current_texts(l);
    ratify::mask_inactive(&mut texts, l.cfg.verify.enabled, l.cfg.verify.completeness);
    let path = ratify::write_lock(&l.repo_root, &hashes, &texts, &head, &l.date, reason)?;
    println!(
        "specguard: prompt (メタ正典) を批准した -> {}",
        path.display()
    );
    println!("  canon commit に pin (canon_commit: {head})");
    let mut pinned = vec!["audit", "decisions"];
    if l.cfg.verify.enabled {
        pinned.push("refute");
    }
    if l.cfg.verify.completeness {
        pinned.push("completeness");
    }
    println!("  pin したポリシー: {}", pinned.join(", "));
    println!("  理由: {reason}");
    Ok(EXIT_OK)
}

/// Fingerprints of all four prompt templates (meta-canon) as they stand now.
/// Used by the ratification gate; `ratify::drifted` decides which to enforce.
fn current_hashes(l: &Loaded) -> ratify::TemplateHashes {
    ratify::TemplateHashes {
        audit: ratify::hash(&l.template),
        decisions: ratify::hash(prompt::DECISIONS_TEMPLATE),
        refute: ratify::hash(prompt::REFUTE_TEMPLATE),
        completeness: ratify::hash(prompt::COMPLETENESS_TEMPLATE),
    }
}

/// The full texts of all four prompt templates (meta-canon) as they stand now.
/// Parallel to [`current_hashes`]; the graded gate ([`ratify::triage_drift`])
/// measures these against the lock's recorded corpus, and `accept_prompt` records
/// them as the new precedent.
fn current_texts(l: &Loaded) -> ratify::TemplateTexts {
    ratify::TemplateTexts {
        audit: l.template.clone(),
        decisions: prompt::DECISIONS_TEMPLATE.to_string(),
        refute: prompt::REFUTE_TEMPLATE.to_string(),
        completeness: prompt::COMPLETENESS_TEMPLATE.to_string(),
    }
}

/// Print every shard's rendered prompt (debug view for `specguard prompt`).
fn print_prompts(l: &Loaded, scope: &scope::Scope, shards: &[prompt::Shard]) {
    if shards.is_empty() {
        println!("(in-scope 領域なし・不変条件なし — 監査対象の shard がない)");
        return;
    }
    for (i, &sh) in shards.iter().enumerate() {
        if i > 0 {
            println!();
        }
        println!(
            "===== shard: {} =====",
            prompt::shard_label(&l.cfg, scope, sh)
        );
        print!("{}", render_shard_prompt(l, scope, sh));
    }
}

/// Assemble the merged human-readable report: a harness-built header + overall
/// scope, then each shard's body verbatim under a divider.
fn merge_report(
    cfg: &Config,
    scope: &scope::Scope,
    date: &str,
    head: &str,
    parsed: &[(String, parse::Parsed)],
) -> String {
    let labels: Vec<&str> = parsed.iter().map(|(l, _)| l.as_str()).collect();
    let mut out = format!("# {} 仕様↔実装 整合監査 {}\n\n", cfg.project.name, date);
    out.push_str("## スコープ (全体)\n");
    out.push_str(&format!("- baseline: `{}`", scope.baseline));
    if scope.fell_back {
        out.push_str(" (fallback)");
    }
    out.push('\n');
    // Provenance: the canon commit this verdict was judged against, so a past
    // report is reproducible and B/C classification has a temporal anchor.
    out.push_str(&format!("- canon commit (HEAD): `{head}`\n"));
    out.push_str(&format!(
        "- 変更ファイル数: {}\n",
        scope.changed_files.len()
    ));
    out.push_str(&format!(
        "- shard: {}\n",
        join(&labels.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    ));
    out.push_str(
        "- 各 shard は独立した read-only エージェント (fresh context) で監査し、ここに統合した。\n",
    );
    for (label, p) in parsed {
        out.push_str(&format!("\n---\n\n## shard: {label}\n\n"));
        out.push_str(p.report.trim_end());
        out.push('\n');
    }
    out
}

/// Merge the sentinel summary across shards that flagged `needs_user`. A single
/// flagged shard contributes its summary verbatim; multiple shards are labelled.
fn merge_summary(parsed: &[(String, parse::Parsed)]) -> String {
    let flagged: Vec<(&str, &str)> = parsed
        .iter()
        .filter(|(_, p)| p.needs_user && !p.summary.trim().is_empty())
        .map(|(l, p)| (l.as_str(), p.summary.trim()))
        .collect();
    match flagged.as_slice() {
        [] => String::new(),
        [(_, s)] => s.to_string(),
        many => many
            .iter()
            .map(|(l, s)| format!("[{l}] {s}"))
            .collect::<Vec<_>>()
            .join(" / "),
    }
}

fn print_scope(cfg: &Config, scope: &scope::Scope) {
    println!("baseline: {}", scope.baseline);
    if scope.fell_back {
        println!("(baseline fell back from configured/recorded ref)");
    }
    println!("changed files: {}", scope.changed_files.len());
    println!("in-scope areas:");
    if scope.in_scope.is_empty() {
        println!("  (none)");
    }
    for hit in &scope.in_scope {
        let canon_note = if hit.changed_canon.is_empty() {
            String::new()
        } else {
            format!(", canon changed: {}", hit.changed_canon.len())
        };
        println!(
            "  - {} ({} file(s){})",
            cfg.areas[hit.area_index].name,
            hit.matched_files.len(),
            canon_note
        );
    }
    println!("skipped areas: {}", join(&scope.skipped_areas));
    println!(
        "invariants (always): {}",
        join(
            &cfg.invariants
                .iter()
                .map(|i| i.name.clone())
                .collect::<Vec<_>>()
        )
    );
    println!("decision records (D3): {}", scope.decision_files.len());
}

fn join(items: &[String]) -> String {
    if items.is_empty() {
        "(none)".to_string()
    } else {
        items.join(", ")
    }
}

/// Resolve the template text: explicit `[prompt].template` path, else embedded.
fn load_template(cfg: &Config, config_dir: &Path) -> Result<String> {
    if cfg.prompt.template.trim().is_empty() {
        Ok(prompt::DEFAULT_TEMPLATE.to_string())
    } else {
        let p = config_dir.join(&cfg.prompt.template);
        std::fs::read_to_string(&p)
            .with_context(|| format!("reading prompt template {}", p.display()))
    }
}

/// Strict `YYYY-MM-DD` calendar-date shape: 4-digit year, 2-digit month,
/// 2-digit day, hyphen-separated, ASCII digits only. This is a shape guard
/// (not a semantic calendar check), enough to reject arbitrary/hostile env
/// strings before they are echoed into the report as the run date.
fn is_calendar_date(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() != 10 {
        return false;
    }
    let digit = |i: usize| b[i].is_ascii_digit();
    (0..4).all(digit)
        && b[4] == b'-'
        && digit(5)
        && digit(6)
        && b[7] == b'-'
        && digit(8)
        && digit(9)
}

fn resolve_date(cli_date: Option<String>) -> String {
    if let Some(d) = cli_date {
        return d;
    }
    if let Ok(d) = std::env::var("SPECGUARD_NOW") {
        let d = d.trim();
        // Only honor SPECGUARD_NOW when it matches a strict calendar-date shape.
        // Any other value (garbage, shell metacharacters, injection attempts) is
        // ignored and we fall back to today's date rather than echoing it back.
        if is_calendar_date(d) {
            return d.to_string();
        }
    }
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

/// Canonicalize a path, falling back to the joined path if it doesn't exist yet.
fn canonicalize(p: &Path) -> Result<PathBuf> {
    std::fs::canonicalize(p).or_else(|_| Ok(p.to_path_buf()))
}

#[cfg(test)]
#[allow(clippy::panic)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn make_paths(sentinel: PathBuf) -> report::Paths {
        report::Paths {
            report: sentinel.with_extension("report.md"),
            last_ref: sentinel.with_extension("last-ref"),
            sentinel,
        }
    }

    // repo_root for git operations — use the harness workspace root.
    fn repo_root() -> PathBuf {
        let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        p.pop(); // up from crates/specguard → crates/
        p.pop(); // up from crates/ → workspace root
        p
    }

    // SPECGUARD_NOW is process-global; serialize the tests that touch it so a
    // parallel test never observes a half-set value.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn resolve_date_ignores_non_date_env() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();

        let hostile = "not-a-date; rm -rf /";
        std::env::set_var("SPECGUARD_NOW", hostile);
        let got = resolve_date(None);
        std::env::remove_var("SPECGUARD_NOW");

        // A garbage env value must NOT be echoed back; we fall back to today.
        assert_ne!(got, hostile);
        assert_eq!(got, today);
    }

    #[test]
    fn resolve_date_honors_valid_env_date() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("SPECGUARD_NOW", "2026-07-07");
        let got = resolve_date(None);
        std::env::remove_var("SPECGUARD_NOW");
        assert_eq!(got, "2026-07-07");
    }

    // -- overwatch fleet-violation emission (fail-soft side signal) ----------

    fn finding(kind: auditmap::StructuralKind, key: &str) -> auditmap::StructuralFinding {
        auditmap::StructuralFinding {
            key: key.to_string(),
            kind,
            detail: "detail".to_string(),
        }
    }

    // Run `body` with HOME pointed at `home`, serialized against the shared
    // env-mutation lock (HOME is process-global, like SPECGUARD_NOW).
    fn with_home<R>(home: &Path, body: impl FnOnce() -> R) -> R {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev = std::env::var_os("HOME");
        std::env::set_var("HOME", home);
        let out = body();
        match prev {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        out
    }

    #[test]
    fn emit_audit_violations_appends_signed_violation_per_finding() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();

        let findings = vec![
            finding(auditmap::StructuralKind::Undocumented, "crate::foo::Bar"),
            finding(auditmap::StructuralKind::Untested, "crate::baz::Qux"),
        ];

        let events = with_home(tmp.path(), || {
            std::env::set_var("CLAUDE_CODE_SESSION_ID", "sess-123");
            emit_audit_violations(&repo, &findings);
            std::env::remove_var("CLAUDE_CODE_SESSION_ID");
            overwatch::store::read_violations(&repo).unwrap()
        });

        assert_eq!(events.len(), 2);
        // Signature shape: specguard:<kind>:<symbol> (lowercased by overwatch).
        let sigs: Vec<&str> = events.iter().map(|e| e.signature.as_str()).collect();
        assert!(sigs.contains(&"specguard:undocumented:crate::foo::bar"));
        assert!(sigs.contains(&"specguard:untested:crate::baz::qux"));
        for e in &events {
            assert_eq!(e.source, overwatch::violation::ViolationSource::Specguard);
            assert_eq!(e.session_id, "sess-123");
        }
    }

    #[test]
    fn emit_audit_violations_clean_run_emits_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();

        let events = with_home(tmp.path(), || {
            emit_audit_violations(&repo, &[]);
            overwatch::store::read_violations(&repo).unwrap()
        });
        assert!(
            events.is_empty(),
            "a no-findings run must emit no violation"
        );
    }

    #[test]
    fn emit_audit_violations_does_not_change_audit_outputs() {
        // The emit is a pure side signal: given the SAME findings slice before and
        // after, the finding set the caller uses is untouched (we pass by &ref and
        // never mutate), and the function returns () — it cannot alter exit codes.
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let findings = vec![finding(auditmap::StructuralKind::Undocumented, "a::b")];
        let before = findings.clone();
        with_home(tmp.path(), || emit_audit_violations(&repo, &findings));
        assert_eq!(findings, before, "findings set must be unchanged by emit");
    }

    #[test]
    fn emit_audit_violations_fail_soft_when_store_unwritable() {
        // Point HOME at a *file* so `~/.overwatch/...` can never be created:
        // create_dir_all under a non-directory path fails, append_violation
        // returns Err, and emit must swallow it without panicking.
        let tmp = tempfile::tempdir().unwrap();
        let home_file = tmp.path().join("home-is-a-file");
        std::fs::write(&home_file, b"not a dir").unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let findings = vec![finding(auditmap::StructuralKind::Untested, "x::y")];
        // Must NOT panic.
        with_home(&home_file, || emit_audit_violations(&repo, &findings));
    }

    #[test]
    fn filter_map_narrows_to_matching_entries() {
        use specmap::{EntryKind, MapEntry, SpecMap, Status};
        fn feat(key: &str, impl_file: &str) -> MapEntry {
            MapEntry {
                key: key.to_string(),
                kind: EntryKind::Feature,
                spec_doc: None,
                status: Status::Tracked,
                last_ref: None,
                impl_files: vec![impl_file.to_string()],
                test_files: vec![],
                client_refs: vec![],
                api: None,
            }
        }
        let mut map = SpecMap {
            last_synced: "cafef00d".to_string(),
            ..SpecMap::default()
        };
        map.entries
            .insert("login".to_string(), feat("login", "src/login.rs"));
        map.entries
            .insert("logout".to_string(), feat("logout", "src/logout.rs"));

        // Filter by a substring present in only one entry → only it remains.
        let narrowed = filter_map(&map, "login");
        assert_eq!(narrowed.entries.len(), 1);
        assert!(narrowed.entries.contains_key("login"));
        assert!(!narrowed.entries.contains_key("logout"));
        // last_synced provenance is preserved in the filtered view.
        assert_eq!(narrowed.last_synced, "cafef00d");

        // An empty/blank filter is backward compatible: every entry is kept.
        assert_eq!(filter_map(&map, "").entries.len(), 2);
        assert_eq!(filter_map(&map, "   ").entries.len(), 2);
    }

    #[test]
    fn ack_no_sentinel_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let paths = make_paths(dir.path().join("sentinel.txt"));
        let result = ack(&paths, &repo_root(), false).unwrap();
        assert_eq!(result, EXIT_OK);
    }

    #[test]
    fn ack_force_bypasses_commit_check() {
        let dir = tempfile::tempdir().unwrap();
        let sentinel = dir.path().join("sentinel.txt");
        // Write a sentinel with raised_at = current HEAD so without force it would block.
        let head = scope::current_head(&repo_root()).unwrap_or_else(|_| "abc123".to_string());
        std::fs::write(&sentinel, format!("date: 2026-06-26\nraised_at: {head}\n")).unwrap();
        let paths = make_paths(sentinel.clone());
        let result = ack(&paths, &repo_root(), true).unwrap();
        assert_eq!(result, EXIT_OK);
        assert!(!sentinel.exists(), "sentinel should have been removed");
    }

    #[test]
    fn ack_blocks_when_no_new_commits_since_raised() {
        let dir = tempfile::tempdir().unwrap();
        let sentinel = dir.path().join("sentinel.txt");
        let head = scope::current_head(&repo_root()).unwrap_or_else(|_| "abc123".to_string());
        std::fs::write(&sentinel, format!("date: 2026-06-26\nraised_at: {head}\n")).unwrap();
        let paths = make_paths(sentinel.clone());
        let result = ack(&paths, &repo_root(), false).unwrap();
        assert_eq!(result, EXIT_NO_FIX_COMMIT);
        assert!(sentinel.exists(), "sentinel should NOT have been removed");
    }

    #[test]
    fn ack_blocks_when_current_head_errors() {
        // CA-specguard-003 regression: repo_root points at a non-git directory,
        // so scope::current_head() errors. ack must fail closed (deny, keep
        // sentinel) rather than treating the error as a HEAD change.
        let non_git_dir = tempfile::tempdir().unwrap();
        let sentinel_dir = tempfile::tempdir().unwrap();
        let sentinel = sentinel_dir.path().join("sentinel.txt");
        std::fs::write(
            &sentinel,
            "date: 2026-06-26\nraised_at: 0000000000000000000000000000000000000000\n",
        )
        .unwrap();
        let paths = make_paths(sentinel.clone());
        assert!(scope::current_head(non_git_dir.path()).is_err());
        let result = ack(&paths, non_git_dir.path(), false).unwrap();
        assert_eq!(result, EXIT_NO_FIX_COMMIT);
        assert!(
            sentinel.exists(),
            "sentinel should NOT have been removed when HEAD can't be resolved"
        );
    }

    #[test]
    fn ack_succeeds_when_new_commit_exists() {
        let dir = tempfile::tempdir().unwrap();
        let sentinel = dir.path().join("sentinel.txt");
        // A different (old) commit sha → new commits exist.
        std::fs::write(
            &sentinel,
            "date: 2026-06-25\nraised_at: 0000000000000000000000000000000000000000\n",
        )
        .unwrap();
        let paths = make_paths(sentinel.clone());
        let result = ack(&paths, &repo_root(), false).unwrap();
        assert_eq!(result, EXIT_OK);
        assert!(!sentinel.exists(), "sentinel should have been removed");
    }

    #[test]
    fn ack_old_sentinel_without_raised_at_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let sentinel = dir.path().join("sentinel.txt");
        // Old-format sentinel with no raised_at field — guard is skipped.
        std::fs::write(
            &sentinel,
            "date: 2026-01-01\nreport: reports/spec-audit/2026-01-01.md\nsummary: some drift\n",
        )
        .unwrap();
        let paths = make_paths(sentinel.clone());
        let result = ack(&paths, &repo_root(), false).unwrap();
        assert_eq!(result, EXIT_OK);
        assert!(!sentinel.exists(), "sentinel should have been removed");
    }

    #[test]
    fn ack_blocks_when_raised_at_poisoned_even_with_healthy_head() {
        // CA-specguard-005 regression: simulate a sentinel that was RAISED at
        // a moment when scope::current_head() errored (e.g. git unavailable),
        // so `finish()` fell back to `report::POISONED_RAISED_AT` instead of a
        // real hash (see main.rs `finish`). Later, git is healthy again and
        // `ack` (no --force) resolves a perfectly normal current HEAD with
        // zero fix commits made since the raise. Without the fix, a plain
        // string inequality (`POISONED_RAISED_AT != <real head>`) would read
        // as "a new commit happened" and let ack clear the sentinel for free
        // — the exact ack-bypass this test guards against.
        let dir = tempfile::tempdir().unwrap();
        let sentinel = dir.path().join("sentinel.txt");
        std::fs::write(
            &sentinel,
            format!(
                "date: 2026-07-14\nraised_at: {}\n",
                report::POISONED_RAISED_AT
            ),
        )
        .unwrap();
        let paths = make_paths(sentinel.clone());
        // repo_root() is a real, healthy git repo, so current_head() inside
        // ack() resolves successfully to some real (non-poisoned) hash —
        // exercising the "git recovered" scenario, not the CA-specguard-003
        // (current_head errors during ack) path.
        assert!(scope::current_head(&repo_root()).is_ok());
        let result = ack(&paths, &repo_root(), false).unwrap();
        assert_eq!(result, EXIT_NO_FIX_COMMIT);
        assert!(
            sentinel.exists(),
            "sentinel raised with a poisoned raised_at must NOT be clearable \
             by ack without --force, even once HEAD resolves normally again"
        );
    }

    #[test]
    fn ack_force_still_clears_poisoned_sentinel() {
        // --force must remain the escape hatch regardless of raised_at value.
        let dir = tempfile::tempdir().unwrap();
        let sentinel = dir.path().join("sentinel.txt");
        std::fs::write(
            &sentinel,
            format!(
                "date: 2026-07-14\nraised_at: {}\n",
                report::POISONED_RAISED_AT
            ),
        )
        .unwrap();
        let paths = make_paths(sentinel.clone());
        let result = ack(&paths, &repo_root(), true).unwrap();
        assert_eq!(result, EXIT_OK);
        assert!(!sentinel.exists(), "sentinel should have been removed");
    }

    // --- content-hash memoization (skip unchanged shards) ---

    fn memo_cfg() -> Config {
        toml::from_str(
            r#"
            [project]
            name = "x"
            [[area]]
            name = "src"
            globs = ["src/**"]
            canon = ["spec.md"]
            "#,
        )
        .unwrap()
    }

    fn memo_scope(matched: Vec<String>) -> scope::Scope {
        scope::Scope {
            baseline: "abc".into(),
            fell_back: false,
            changed_files: vec![],
            in_scope: vec![scope::AreaHit {
                area_index: 0,
                matched_files: matched,
                changed_canon: vec![],
            }],
            skipped_areas: vec![],
            decision_files: vec![],
        }
    }

    /// (a) The skip path: on a first run there is nothing stored yet (not
    /// cached — identical to pre-memoization behavior); after persisting the
    /// fingerprint a green audit would have recorded, an unchanged re-run
    /// classifies the shard as cached.
    #[test]
    fn classify_cache_skip_path_unchanged_input_is_cached() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("spec.md"), "spec v1").unwrap();
        std::fs::write(tmp.path().join("main.rs"), "fn main() {}").unwrap();
        let cfg = memo_cfg();
        let scope = memo_scope(vec!["main.rs".to_string()]);
        let shards = vec![prompt::Shard::Area(0)];

        let empty = std::collections::HashMap::new();
        let first = classify_cache(&cfg, &scope, &shards, tmp.path(), &empty);
        assert!(!first[0].cached, "first run has no stored fingerprint yet");

        let mut stored = std::collections::HashMap::new();
        let files = scope::shard_input_files(&cfg, &scope, prompt::Shard::Area(0));
        stored.insert(
            first[0].label.clone(),
            scope::fingerprint_files(tmp.path(), &files),
        );
        let second = classify_cache(&cfg, &scope, &shards, tmp.path(), &stored);
        assert!(
            second[0].cached,
            "unchanged input must be classified as cached"
        );
    }

    /// (b) The re-audit path: the shard's implementation file changes ->
    /// fingerprint diverges from the stored one -> NOT cached.
    #[test]
    fn classify_cache_reaudit_path_changed_input_is_not_cached() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("spec.md"), "spec v1").unwrap();
        std::fs::write(tmp.path().join("main.rs"), "fn main() {}").unwrap();
        let cfg = memo_cfg();
        let scope = memo_scope(vec!["main.rs".to_string()]);
        let shards = vec![prompt::Shard::Area(0)];

        let mut stored = std::collections::HashMap::new();
        let files = scope::shard_input_files(&cfg, &scope, prompt::Shard::Area(0));
        let label = prompt::shard_label(&cfg, &scope, prompt::Shard::Area(0));
        stored.insert(label, scope::fingerprint_files(tmp.path(), &files));

        std::fs::write(tmp.path().join("main.rs"), "fn main() { /* changed */ }").unwrap();
        let after = classify_cache(&cfg, &scope, &shards, tmp.path(), &stored);
        assert!(!after[0].cached, "changed input must trigger a re-audit");
    }

    #[test]
    fn fingerprints_roundtrip_through_read_write() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".shard-fingerprints");
        let mut map = std::collections::HashMap::new();
        map.insert("src".to_string(), "deadbeef".to_string());
        map.insert("invariants".to_string(), "cafebabe".to_string());
        write_fingerprints(&path, &map).unwrap();
        assert_eq!(read_fingerprints(&path), map);
    }

    #[test]
    fn read_fingerprints_missing_file_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".nope");
        assert!(read_fingerprints(&path).is_empty());
    }

    /// `persist_fingerprints` records a shard that came back clean; a shard
    /// still flagged `needs_user` has its (possibly stale) entry dropped
    /// instead, so it keeps being re-audited rather than silently skipped.
    #[test]
    fn persist_fingerprints_only_records_clean_shards() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("spec.md"), "spec v1").unwrap();
        std::fs::write(tmp.path().join("main.rs"), "fn main() {}").unwrap();
        let cfg = memo_cfg();
        let scope = memo_scope(vec!["main.rs".to_string()]);
        let shards = vec![prompt::Shard::Area(0)];
        let label = prompt::shard_label(&cfg, &scope, prompt::Shard::Area(0));

        let l = Loaded {
            cfg: cfg.clone(),
            repo_root: tmp.path().to_path_buf(),
            template: String::new(),
            date: "2026-01-01".to_string(),
        };
        let paths = make_paths(tmp.path().join("sentinel.txt"));

        let parsed = vec![(
            label.clone(),
            parse::parse("body\n<<<SPEC_AUDIT>>>\nneeds_user: no\nsummary: ok"),
        )];
        persist_fingerprints(&l, &scope, &shards, &paths, &parsed);
        let stored = read_fingerprints(&fingerprint_state_path(&paths));
        assert!(stored.contains_key(&label), "clean shard must be persisted");

        let dirty = vec![(
            label.clone(),
            parse::parse("body\n<<<SPEC_AUDIT>>>\nneeds_user: yes\nsummary: drift"),
        )];
        persist_fingerprints(&l, &scope, &shards, &paths, &dirty);
        let stored_after = read_fingerprints(&fingerprint_state_path(&paths));
        assert!(
            !stored_after.contains_key(&label),
            "dirty shard's stale entry must be dropped"
        );
    }

    // --- progressive scope escalation (t4 Part B) ---

    fn shard_out(label: &str, stdout: &str, code: i32) -> agent::ShardOutput {
        agent::ShardOutput {
            label: label.to_string(),
            out: agent::AgentOutput {
                stdout: stdout.to_string(),
                stderr: String::new(),
                code,
            },
        }
    }

    /// Only AREA shards with a clean exit that emitted the signal are eligible:
    /// an invariants shard emitting it, and a crashed area shard, are excluded.
    #[test]
    fn insufficient_context_shards_selects_only_clean_area_signals() {
        let shards = vec![
            prompt::Shard::Area(0),
            prompt::Shard::Invariants,
            prompt::Shard::Area(1),
        ];
        let sig = prompt::NEEDS_WIDER_SCOPE_SIGNAL;
        let outs = vec![
            shard_out("a0", &format!("need more\n{sig}\n"), 0), // eligible
            shard_out("inv", sig, 0),                           // excluded: not an area
            shard_out("a1", &format!("crashed {sig}"), 4),      // excluded: nonzero exit
        ];
        assert_eq!(insufficient_context_shards(&shards, &outs), vec![0]);

        // A clean area shard with no signal flags nothing.
        let clean = vec![
            shard_out("a0", "all good", 0),
            shard_out("inv", "ok", 0),
            shard_out("a1", "ok", 0),
        ];
        assert!(insufficient_context_shards(&shards, &clean).is_empty());
    }

    /// The escalation core re-dispatches each flagged shard EXACTLY once (the
    /// injected runner is `FnOnce` and asserts it was called with just the
    /// flagged shard) and replaces only that shard's output.
    #[test]
    fn escalate_core_redispatches_flagged_once_and_replaces() {
        let outs = vec![
            shard_out("a0", "first-pass a0", 0),
            shard_out("a1", "first-pass a1 (needs wider)", 0),
            shard_out("a2", "first-pass a2", 0),
        ];
        let flagged = vec![1usize];

        let widen = |i: usize| agent::ShardPrompt {
            label: format!("widened:{i}"),
            prompt: format!("WIDENED PROMPT for {i}"),
        };

        let calls = std::cell::Cell::new(0usize);
        let run = |prompts: Vec<agent::ShardPrompt>| {
            calls.set(calls.get() + 1);
            // Exactly the flagged shard is re-dispatched, with the widened prompt.
            assert_eq!(prompts.len(), 1);
            assert_eq!(prompts[0].label, "widened:1");
            assert!(prompts[0].prompt.contains("WIDENED PROMPT for 1"));
            vec![shard_out("a1", "SECOND-PASS a1 widened", 0)]
        };

        let merged = escalate_core(outs, &flagged, widen, run);
        assert_eq!(calls.get(), 1, "exactly one re-dispatch");
        // Only the flagged shard's output was replaced.
        assert_eq!(merged[0].out.stdout, "first-pass a0");
        assert_eq!(merged[1].out.stdout, "SECOND-PASS a1 widened");
        assert_eq!(merged[2].out.stdout, "first-pass a2");
    }

    /// No flagged shard -> the injected runner is never called and outputs pass
    /// through untouched (the fully-sufficient / feature-quiet path).
    #[test]
    fn escalate_core_noop_when_nothing_flagged() {
        let outs = vec![shard_out("a0", "ok", 0), shard_out("a1", "ok", 0)];
        let run = |_: Vec<agent::ShardPrompt>| -> Vec<agent::ShardOutput> {
            panic!("runner must not be called when nothing is flagged");
        };
        let merged = escalate_core(outs, &[], |_| unreachable!(), run);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].out.stdout, "ok");
        assert_eq!(merged[1].out.stdout, "ok");
    }

    #[test]
    fn relevant_map_enabled_reads_env() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var("SPECGUARD_RELEVANT_MAP");
        assert!(!relevant_map_enabled(), "default (unset) is off");
        std::env::set_var("SPECGUARD_RELEVANT_MAP", "1");
        assert!(relevant_map_enabled());
        std::env::set_var("SPECGUARD_RELEVANT_MAP", "yes");
        assert!(relevant_map_enabled());
        std::env::set_var("SPECGUARD_RELEVANT_MAP", "0");
        assert!(!relevant_map_enabled());
        std::env::remove_var("SPECGUARD_RELEVANT_MAP");
    }
}
