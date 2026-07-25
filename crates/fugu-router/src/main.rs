//! fugu-router — fugu-style per-model routing for Claude Code orchestration.
//!
//! fugu (Sakana AI) hides a trained coordinator that routes each request across
//! a pool of models by role. We can't train Claude's weights, so the coordinator
//! becomes a deterministic policy over a *retrieval* store: record which model
//! passed verification on which kind of task (and at what cost), then pick the
//! cheapest tier that historically clears similar work. condukt calls `route` to
//! set each task's `suggested_model`, and `record` to feed outcomes back.

mod budget;
mod confidence;
mod config;
mod decomp;
mod fingerprint;
mod inject;
mod install;
mod mode;
mod pathutil;
mod policy;
mod rag;
mod rng;
mod semantic;
mod store;

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use harness_core::hook::{read_stdin, run_hook, HookInput};
use serde_json::json;
use wait_timeout::ChildExt;

#[derive(Parser)]
#[command(
    name = "fugu-router",
    about = "fugu-style per-model routing for Claude Code orchestration"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

// `Record` carries many optional measurement/provenance fields (routing
// basis/confidence/rationale, lines-changed, token usage) alongside the
// smaller variants; it's a short-lived CLI arg struct consumed once per
// process, so the size delta doesn't warrant boxing every field.
#[allow(clippy::large_enum_variant)]
#[derive(Subcommand)]
enum Command {
    /// Enrich a condukt decomposition: set each task's suggested_model from
    /// routing memory. Reads JSON (--file or stdin), writes JSON to stdout.
    Route {
        #[arg(long)]
        file: Option<PathBuf>,
        /// Also write a per-task routing report (worker/verifier/basis) here.
        #[arg(long)]
        report: Option<PathBuf>,
        /// Aggressiveness preset applied on top of the policy's pick: `fast`
        /// (one tier down, opus never selected), `normal` (identity,
        /// back-compat default), `high` (one tier up, capped at opus).
        /// Precedence: this flag > `FUGU_ROUTER_MODE` env > config.toml
        /// `mode` > `normal`. A `gated` task is unaffected by any mode.
        #[arg(long)]
        mode: Option<mode::Mode>,
    },
    /// Record one task outcome into the episode store (the learning signal).
    Record {
        #[arg(long)]
        title: String,
        #[arg(long, default_value = "")]
        files: String,
        #[arg(long, default_value = "")]
        class: String,
        #[arg(long)]
        model: String,
        /// verified | failed (anything but a pass-word counts as a non-pass)
        #[arg(long)]
        status: String,
        #[arg(long, default_value = "worker")]
        role: String,
        #[arg(long, default_value_t = 0.0)]
        cost: f64,
        /// Acceptance criteria for this task (persisted to playbook store when pass).
        #[arg(long, default_value = "")]
        done_criteria: String,
        /// Optional free-text notes about how the task was solved.
        #[arg(long, default_value = "")]
        notes: String,
        /// Fingerprint of the active SKILL.md corpus (see `fugu-router fingerprint`).
        #[arg(long, default_value = "")]
        skill_fingerprint: String,
        /// Measured wall-clock duration (seconds) of the worker/verifier task,
        /// if known (e.g. from condukt's started_at/updated_at). Measurement
        /// only — never consulted by routing/scoring. OMIT the flag to record
        /// the episode as unmeasured; a non-positive value is REJECTED (exit
        /// non-zero) because `0.0`/negative would be an unrepresentable /
        /// unmeasured measurement — omit the flag instead.
        #[arg(long)]
        duration: Option<f64>,
        /// Which delegation strategy (fork | inline) produced this episode,
        /// when manually recorded for a delegation-strategy comparison. Free
        /// text, not validated (same looseness as --class). Omit for ordinary
        /// worker/verifier records unrelated to that comparison.
        #[arg(long)]
        delegation: Option<String>,
        /// The routing `Decision`'s `basis` ("learned"|"prior"|"gated") that
        /// put this task on --model, when the caller carried it through from
        /// `route.json` (see `fugu-router route --report`). Measurement
        /// only — never consulted by routing/scoring. Omit if unknown.
        #[arg(long)]
        route_basis: Option<String>,
        /// The routing `Decision`'s `confidence` ("high"|"low") at route time.
        /// Same provenance/measurement-only caveats as --route-basis.
        #[arg(long)]
        route_confidence: Option<String>,
        /// The routing `Decision`'s free-text `rationale` at route time. Same
        /// provenance/measurement-only caveats as --route-basis.
        #[arg(long)]
        route_rationale: Option<String>,
        /// `git diff --stat` insertion count for the task's commit(s), if the
        /// caller measured it before the branch was merged/removed.
        #[arg(long)]
        lines_added: Option<u64>,
        /// `git diff --stat` deletion count for the task's commit(s), if the
        /// caller measured it before the branch was merged/removed.
        #[arg(long)]
        lines_removed: Option<u64>,
        /// Input tokens consumed by the worker/verifier subagent, if the
        /// caller resolved it (e.g. condukt via `gauge subagents --json`).
        #[arg(long)]
        tokens_input: Option<u64>,
        /// Output tokens produced by the worker/verifier subagent, if the
        /// caller resolved it (e.g. condukt via `gauge subagents --json`).
        #[arg(long)]
        tokens_output: Option<u64>,
        /// The mode axis ("fast"|"normal"|"high") this episode was routed
        /// under, when the caller knows it. `None` (omit the flag) means
        /// "not recorded" — distinct from, and never coerced into,
        /// `Some("normal")`. Measurement only — never consulted by
        /// `policy::route`/`decide_bandit`.
        #[arg(long)]
        mode: Option<mode::Mode>,
    },
    /// Check whether an episode of the given class was recorded within the
    /// last N seconds. Exit 0 if found, 1 if not — lets a caller (e.g.
    /// /flow's delegation-recording step) verify its own `record` call
    /// actually landed instead of trusting a self-report.
    AuditRecent {
        #[arg(long)]
        class: String,
        #[arg(long)]
        within: u64,
    },
    /// Apply a human label to a recorded episode, overriding the verifier's
    /// self-pass in policy aggregation. The teacher signal that de-biases the
    /// verifier's self-reinforcing feedback loop (record's sibling).
    Label {
        /// Title substring selecting the episode to label (case-insensitive);
        /// the most recent match is chosen. Omit with --latest.
        selector: Option<String>,
        /// Human verdict for the episode.
        #[arg(long, value_parser = ["good", "bad"])]
        verdict: String,
        /// Who applied the label (provenance).
        #[arg(long, default_value = "human")]
        by: String,
        /// Label the most recently recorded episode regardless of title.
        #[arg(long)]
        latest: bool,
    },
    /// Search past task *procedures* (how similar verified tasks were solved) by
    /// k-NN. Distinct from the standalone `playbook` plugin, which injects curated
    /// knowledge notes. `playbook` is kept as a hidden alias for back-compat.
    #[command(alias = "playbook")]
    Procedures {
        #[command(subcommand)]
        action: ProceduresAction,
    },
    /// Cross-project *lessons* store — durable, machine-scope carryovers
    /// ("this error pattern means X", "this repo's convention is Y") that help
    /// any future task on any project. Project-INDEPENDENT path (not keyed by
    /// cwd/repo); the underlying store lives in harness-core::lessons.
    Lessons {
        #[command(subcommand)]
        action: LessonsAction,
    },
    /// Suggest a model for a single free-text task.
    Suggest {
        #[arg(long, default_value = "")]
        files: String,
        #[arg(long, default_value = "")]
        class: String,
        /// The task description (free text).
        text: Vec<String>,
        /// Aggressiveness preset — see `route --mode` for the semantics and
        /// precedence order (this flag > `FUGU_ROUTER_MODE` env > config.toml
        /// `mode` > `normal`).
        #[arg(long)]
        mode: Option<mode::Mode>,
    },
    /// Print a calibrated confidence in [0,1] that a task like this one will
    /// pass verification, derived from the historical k-NN pass-rate of
    /// similar episodes (Brier-informed, no LLM). Deterministic: same store
    /// state + same inputs => byte-identical output.
    Confidence {
        #[arg(long, default_value = "")]
        files: String,
        #[arg(long, default_value = "")]
        class: String,
        /// The task description (free text).
        text: Vec<String>,
    },
    /// Show per-model pass-rate / cost learned so far.
    Stats {
        #[arg(long)]
        json: bool,
    },
    /// Compare fork vs inline delegation-strategy episodes (class
    /// "flow-delegation"): count / pass-rate / avg cost / avg duration per
    /// bucket. See docs/design-delegation-strategy-measurement.md.
    DelegationStats {
        #[arg(long)]
        json: bool,
    },
    /// Flag models whose avg duration within a task class is a relative
    /// outlier vs other models in the same class, and check whether those
    /// outlier episodes have a lower effective pass rate — the measurement
    /// test for PDO hypothesis ae64db03 ("duration outliers signal an
    /// under-powered model that should be escalated"). Also prints duration
    /// coverage (measured / total episodes). Aliased as `duration`.
    #[command(alias = "duration")]
    DurationOutliers {
        #[arg(long)]
        json: bool,
        /// A model's per-class avg duration counts as an outlier when it
        /// exceeds the class's cross-model mean by this multiplier.
        #[arg(long, default_value_t = 1.5)]
        threshold: f64,
    },
    /// UserPromptSubmit hook: inject a routing-memory summary.
    Prompt,
    /// Write a starter fugu-router.toml in the current directory.
    Init {
        #[arg(default_value = "fugu-router.toml")]
        target: String,
    },
    /// Merge the UserPromptSubmit hook into ~/.claude/settings.json.
    Install {
        #[arg(long)]
        dry_run: bool,
    },
    /// Remove fugu-router hooks from ~/.claude/settings.json.
    Uninstall {
        #[arg(long)]
        dry_run: bool,
    },
    /// Sync the record store with a remote git repository (configured via sync_repo).
    /// Default: pull from remote first, then commit & push local changes.
    Sync {
        /// Only pull from remote (git clone/pull); do not push.
        #[arg(long)]
        pull_only: bool,
        /// Only commit & push local changes; do not pull first.
        #[arg(long)]
        push_only: bool,
    },
    /// Merge another machine's store file(s) into the local stores, deduplicating
    /// by content hash. At least one of --episodes or --playbooks must be given.
    /// With --dedup (and no source path), deduplicates the LOCAL stores in place.
    Import {
        /// Path to the source episodes JSONL to import.
        #[arg(long)]
        episodes: Option<PathBuf>,
        /// Path to the source playbooks JSONL to import.
        #[arg(long)]
        playbooks: Option<PathBuf>,
        /// Report counts only; write nothing.
        #[arg(long)]
        dry_run: bool,
        /// Dedup the LOCAL store(s) in place rather than importing from a source.
        #[arg(long)]
        dedup: bool,
    },
    /// Print a deterministic fingerprint of the SKILL.md corpus under a directory,
    /// so recorded outcomes can be tied to the skill version that produced them.
    Fingerprint {
        /// Directory to walk for SKILL.md files (default: current directory).
        #[arg(long)]
        dir: Option<PathBuf>,
    },
    /// Deterministic lexical code-symbol index (code-RAG slice-1): no
    /// embeddings/external API, a per-repo JSONL index built from git-tracked
    /// `.rs` files. The underlying scanner/store lives in
    /// harness_core::code_index.
    CodeIndex {
        #[command(subcommand)]
        action: CodeIndexAction,
    },
}

#[derive(Subcommand)]
enum CodeIndexAction {
    /// (Re)build the code index for a repo: enumerate git-tracked `.rs`
    /// files under `--root`, extract symbols, and rebuild the index file.
    Build {
        /// Repo root to index (default: current directory).
        #[arg(long)]
        root: Option<PathBuf>,
        /// Only rebuild when the source `.rs` set changed since the last
        /// build (code-RAG slice-3). Compares a cheap deterministic
        /// fingerprint (path+size+mtime, no content reads) against the sidecar
        /// meta; a match is a no-op (`rebuilt:false`). Without this flag,
        /// `build` always rebuilds (back-compat).
        #[arg(long)]
        if_stale: bool,
    },
    /// Lexical top-K search over a previously-built code index (returns a
    /// JSON array). An absent/empty index yields `[]` (fail-soft, exit 0).
    Search {
        /// Query text.
        #[arg(long)]
        query: String,
        /// Repo root whose index to search (default: current directory).
        #[arg(long)]
        root: Option<PathBuf>,
        /// Number of results to return.
        #[arg(long, default_value_t = harness_core::code_index::DEFAULT_K)]
        k: usize,
    },
}

#[derive(Subcommand)]
enum ProceduresAction {
    /// Search procedures for similar past tasks (returns JSON array).
    Search {
        /// Query text (task title / description).
        #[arg(long)]
        query: String,
        /// Comma-separated file paths to boost similarity.
        #[arg(long, default_value = "")]
        files: String,
        /// Number of results to return.
        #[arg(long, default_value_t = 3)]
        k: usize,
    },
}

#[derive(Subcommand)]
enum LessonsAction {
    /// Append a distilled lesson (idempotent by content-derived id).
    Add {
        /// Lesson kind: error-pattern | convention.
        #[arg(long, value_parser = ["error-pattern", "convention"])]
        kind: String,
        /// Short description of the task the lesson came from.
        #[arg(long)]
        task_summary: String,
        /// The lesson itself (the carryover text).
        #[arg(long)]
        lesson_text: String,
        /// Which run/session produced it (provenance).
        #[arg(long, default_value = "")]
        source_run: String,
    },
    /// Lexical top-K search over stored lessons (returns a JSON array).
    Search {
        /// Query text.
        #[arg(long)]
        query: String,
        /// Number of results to return.
        #[arg(long, default_value_t = 3)]
        k: usize,
    },
}

fn split_files(s: &str) -> Vec<String> {
    s.split(',')
        .map(|x| x.trim())
        .filter(|x| !x.is_empty())
        .map(String::from)
        .collect()
}

fn is_pass(status: &str) -> bool {
    matches!(status, "verified" | "pass" | "passed" | "ok" | "true")
}

/// Pick the episode to label: the most recent (`ts`, tie → last recorded) among
/// those matching `selector` (case-insensitive title substring), or the most
/// recent overall when `latest`. Pure for unit-testing.
fn select_episode_index(
    eps: &[store::Episode],
    selector: Option<&str>,
    latest: bool,
) -> Option<usize> {
    let needle = if latest {
        None
    } else {
        selector.map(|s| s.to_lowercase())
    };
    eps.iter()
        .enumerate()
        .filter(|(_, ep)| match &needle {
            Some(n) => ep.title.to_lowercase().contains(n),
            None => true,
        })
        .max_by_key(|(i, ep)| (ep.ts, *i))
        .map(|(i, _)| i)
}

/// Apply a human label to a recorded episode and rewrite the store. The label
/// overrides the verifier's self-pass in `policy::aggregate` (via
/// `Episode::effective_pass`).
fn cmd_label(
    cfg: &config::Config,
    selector: Option<String>,
    verdict: &str,
    by: &str,
    latest: bool,
) -> Result<()> {
    if selector.is_none() && !latest {
        anyhow::bail!("provide a title selector or --latest to choose an episode");
    }
    let path = cfg.store_path();
    let mut eps = store::load(&path);
    if eps.is_empty() {
        anyhow::bail!("no episodes recorded yet (store: {})", path.display());
    }
    let idx = select_episode_index(&eps, selector.as_deref(), latest).ok_or_else(|| {
        anyhow::anyhow!(
            "no episode title matched {:?}",
            selector.as_deref().unwrap_or("<latest>")
        )
    })?;
    let good = verdict == "good";
    eps[idx].human_label = Some(good);
    eps[idx].labeled_by = Some(by.to_string());
    store::save_all(&path, &eps).context("rewriting episode store")?;
    eprintln!(
        "labeled: \"{}\" [{}] human_label={} by={} (was pass={})",
        eps[idx].title, eps[idx].model, good, by, eps[idx].pass
    );
    Ok(())
}

/// Seed the PRNG from wall-clock (nanosecond) + store size so exploration varies
/// run-to-run — second-granularity would make rapid back-to-back routes identical.
fn seed_rng(eps_len: usize) -> rng::Rng {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    rng::Rng::new(nanos ^ (eps_len as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15))
}

/// Route one task: retrieve neighbours, then explore (bandit) or exploit (threshold).
fn route_decision(
    cfg: &config::Config,
    title: &str,
    files: &[String],
    class: &str,
    eps: &[store::Episode],
    rng: &mut rng::Rng,
) -> policy::Decision {
    let nb = rag::knn(title, files, eps, cfg.k, cfg.sim_threshold);
    if cfg.explore {
        policy::decide_bandit(
            title,
            files,
            class,
            &nb,
            cfg.pass_threshold,
            cfg.min_samples,
            rng,
        )
    } else {
        policy::decide(
            title,
            files,
            class,
            &nb,
            cfg.pass_threshold,
            cfg.min_samples,
        )
    }
}

fn main() {
    let cli = Cli::parse();
    match cli.command {
        Command::Prompt => run_hook(|| {
            if config::disabled_env() {
                return;
            }
            let cfg = config::Config::load();
            if !cfg.enabled {
                return;
            }
            let input = HookInput::parse(&read_stdin()).unwrap_or_default();
            if !inject::looks_actionable(&input.prompt) {
                return;
            }
            let eps = store::load(&cfg.store_path());
            if let Some(ctx) = inject::summary(&eps, cfg.inject_limit) {
                harness_core::inject_metrics::record(
                    "fugu-router",
                    &input.session_id,
                    &input.prompt,
                    ctx.chars().count(),
                );
                println!("{}", json!({ "additionalContext": ctx }));
            }
        }),
        other => {
            if let Err(e) = run_user(other) {
                eprintln!("fugu-router: {e:#}");
                std::process::exit(1);
            }
        }
    }
}

/// Resolve the effective mode for a CLI invocation: `mode::resolve` with the
/// live `FUGU_ROUTER_MODE` env var and `cfg.mode`. An invalid env/config
/// value becomes an `anyhow::Error` here, which `main()` turns into an
/// `eprintln!` + non-zero exit — never a silent fallback to `normal`.
fn resolve_mode(cli: Option<mode::Mode>, cfg: &config::Config) -> Result<mode::Mode> {
    let env_value = std::env::var("FUGU_ROUTER_MODE").ok();
    mode::resolve(cli, env_value, cfg.mode.as_deref()).map_err(anyhow::Error::msg)
}

fn run_user(cmd: Command) -> Result<()> {
    let cfg = config::Config::load();
    match cmd {
        Command::Route { file, report, mode } => {
            let mode = resolve_mode(mode, &cfg)?;
            cmd_route(&cfg, file, report, mode)
        }
        Command::Record {
            title,
            files,
            class,
            model,
            status,
            role,
            cost,
            done_criteria,
            notes,
            skill_fingerprint,
            duration,
            delegation,
            route_basis,
            route_confidence,
            route_rationale,
            lines_added,
            lines_removed,
            tokens_input,
            tokens_output,
            mode,
        } => {
            // A supplied --duration must be a real positive measurement.
            // `0.0`/negative means "unmeasured", which is represented by
            // OMITTING the flag (Episode.duration_secs = None), never by writing
            // an unrepresentable 0.0 that the store now reads back as unmeasured.
            let duration_secs = match duration {
                Some(d) if d > 0.0 => Some(d),
                Some(d) => anyhow::bail!(
                    "--duration must be a positive number of seconds (got {d}); 0 or a negative value means \"unmeasured\" — omit --duration to record the episode as unmeasured"
                ),
                None => None,
            };
            let raw_touched = split_files(&files);
            // Normalise absolute paths to repo-relative so stored paths are
            // portable across machines (no username / mount-point leakage).
            let repo_root = pathutil::repo_root_from_cwd();
            let touched = pathutil::normalise_paths(&raw_touched, repo_root.as_deref());
            let pass = is_pass(&status);
            let ep = store::Episode {
                ts: store::now_secs(),
                title: title.clone(),
                touched_files: touched.clone(),
                class: class.clone(),
                model,
                role,
                pass,
                cost_usd: cost,
                human_label: None,
                labeled_by: None,
                skill_fingerprint: if skill_fingerprint.is_empty() {
                    None
                } else {
                    Some(skill_fingerprint)
                },
                duration_secs,
                delegation,
                route_basis,
                route_confidence,
                route_rationale,
                lines_added,
                lines_removed,
                tokens_input,
                tokens_output,
                mode: mode.map(|m| m.as_str().to_string()),
            };
            store::append(&cfg.store_path(), &ep).context("appending episode")?;
            if pass && !done_criteria.is_empty() {
                let pb = store::Playbook {
                    ts: ep.ts,
                    title,
                    touched_files: touched,
                    class,
                    done_criteria,
                    notes,
                };
                store::append_playbook(&cfg.playbook_path(), &pb).context("appending playbook")?;
                eprintln!(
                    "recorded: {} \"{}\" pass={} (playbook saved)",
                    ep.model, ep.title, pass
                );
            } else {
                eprintln!("recorded: {} \"{}\" pass={}", ep.model, ep.title, pass);
            }
            Ok(())
        }
        Command::AuditRecent { class, within } => {
            let eps = store::load(&cfg.store_path());
            let now = store::now_secs();
            let found = store::recorded_within(&eps, &class, within, now);
            if found {
                println!("{{\"found\":true}}");
                Ok(())
            } else {
                println!("{{\"found\":false}}");
                std::process::exit(1);
            }
        }
        Command::Label {
            selector,
            verdict,
            by,
            latest,
        } => cmd_label(&cfg, selector, &verdict, &by, latest),
        Command::Procedures { action } => match action {
            ProceduresAction::Search { query, files, k } => {
                let playbooks = store::load_playbooks(&cfg.playbook_path());
                if playbooks.is_empty() {
                    println!("[]");
                    return Ok(());
                }
                let query_files = split_files(&files);
                let q_tok = rag::tokenize(&query);
                let q_files = rag::file_tokens(&query_files);
                let mut scored: Vec<(f64, &store::Playbook)> = playbooks
                    .iter()
                    .map(|pb| {
                        let e_tok = rag::tokenize(&pb.title);
                        let e_files = rag::file_tokens(&pb.touched_files);
                        let sim = rag::similarity(&q_tok, &q_files, &e_tok, &e_files);
                        (sim, pb)
                    })
                    .filter(|(s, _)| *s > 0.0)
                    .collect();
                scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
                scored.truncate(k);
                let results: Vec<&store::Playbook> = scored.into_iter().map(|(_, pb)| pb).collect();
                println!("{}", serde_json::to_string_pretty(&results)?);
                Ok(())
            }
        },
        Command::Lessons { action } => cmd_lessons(action),
        Command::Suggest {
            files,
            class,
            text,
            mode,
        } => {
            let title = text.join(" ");
            let f = split_files(&files);
            let eps = store::load(&cfg.store_path());
            let mut rng = seed_rng(eps.len());
            let d = route_decision(&cfg, &title, &f, &class, &eps, &mut rng);
            let resolved_mode = resolve_mode(mode, &cfg)?;
            let d = mode::apply(d, resolved_mode, &class, &title);
            println!(
                "worker={} verifier={} ({}, {} confidence)\n  {}",
                d.worker_model, d.verifier_model, d.basis, d.confidence, d.rationale
            );
            Ok(())
        }
        Command::Confidence { files, class, text } => {
            let title = text.join(" ");
            let f = split_files(&files);
            let eps = store::load(&cfg.store_path());
            let score = confidence::calibrated_confidence(
                &title,
                &f,
                &class,
                &eps,
                cfg.k,
                cfg.sim_threshold,
                cfg.min_samples,
            );
            println!("{score:.4}");
            Ok(())
        }
        Command::Stats { json } => cmd_stats(&cfg, json),
        Command::DelegationStats { json } => cmd_delegation_stats(&cfg, json),
        Command::DurationOutliers { json, threshold } => {
            cmd_duration_outliers(&cfg, json, threshold)
        }
        Command::Init { target } => config::init_config(&target),
        Command::Install { dry_run } => install::install(dry_run),
        Command::Uninstall { dry_run } => install::uninstall(dry_run),
        Command::Import {
            episodes,
            playbooks,
            dry_run,
            dedup,
        } => cmd_import(&cfg, episodes, playbooks, dry_run, dedup),
        Command::Sync {
            pull_only,
            push_only,
        } => cmd_sync(&cfg, pull_only, push_only),
        Command::Fingerprint { dir } => {
            let root = dir.unwrap_or_else(|| PathBuf::from("."));
            let fp = fingerprint::skill_fingerprint(&root).with_context(|| {
                format!("fingerprinting SKILL.md corpus under {}", root.display())
            })?;
            println!("{fp}");
            Ok(())
        }
        Command::CodeIndex { action } => match action {
            CodeIndexAction::Build { root, if_stale } => cmd_code_index_build(root, if_stale),
            CodeIndexAction::Search { query, root, k } => cmd_code_index_search(query, root, k),
        },
        Command::Prompt => unreachable!("handled in main"),
    }
}

/// Path convention for the per-repo code index: `<root>/.fugu/code-index.jsonl`.
/// Deliberately **per-repo** (not the machine-global `~/.fugu-router/...`
/// episode-store convention) because a code index's symbols/file-paths are
/// only meaningful relative to the repo they were extracted from — unlike
/// routing episodes or lessons, which are cross-project by design. `.fugu/`
/// sits alongside the repo (like `.git/`), namespaced under the plugin so it
/// doesn't collide with other tools' dotfiles.
fn code_index_path(root: &Path) -> PathBuf {
    root.join(".fugu").join("code-index.jsonl")
}

/// Sidecar meta path next to the index (code-RAG slice-3). Holds the
/// staleness fingerprint so `build --if-stale` can skip an unchanged rebuild
/// without touching the pure Symbol-per-line `.jsonl` body.
fn code_index_meta_path(root: &Path) -> PathBuf {
    root.join(".fugu").join("code-index.meta.json")
}

/// Enumerate git-tracked `.rs` files under `root` as repo-relative paths (via
/// `git ls-files`, so untracked/ignored files are skipped deterministically
/// the same way the rest of the repo treats "tracked"). Shared by the rebuild
/// and the `--if-stale` fingerprint paths so both see the identical file set.
fn code_index_rs_files(root: &Path) -> Result<Vec<String>> {
    let out = std::process::Command::new("git")
        .args(["-C", &root.to_string_lossy(), "ls-files"])
        .output()
        .with_context(|| format!("running git ls-files under {}", root.display()))?;
    anyhow::ensure!(
        out.status.success(),
        "git ls-files failed under {}: {}",
        root.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    let listing = String::from_utf8_lossy(&out.stdout);
    Ok(listing
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && l.ends_with(".rs"))
        .map(|l| l.to_string())
        .collect())
}

/// Compute the cheap, deterministic staleness fingerprint over the current
/// `.rs` set: `stat` each file for `(size, mtime-nanos)` and fold through
/// `harness_core::code_index::fingerprint`. File *contents* are never read.
/// An unreadable file contributes a `(path, 0, 0)` entry, so it still counts
/// toward set membership (add/remove/rename shifts the fingerprint).
fn code_index_fingerprint(root: &Path, rel_files: &[String]) -> String {
    use std::time::UNIX_EPOCH;
    let entries: Vec<(String, u64, i64)> = rel_files
        .iter()
        .map(|rel| {
            let (size, mtime) = match std::fs::metadata(root.join(rel)) {
                Ok(md) => {
                    let mtime = md
                        .modified()
                        .ok()
                        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                        .map(|d| d.as_nanos() as i64)
                        .unwrap_or(0);
                    (md.len(), mtime)
                }
                Err(_) => (0, 0),
            };
            (rel.clone(), size, mtime)
        })
        .collect();
    harness_core::code_index::fingerprint(&entries)
}

/// `code-index build [--root] [--if-stale]`: extract symbols from every
/// git-tracked `.rs` file and rebuild the index wholesale.
///
/// With `--if-stale`, first compute the cheap fingerprint of the current `.rs`
/// set; if it matches the sidecar meta from the last build **and** the index
/// file still exists, skip the rebuild and report `rebuilt:false`. Otherwise
/// (or without the flag) rebuild the index and refresh the sidecar meta,
/// reporting `rebuilt:true`. Plain `build` (no flag) always rebuilds —
/// back-compat.
fn cmd_code_index_build(root: Option<PathBuf>, if_stale: bool) -> Result<()> {
    use harness_core::code_index::{
        extract_symbols, read_meta, write_index, write_meta, IndexMeta,
    };

    let root = root.unwrap_or_else(|| PathBuf::from("."));
    let rel_files = code_index_rs_files(&root)?;
    let fingerprint = code_index_fingerprint(&root, &rel_files);
    let index_path = code_index_path(&root);
    let meta_path = code_index_meta_path(&root);

    // --if-stale fast path: unchanged source set → no-op (index untouched).
    if if_stale {
        if let Some(meta) = read_meta(&meta_path) {
            if meta.fingerprint == fingerprint && index_path.exists() {
                println!(
                    "{}",
                    json!({
                        "rebuilt": false,
                        "files_scanned": meta.files,
                        "symbols_indexed": meta.symbols,
                        "index_path": index_path.to_string_lossy(),
                    })
                );
                return Ok(());
            }
        }
    }

    let mut symbols = Vec::new();
    let mut files_scanned = 0usize;
    for rel in &rel_files {
        let Ok(contents) = std::fs::read_to_string(root.join(rel)) else {
            // Fail-soft: an unreadable tracked file (deleted-but-staged,
            // binary misdetection, permissions) is skipped, not fatal.
            continue;
        };
        symbols.extend(extract_symbols(&contents, rel));
        files_scanned += 1;
    }

    write_index(&index_path, &symbols);
    write_meta(
        &meta_path,
        &IndexMeta {
            fingerprint,
            files: files_scanned,
            symbols: symbols.len(),
        },
    );

    println!(
        "{}",
        json!({
            "rebuilt": true,
            "files_scanned": files_scanned,
            "symbols_indexed": symbols.len(),
            "index_path": index_path.to_string_lossy(),
        })
    );
    Ok(())
}

/// `code-index search --query <q> [--root] [--k]`: load the index for `root`
/// and run the deterministic lexical search. Fail-soft: a missing/empty index
/// yields `[]` and exit 0, never an error.
fn cmd_code_index_search(query: String, root: Option<PathBuf>, k: usize) -> Result<()> {
    use harness_core::code_index::{load_index, search};

    let root = root.unwrap_or_else(|| PathBuf::from("."));
    let symbols = load_index(&code_index_path(&root));
    let hits = search(&symbols, &query, k);
    let arr: Vec<serde_json::Value> = hits
        .iter()
        .map(|h| {
            let mut v = serde_json::to_value(&h.symbol).unwrap_or_else(|_| json!({}));
            if let Some(obj) = v.as_object_mut() {
                obj.insert("score".into(), json!(h.score));
            }
            v
        })
        .collect();
    println!("{}", serde_json::to_string_pretty(&arr)?);
    Ok(())
}

/// Handle `lessons add|search` over the project-INDEPENDENT lessons store.
fn cmd_lessons(action: LessonsAction) -> Result<()> {
    use harness_core::lessons::{self, Kind, Lesson};
    use sha2::{Digest, Sha256};

    match action {
        LessonsAction::Add {
            kind,
            task_summary,
            lesson_text,
            source_run,
        } => {
            let kind = match kind.as_str() {
                "error-pattern" => Kind::ErrorPattern,
                "convention" => Kind::Convention,
                other => {
                    anyhow::bail!("unknown --kind {other:?} (expected error-pattern|convention)")
                }
            };
            // Content-derived id so re-adding identical content is a true no-op
            // (append is idempotent by id).
            let mut hasher = Sha256::new();
            hasher.update(kind_str(kind).as_bytes());
            hasher.update([0]);
            hasher.update(task_summary.as_bytes());
            hasher.update([0]);
            hasher.update(lesson_text.as_bytes());
            let id = format!("{:x}", hasher.finalize())[..16].to_string();
            let lesson = Lesson {
                id: id.clone(),
                kind,
                task_summary,
                lesson_text,
                source_run,
                ts: store::now_secs(),
            };
            lessons::append(&lesson);
            eprintln!("lesson stored: {id}");
            println!("{id}");
            Ok(())
        }
        LessonsAction::Search { query, k } => {
            // Fail-soft: an uninitialized/empty store, an empty query, or no
            // overlap all yield an empty JSON array (exit 0, never an error).
            let lessons = lessons::load();
            let hits = lessons::search(&query, &lessons, k);
            let arr: Vec<serde_json::Value> = hits
                .iter()
                .map(|m| {
                    let mut v = serde_json::to_value(&m.lesson).unwrap_or_else(|_| json!({}));
                    if let Some(obj) = v.as_object_mut() {
                        obj.insert("score".into(), json!(m.score));
                    }
                    v
                })
                .collect();
            println!("{}", serde_json::to_string_pretty(&arr)?);
            Ok(())
        }
    }
}

/// Kebab-case string for a lesson kind (matches its serde representation), used
/// to seed the content-derived id.
fn kind_str(kind: harness_core::lessons::Kind) -> &'static str {
    use harness_core::lessons::Kind;
    match kind {
        Kind::ErrorPattern => "error-pattern",
        Kind::Convention => "convention",
    }
}

fn cmd_route(
    cfg: &config::Config,
    file: Option<PathBuf>,
    report: Option<PathBuf>,
    mode: mode::Mode,
) -> Result<()> {
    let raw = match file {
        Some(p) => {
            std::fs::read_to_string(&p).with_context(|| format!("reading {}", p.display()))?
        }
        None => read_stdin(),
    };
    let mut dec: decomp::Decomposition =
        serde_json::from_str(&raw).context("parsing decomposition JSON")?;
    let eps = store::load(&cfg.store_path());
    let mut rng = seed_rng(eps.len());

    // Budget-aware downgrade (soft dep): if budgetguard reports the day's spend
    // has reached the warn threshold, bias every routed task cheaper. Checked
    // once per route — the day total doesn't move between tasks of one call.
    let pressured = budget::under_pressure();
    if pressured {
        eprintln!("fugu-router: daily budget pressure — downgrading model choices one tier");
    }

    let mut report_map = serde_json::Map::new();
    for t in &mut dec.tasks {
        let d = route_decision(cfg, &t.title, &t.touched_files, &t.class, &eps, &mut rng);
        // Mode clamp FIRST, budget downgrade LAST: budget is a hard resource
        // limit and mode is a preference, so budget must be able to negate a
        // `high` pick (mode::apply's rationale note survives into
        // downgrade_for_budget's appended note, so the negation stays
        // legible rather than the high tier silently disappearing).
        let d = mode::apply(d, mode, &t.class, &t.title);
        let d = if pressured {
            policy::downgrade_for_budget(d)
        } else {
            d
        };
        // gated tasks keep whatever the interpreter chose; everything else is set.
        if d.basis != "gated" {
            t.suggested_model = d.worker_model.clone();
        }
        eprintln!(
            "  {:<16} worker={:<6} verifier={:<6} [{} {}] {}",
            t.id, t.suggested_model, d.verifier_model, d.basis, d.confidence, d.rationale
        );
        report_map.insert(
            t.id.clone(),
            json!({
                "worker_model": t.suggested_model,
                "verifier_model": d.verifier_model,
                "basis": d.basis,
                "confidence": d.confidence,
                "neighbors": d.neighbors,
                "rationale": d.rationale,
            }),
        );
    }

    println!("{}", serde_json::to_string_pretty(&dec)?);
    if let Some(rp) = report {
        std::fs::write(&rp, serde_json::to_string_pretty(&report_map)?)
            .with_context(|| format!("writing report {}", rp.display()))?;
        eprintln!("wrote routing report -> {}", rp.display());
    }
    Ok(())
}

fn cmd_import(
    cfg: &config::Config,
    episodes_src: Option<PathBuf>,
    playbooks_src: Option<PathBuf>,
    dry_run: bool,
    dedup: bool,
) -> Result<()> {
    if dedup {
        // Dedup LOCAL stores in place; source paths must not be given.
        if episodes_src.is_some() || playbooks_src.is_some() {
            anyhow::bail!("--dedup cannot be combined with --episodes or --playbooks source paths");
        }
        let ep_path = cfg.store_path();
        if ep_path.exists() {
            let s = store::dedup_episodes(&ep_path).context("deduplicating episodes")?;
            println!(
                "episodes: {} read, {} unique kept, {} duplicates removed",
                s.read, s.new, s.skipped
            );
        } else {
            println!("episodes: store not found, nothing to dedup");
        }
        let pb_path = cfg.playbook_path();
        if pb_path.exists() {
            let s = store::dedup_playbooks(&pb_path).context("deduplicating playbooks")?;
            println!(
                "playbooks: {} read, {} unique kept, {} duplicates removed",
                s.read, s.new, s.skipped
            );
        } else {
            println!("playbooks: store not found, nothing to dedup");
        }
        return Ok(());
    }

    if episodes_src.is_none() && playbooks_src.is_none() {
        anyhow::bail!(
            "at least one of --episodes or --playbooks must be specified (or use --dedup)"
        );
    }

    if dry_run {
        eprintln!("dry-run mode: no files will be written");
    }

    if let Some(src) = episodes_src {
        let dst = cfg.store_path();
        let s = store::import_episodes(&src, &dst, dry_run)
            .with_context(|| format!("importing episodes from {}", src.display()))?;
        println!(
            "episodes: {} read, {} new appended, {} duplicates skipped",
            s.read, s.new, s.skipped
        );
    }

    if let Some(src) = playbooks_src {
        let dst = cfg.playbook_path();
        let s = store::import_playbooks(&src, &dst, dry_run)
            .with_context(|| format!("importing playbooks from {}", src.display()))?;
        println!(
            "playbooks: {} read, {} new appended, {} duplicates skipped",
            s.read, s.new, s.skipped
        );
    }

    Ok(())
}

/// Timeout for the slow, network-bound git operations (pull/clone/push).
const GIT_NETWORK_TIMEOUT: Duration = Duration::from_secs(30);
/// Timeout for the fast, local-only git operations (status/add/diff/commit).
const GIT_LOCAL_TIMEOUT: Duration = Duration::from_secs(10);

/// Run `git <args>` bounded by `timeout`: kills (and reaps) the child on
/// expiry rather than letting a hung/network-stalled git subprocess wedge
/// `cmd_sync` indefinitely. Fail-soft in the sense that a timeout is reported
/// as a normal `Err` (not a panic) the same way a non-zero exit would be.
fn run_git_with_timeout(args: &[&str], timeout: Duration) -> Result<std::process::Output> {
    let mut cmd = std::process::Command::new("git");
    cmd.args(args);
    spawn_and_wait_timeout(cmd, timeout).with_context(|| format!("running git {args:?}"))
}

/// Spawn `cmd` and wait at most `timeout` for it to finish, killing (and
/// reaping) it on expiry. Generic over the command so the timeout/kill path
/// is testable without a real `git` binary.
fn spawn_and_wait_timeout(
    mut cmd: std::process::Command,
    timeout: Duration,
) -> Result<std::process::Output> {
    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("spawning subprocess")?;
    match child.wait_timeout(timeout) {
        Ok(Some(_status)) => {}
        Ok(None) => {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("subprocess timed out after {timeout:?} and was killed");
        }
        Err(e) => anyhow::bail!("waiting on subprocess: {e}"),
    }
    child
        .wait_with_output()
        .context("collecting subprocess output")
}

fn cmd_sync(cfg: &config::Config, pull_only: bool, push_only: bool) -> Result<()> {
    let repo_url = cfg.sync_repo.as_deref().ok_or_else(|| {
        anyhow::anyhow!("sync_repo is not configured in ~/.fugu-router/config.toml")
    })?;
    let sync_dir = cfg.sync_dir_path();
    let sync_dir_str = sync_dir.to_string_lossy().into_owned();

    // --- pull phase ---
    if !push_only {
        if sync_dir.join(".git").exists() {
            eprintln!("pulling from remote…");
            let out = run_git_with_timeout(
                &["-C", &sync_dir_str, "pull", "--ff-only"],
                GIT_NETWORK_TIMEOUT,
            )?;
            anyhow::ensure!(out.status.success(), "git pull failed");
        } else {
            eprintln!("cloning {} → {}…", repo_url, sync_dir.display());
            if let Some(parent) = sync_dir.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let out =
                run_git_with_timeout(&["clone", repo_url, &sync_dir_str], GIT_NETWORK_TIMEOUT)?;
            anyhow::ensure!(out.status.success(), "git clone failed");
        }
        eprintln!("pull done.");
    }

    if pull_only {
        return Ok(());
    }

    // --- push phase: commit any new records ---
    // The store files are already inside sync_dir (store_path() points there when
    // sync_repo is set), so we just need to add & commit anything that changed.
    let status_out = run_git_with_timeout(
        &["-C", &sync_dir_str, "status", "--porcelain"],
        GIT_LOCAL_TIMEOUT,
    )?;
    let dirty = !status_out.stdout.is_empty();

    if !dirty {
        eprintln!("nothing to push (store unchanged).");
        return Ok(());
    }

    let ts = store::now_secs();
    let commit_msg = format!("fugu-router sync {ts}");

    let add = run_git_with_timeout(&["-C", &sync_dir_str, "add", "-u"], GIT_LOCAL_TIMEOUT)?;
    anyhow::ensure!(add.status.success(), "git add failed");

    // Check if anything is actually staged before committing.
    let staged = run_git_with_timeout(
        &["-C", &sync_dir_str, "diff", "--cached", "--quiet"],
        GIT_LOCAL_TIMEOUT,
    )?;
    if staged.status.success() {
        eprintln!("nothing to push (no staged changes after add).");
        return Ok(());
    }

    let commit = run_git_with_timeout(
        &[
            "-C",
            &sync_dir_str,
            "commit",
            "--no-verify",
            "-m",
            &commit_msg,
        ],
        GIT_LOCAL_TIMEOUT,
    )?;
    if !commit.status.success() {
        let stderr = String::from_utf8_lossy(&commit.stderr);
        let stdout = String::from_utf8_lossy(&commit.stdout);
        anyhow::bail!("git commit failed:\nstdout: {stdout}\nstderr: {stderr}");
    }

    let push = run_git_with_timeout(&["-C", &sync_dir_str, "push"], GIT_NETWORK_TIMEOUT)?;
    if !push.status.success() {
        let stderr = String::from_utf8_lossy(&push.stderr);
        anyhow::bail!("git push failed:\nstderr: {stderr}");
    }

    eprintln!("pushed: {commit_msg}");
    Ok(())
}

fn cmd_stats(cfg: &config::Config, as_json: bool) -> Result<()> {
    use std::collections::BTreeMap;
    let eps = store::load(&cfg.store_path());
    let mut agg: BTreeMap<String, (usize, usize, f64)> = BTreeMap::new();
    for e in &eps {
        let s = agg.entry(e.model.clone()).or_insert((0, 0, 0.0));
        s.0 += 1;
        if e.pass {
            s.1 += 1;
        }
        s.2 += e.cost_usd;
    }
    // Duration coverage: how many episodes carry a real (Some) measurement out
    // of the total — surfaced so the low recording rate is visible.
    let measured_n = eps.iter().filter(|e| e.duration_secs.is_some()).count();
    let total_n = eps.len();
    if as_json {
        let obj: serde_json::Map<String, serde_json::Value> = agg
            .iter()
            .map(|(m, (n, p, c))| {
                (
                    m.clone(),
                    json!({
                        "count": n,
                        "passes": p,
                        "pass_rate": if *n > 0 { *p as f64 / *n as f64 } else { 0.0 },
                        "avg_cost_usd": if *n > 0 { c / *n as f64 } else { 0.0 },
                    }),
                )
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "episodes": eps.len(),
                "duration_coverage": {"recorded": measured_n, "total": total_n},
                "models": obj,
            }))?
        );
    } else {
        println!("episodes: {}", eps.len());
        println!("duration coverage: {measured_n}/{total_n} episodes measured");
        for (m, (n, p, c)) in &agg {
            println!(
                "  {m:<6} {p}/{n} pass ({:.0}%)  avg ${:.4}",
                if *n > 0 {
                    *p as f64 / *n as f64 * 100.0
                } else {
                    0.0
                },
                if *n > 0 { c / *n as f64 } else { 0.0 }
            );
        }
    }
    Ok(())
}

/// Canonicalize a free-text `--delegation` value (`crates/fugu-router/src/
/// store.rs`'s `Episode::delegation` is intentionally unvalidated, same
/// looseness as `class`). Case/whitespace variants of "fork"/"inline" map to
/// their canonical form; anything else (including full-width lookalikes we
/// don't special-case) falls into `"other"` rather than being silently
/// dropped, so mis-typed values are still visible in the report.
fn normalize_delegation(raw: &str) -> String {
    match raw.trim().to_lowercase().as_str() {
        "fork" => "fork".to_string(),
        "inline" => "inline".to_string(),
        _ => "other".to_string(),
    }
}

/// One delegation bucket's aggregate stats. `duration_n` (episodes with a
/// recorded `duration_secs > 0.0`) can be smaller than `count` — many
/// episodes predate the duration feature entirely, and averaging their
/// untracked `0.0` in with real measurements would silently pull the mean
/// toward zero. `avg_duration_secs` is only ever computed over `duration_n`.
struct DelegationBucket {
    count: usize,
    passes: usize,
    cost_total: f64,
    duration_n: usize,
    duration_total: f64,
}

fn cmd_delegation_stats(cfg: &config::Config, as_json: bool) -> Result<()> {
    use std::collections::BTreeMap;
    let eps = store::load(&cfg.store_path());
    let mut agg: BTreeMap<String, DelegationBucket> = BTreeMap::new();
    for e in eps.iter().filter(|e| e.class == "flow-delegation") {
        let Some(raw) = &e.delegation else {
            continue; // unrecorded delegation is meaningless for this comparison
        };
        let bucket = agg
            .entry(normalize_delegation(raw))
            .or_insert(DelegationBucket {
                count: 0,
                passes: 0,
                cost_total: 0.0,
                duration_n: 0,
                duration_total: 0.0,
            });
        bucket.count += 1;
        if e.pass {
            bucket.passes += 1;
        }
        bucket.cost_total += e.cost_usd;
        if let Some(d) = e.duration_secs {
            bucket.duration_n += 1;
            bucket.duration_total += d;
        }
    }

    let total: usize = agg.values().map(|b| b.count).sum();

    if as_json {
        let obj: serde_json::Map<String, serde_json::Value> = agg
            .iter()
            .map(|(k, b)| {
                (
                    k.clone(),
                    json!({
                        "count": b.count,
                        "passes": b.passes,
                        "pass_rate": if b.count > 0 { b.passes as f64 / b.count as f64 } else { 0.0 },
                        "avg_cost_usd": if b.count > 0 { b.cost_total / b.count as f64 } else { 0.0 },
                        "avg_duration_secs": if b.duration_n > 0 { b.duration_total / b.duration_n as f64 } else { 0.0 },
                        "duration_samples": b.duration_n,
                    }),
                )
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(
                &json!({ "flow_delegation_episodes": total, "delegation": obj })
            )?
        );
    } else if total == 0 {
        println!("no flow-delegation episodes recorded yet (fugu-router record --class flow-delegation --delegation fork|inline ...)");
    } else {
        println!("flow-delegation episodes: {total}");
        for (k, b) in &agg {
            let avg_dur = if b.duration_n > 0 {
                format!(
                    "{:.1}s (n={})",
                    b.duration_total / b.duration_n as f64,
                    b.duration_n
                )
            } else {
                "no duration data".to_string()
            };
            println!(
                "  {k:<6} {}/{} pass ({:.0}%)  avg ${:.4}  avg duration {avg_dur}",
                b.passes,
                b.count,
                if b.count > 0 {
                    b.passes as f64 / b.count as f64 * 100.0
                } else {
                    0.0
                },
                if b.count > 0 {
                    b.cost_total / b.count as f64
                } else {
                    0.0
                }
            );
        }
    }
    Ok(())
}

/// Per-(class, model) duration aggregate, plus effective-pass counts split
/// by whether this (class, model) pair is flagged as a duration outlier.
struct ClassModelDuration {
    duration_n: usize,
    duration_total: f64,
}

fn cmd_duration_outliers(cfg: &config::Config, as_json: bool, threshold: f64) -> Result<()> {
    use std::collections::BTreeMap;
    let eps = store::load(&cfg.store_path());

    // Duration coverage: how many episodes carry a real measurement out of the
    // total. Surfaced so the (historically low) recording rate is visible
    // rather than hidden behind averages computed over the measured subset.
    let measured_n = eps.iter().filter(|e| e.duration_secs.is_some()).count();
    let total_n = eps.len();

    // class -> model -> duration aggregate. Only MEASURED episodes participate
    // (Episode.duration_secs = Some); unmeasured episodes are excluded entirely
    // rather than folded in as 0.0, which would pull averages toward zero.
    let mut by_class_model: BTreeMap<String, BTreeMap<String, ClassModelDuration>> =
        BTreeMap::new();
    for e in &eps {
        let Some(d) = e.duration_secs else { continue };
        let agg = by_class_model
            .entry(e.class.clone())
            .or_default()
            .entry(e.model.clone())
            .or_insert(ClassModelDuration {
                duration_n: 0,
                duration_total: 0.0,
            });
        agg.duration_n += 1;
        agg.duration_total += d;
    }

    // For each class with >= 2 models having duration data, compute the
    // cross-model mean (mean of per-model averages, not episode-count-weighted
    // so one chatty model can't drag the baseline toward itself) and flag any
    // model whose own avg exceeds threshold * that mean.
    let mut outlier_pairs: std::collections::HashSet<(String, String)> =
        std::collections::HashSet::new();
    let mut class_reports: Vec<serde_json::Value> = Vec::new();
    for (class, models) in &by_class_model {
        if models.len() < 2 {
            continue; // need at least 2 models in the class to call anything an outlier
        }
        let per_model_avg: Vec<(String, f64, usize)> = models
            .iter()
            .filter(|(_, agg)| agg.duration_n > 0)
            .map(|(m, agg)| {
                (
                    m.clone(),
                    agg.duration_total / agg.duration_n as f64,
                    agg.duration_n,
                )
            })
            .collect();
        if per_model_avg.len() < 2 {
            continue;
        }
        let cross_model_mean: f64 =
            per_model_avg.iter().map(|(_, avg, _)| avg).sum::<f64>() / per_model_avg.len() as f64;
        let mut model_entries = Vec::new();
        for (model, avg, n) in &per_model_avg {
            let is_outlier = *avg > threshold * cross_model_mean;
            if is_outlier {
                outlier_pairs.insert((class.clone(), model.clone()));
            }
            model_entries.push(json!({
                "model": model,
                "avg_duration_secs": avg,
                "duration_samples": n,
                "outlier": is_outlier,
            }));
        }
        class_reports.push(json!({
            "class": class,
            "cross_model_mean_secs": cross_model_mean,
            "models": model_entries,
        }));
    }

    // Aggregate outlier-vs-non-outlier effective pass rate across all
    // duration-bearing episodes, using effective_pass() (human_label overrides
    // the verifier's self-pass) as the ground truth per Episode's own doc.
    let (mut outlier_n, mut outlier_pass, mut normal_n, mut normal_pass) =
        (0usize, 0usize, 0usize, 0usize);
    for e in eps.iter().filter(|e| e.duration_secs.is_some()) {
        if outlier_pairs.contains(&(e.class.clone(), e.model.clone())) {
            outlier_n += 1;
            if e.effective_pass() {
                outlier_pass += 1;
            }
        } else {
            normal_n += 1;
            if e.effective_pass() {
                normal_pass += 1;
            }
        }
    }
    let outlier_rate = if outlier_n > 0 {
        outlier_pass as f64 / outlier_n as f64
    } else {
        0.0
    };
    let normal_rate = if normal_n > 0 {
        normal_pass as f64 / normal_n as f64
    } else {
        0.0
    };

    if as_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "threshold": threshold,
                "duration_coverage": {"recorded": measured_n, "total": total_n},
                "classes": class_reports,
                "outlier_vs_normal": {
                    "outlier": {"n": outlier_n, "passes": outlier_pass, "pass_rate": outlier_rate},
                    "normal": {"n": normal_n, "passes": normal_pass, "pass_rate": normal_rate},
                },
            }))?
        );
    } else {
        println!("duration coverage: {measured_n}/{total_n} episodes measured");
        if class_reports.is_empty() {
            println!(
                "no class has >= 2 models with recorded duration_secs data yet — nothing to compare"
            );
            return Ok(());
        }
        println!("duration outliers (threshold = {threshold:.2}x cross-model mean):");
        for c in &class_reports {
            println!(
                "  class {:<20} cross-model mean {:.1}s",
                c["class"].as_str().unwrap_or("?"),
                c["cross_model_mean_secs"].as_f64().unwrap_or(0.0)
            );
            for m in c["models"].as_array().unwrap() {
                let flag = if m["outlier"].as_bool().unwrap_or(false) {
                    " <-- OUTLIER"
                } else {
                    ""
                };
                println!(
                    "    {:<10} avg {:.1}s (n={}){flag}",
                    m["model"].as_str().unwrap_or("?"),
                    m["avg_duration_secs"].as_f64().unwrap_or(0.0),
                    m["duration_samples"].as_u64().unwrap_or(0)
                );
            }
        }
        println!(
            "\noutlier episodes:  {outlier_pass}/{outlier_n} effective-pass ({:.0}%)",
            outlier_rate * 100.0
        );
        println!(
            "normal episodes:   {normal_pass}/{normal_n} effective-pass ({:.0}%)",
            normal_rate * 100.0
        );
    }
    Ok(())
}

#[cfg(test)]
mod delegation_stats_tests {
    use super::*;

    fn ep(
        class: &str,
        delegation: Option<&str>,
        pass: bool,
        cost: f64,
        duration: f64,
    ) -> store::Episode {
        store::Episode {
            ts: 0,
            title: "t".to_string(),
            touched_files: vec![],
            class: class.to_string(),
            model: "sonnet".to_string(),
            role: "worker".to_string(),
            pass,
            cost_usd: cost,
            human_label: None,
            labeled_by: None,
            skill_fingerprint: None,
            duration_secs: (duration > 0.0).then_some(duration),
            delegation: delegation.map(|s| s.to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn normalize_handles_case_and_whitespace_and_unknowns() {
        assert_eq!(normalize_delegation("fork"), "fork");
        assert_eq!(normalize_delegation("Fork"), "fork");
        assert_eq!(normalize_delegation("  INLINE  "), "inline");
        assert_eq!(normalize_delegation("async"), "other");
        assert_eq!(normalize_delegation(""), "other");
    }

    #[test]
    fn aggregation_separates_fork_and_inline_and_ignores_other_classes() {
        let eps = [
            ep("flow-delegation", Some("fork"), true, 1.0, 10.0),
            ep("flow-delegation", Some("Fork"), true, 2.0, 20.0),
            ep("flow-delegation", Some("inline"), false, 0.5, 5.0),
            ep("worker", Some("fork"), true, 100.0, 999.0), // wrong class → excluded
            ep("flow-delegation", None, true, 1.0, 1.0),    // no delegation → excluded
        ];
        let mut agg: std::collections::BTreeMap<String, DelegationBucket> =
            std::collections::BTreeMap::new();
        for e in eps.iter().filter(|e| e.class == "flow-delegation") {
            let Some(raw) = &e.delegation else { continue };
            let b = agg
                .entry(normalize_delegation(raw))
                .or_insert(DelegationBucket {
                    count: 0,
                    passes: 0,
                    cost_total: 0.0,
                    duration_n: 0,
                    duration_total: 0.0,
                });
            b.count += 1;
            if e.pass {
                b.passes += 1;
            }
            b.cost_total += e.cost_usd;
            if let Some(d) = e.duration_secs {
                b.duration_n += 1;
                b.duration_total += d;
            }
        }
        assert_eq!(agg.get("fork").unwrap().count, 2);
        assert_eq!(agg.get("inline").unwrap().count, 1);
        assert!(!agg.contains_key("other"));
    }

    #[test]
    fn duration_average_excludes_untracked_zero_duration_episodes() {
        let eps = [
            ep("flow-delegation", Some("fork"), true, 1.0, 0.0), // predates duration feature
            ep("flow-delegation", Some("fork"), true, 1.0, 100.0),
        ];
        let mut duration_n = 0usize;
        let mut duration_total = 0.0f64;
        for e in &eps {
            if let Some(d) = e.duration_secs {
                duration_n += 1;
                duration_total += d;
            }
        }
        assert_eq!(duration_n, 1);
        assert_eq!(duration_total, 100.0);
    }

    #[test]
    fn zero_flow_delegation_episodes_yields_no_data_not_a_panic() {
        let eps = [ep("worker", Some("fork"), true, 1.0, 1.0)];
        let count = eps.iter().filter(|e| e.class == "flow-delegation").count();
        assert_eq!(count, 0);
    }
}

#[cfg(test)]
mod duration_outliers_tests {
    use super::*;

    fn ep(
        class: &str,
        model: &str,
        duration: f64,
        pass: bool,
        human_label: Option<bool>,
    ) -> store::Episode {
        store::Episode {
            ts: 0,
            title: "t".to_string(),
            touched_files: vec![],
            class: class.to_string(),
            model: model.to_string(),
            role: "worker".to_string(),
            pass,
            cost_usd: 0.0,
            human_label,
            labeled_by: None,
            skill_fingerprint: None,
            duration_secs: (duration > 0.0).then_some(duration),
            delegation: None,
            ..Default::default()
        }
    }

    fn aggregate(
        eps: &[store::Episode],
    ) -> std::collections::BTreeMap<String, std::collections::BTreeMap<String, ClassModelDuration>>
    {
        use std::collections::BTreeMap;
        let mut by_class_model: BTreeMap<String, BTreeMap<String, ClassModelDuration>> =
            BTreeMap::new();
        for e in eps {
            let Some(d) = e.duration_secs else { continue };
            let agg = by_class_model
                .entry(e.class.clone())
                .or_default()
                .entry(e.model.clone())
                .or_insert(ClassModelDuration {
                    duration_n: 0,
                    duration_total: 0.0,
                });
            agg.duration_n += 1;
            agg.duration_total += d;
        }
        by_class_model
    }

    #[test]
    fn clearly_separated_durations_flag_the_slow_model_as_outlier() {
        let eps = [
            ep("c1", "fast", 10.0, true, None),
            ep("c1", "slow", 100.0, true, None),
        ];
        let by_class_model = aggregate(&eps);
        let models = &by_class_model["c1"];
        let cross_model_mean = (10.0 + 100.0) / 2.0; // 55.0, count-of-models mean not episode-weighted
        let threshold = 1.5;
        assert!(
            models["slow"].duration_total / models["slow"].duration_n as f64
                > threshold * cross_model_mean
        );
        assert!(
            models["fast"].duration_total / models["fast"].duration_n as f64
                <= threshold * cross_model_mean
        );
    }

    #[test]
    fn human_label_overrides_verifier_self_pass_in_effective_pass() {
        // verifier said pass=true, but a human corrected it to bad.
        let e = ep("c1", "sonnet", 10.0, true, Some(false));
        assert!(!e.effective_pass());
        // verifier said pass=false, but a human corrected it to good.
        let e = ep("c1", "sonnet", 10.0, false, Some(true));
        assert!(e.effective_pass());
        // no human label at all -> verifier's pass stands.
        let e = ep("c1", "sonnet", 10.0, true, None);
        assert!(e.effective_pass());
    }

    #[test]
    fn zero_duration_episodes_are_excluded_from_aggregation() {
        let eps = [
            ep("c1", "sonnet", 0.0, true, None), // predates duration feature
            ep("c1", "sonnet", 50.0, true, None),
        ];
        let by_class_model = aggregate(&eps);
        let agg = &by_class_model["c1"]["sonnet"];
        assert_eq!(agg.duration_n, 1);
        assert_eq!(agg.duration_total, 50.0);
    }

    #[test]
    fn single_model_class_has_no_outlier_comparison() {
        // only one model has data in this class -> nothing to compare against,
        // so cmd_duration_outliers's real code skips classes with < 2 models.
        let eps = [ep("c1", "sonnet", 10.0, true, None)];
        let by_class_model = aggregate(&eps);
        assert_eq!(by_class_model["c1"].len(), 1);
    }
}

#[cfg(test)]
mod sync_timeout_tests {
    use super::*;

    /// A subprocess that outlives its timeout is killed and reported as an
    /// error, not left to hang `cmd_sync` indefinitely.
    #[test]
    fn spawn_and_wait_timeout_kills_slow_subprocess() {
        let mut cmd = std::process::Command::new("sh");
        cmd.args(["-c", "sleep 5"]);
        let start = std::time::Instant::now();
        let result = spawn_and_wait_timeout(cmd, Duration::from_millis(200));
        assert!(result.is_err(), "expected a timeout error, got {result:?}");
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "spawn_and_wait_timeout must not block past the timeout"
        );
    }

    /// A subprocess that finishes well within the timeout returns its output
    /// normally (fail-soft path is not taken on the happy path).
    #[test]
    fn spawn_and_wait_timeout_returns_output_on_fast_success() {
        let mut cmd = std::process::Command::new("sh");
        cmd.args(["-c", "echo hello"]);
        let out = spawn_and_wait_timeout(cmd, Duration::from_secs(5)).expect("should not time out");
        assert!(out.status.success());
        assert_eq!(out.stdout, b"hello\n");
    }
}

#[cfg(test)]
mod label_tests {
    use super::*;
    use crate::store::Episode;

    fn ep(title: &str, ts: u64) -> Episode {
        Episode {
            ts,
            title: title.into(),
            touched_files: vec![],
            class: "parallel".into(),
            model: "sonnet".into(),
            role: "worker".into(),
            pass: true,
            cost_usd: 0.0,
            human_label: None,
            labeled_by: None,
            skill_fingerprint: None,
            duration_secs: None,
            delegation: None,
            ..Default::default()
        }
    }

    #[test]
    fn latest_picks_highest_ts() {
        let eps = vec![ep("a", 10), ep("b", 30), ep("c", 20)];
        assert_eq!(select_episode_index(&eps, None, true), Some(1));
    }

    #[test]
    fn selector_matches_title_case_insensitive_most_recent() {
        let eps = vec![
            ep("Add login", 10),
            ep("add LOGIN again", 20),
            ep("unrelated", 30),
        ];
        assert_eq!(select_episode_index(&eps, Some("login"), false), Some(1));
    }

    #[test]
    fn no_match_returns_none() {
        let eps = vec![ep("a", 1)];
        assert_eq!(select_episode_index(&eps, Some("zzz"), false), None);
    }

    #[test]
    fn tie_breaks_to_last_recorded() {
        let eps = vec![ep("x", 5), ep("x", 5)];
        assert_eq!(select_episode_index(&eps, Some("x"), false), Some(1));
    }
}
