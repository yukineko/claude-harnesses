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
mod pathutil;
mod policy;
mod rag;
mod rng;
mod semantic;
mod store;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use harness_core::hook::{read_stdin, run_hook, HookInput};
use serde_json::json;

#[derive(Parser)]
#[command(
    name = "fugu-router",
    about = "fugu-style per-model routing for Claude Code orchestration"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

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

fn run_user(cmd: Command) -> Result<()> {
    let cfg = config::Config::load();
    match cmd {
        Command::Route { file, report } => cmd_route(&cfg, file, report),
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
        } => {
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
        Command::Suggest { files, class, text } => {
            let title = text.join(" ");
            let f = split_files(&files);
            let eps = store::load(&cfg.store_path());
            let mut rng = seed_rng(eps.len());
            let d = route_decision(&cfg, &title, &f, &class, &eps, &mut rng);
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
            CodeIndexAction::Build { root } => cmd_code_index_build(root),
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

/// `code-index build [--root]`: enumerate git-tracked `.rs` files under
/// `root` (via `git ls-files`, so untracked/ignored files are skipped
/// deterministically the same way the rest of the repo treats "tracked"),
/// extract symbols from each, and rebuild the index file wholesale.
fn cmd_code_index_build(root: Option<PathBuf>) -> Result<()> {
    use harness_core::code_index::{extract_symbols, write_index};

    let root = root.unwrap_or_else(|| PathBuf::from("."));
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

    let mut symbols = Vec::new();
    let mut files_scanned = 0usize;
    for rel in listing.lines() {
        let rel = rel.trim();
        if rel.is_empty() || !rel.ends_with(".rs") {
            continue;
        }
        let path = root.join(rel);
        let Ok(contents) = std::fs::read_to_string(&path) else {
            // Fail-soft: an unreadable tracked file (deleted-but-staged,
            // binary misdetection, permissions) is skipped, not fatal.
            continue;
        };
        symbols.extend(extract_symbols(&contents, rel));
        files_scanned += 1;
    }

    let index_path = code_index_path(&root);
    write_index(&index_path, &symbols);

    println!(
        "{}",
        json!({
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

fn cmd_route(cfg: &config::Config, file: Option<PathBuf>, report: Option<PathBuf>) -> Result<()> {
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

fn cmd_sync(cfg: &config::Config, pull_only: bool, push_only: bool) -> Result<()> {
    let repo_url = cfg.sync_repo.as_deref().ok_or_else(|| {
        anyhow::anyhow!("sync_repo is not configured in ~/.fugu-router/config.toml")
    })?;
    let sync_dir = cfg.sync_dir_path();

    // --- pull phase ---
    if !push_only {
        if sync_dir.join(".git").exists() {
            eprintln!("pulling from remote…");
            let status = std::process::Command::new("git")
                .args(["-C", &sync_dir.to_string_lossy(), "pull", "--ff-only"])
                .status()
                .context("running git pull")?;
            anyhow::ensure!(status.success(), "git pull failed");
        } else {
            eprintln!("cloning {} → {}…", repo_url, sync_dir.display());
            if let Some(parent) = sync_dir.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let status = std::process::Command::new("git")
                .args(["clone", repo_url, &sync_dir.to_string_lossy()])
                .status()
                .context("running git clone")?;
            anyhow::ensure!(status.success(), "git clone failed");
        }
        eprintln!("pull done.");
    }

    if pull_only {
        return Ok(());
    }

    // --- push phase: commit any new records ---
    // The store files are already inside sync_dir (store_path() points there when
    // sync_repo is set), so we just need to add & commit anything that changed.
    let status_out = std::process::Command::new("git")
        .args(["-C", &sync_dir.to_string_lossy(), "status", "--porcelain"])
        .output()
        .context("running git status")?;
    let dirty = !status_out.stdout.is_empty();

    if !dirty {
        eprintln!("nothing to push (store unchanged).");
        return Ok(());
    }

    let ts = store::now_secs();
    let commit_msg = format!("fugu-router sync {ts}");

    let add = std::process::Command::new("git")
        .args(["-C", &sync_dir.to_string_lossy(), "add", "-u"])
        .status()
        .context("running git add")?;
    anyhow::ensure!(add.success(), "git add failed");

    // Check if anything is actually staged before committing.
    let staged = std::process::Command::new("git")
        .args([
            "-C",
            &sync_dir.to_string_lossy(),
            "diff",
            "--cached",
            "--quiet",
        ])
        .status()
        .context("running git diff --cached")?;
    if staged.success() {
        eprintln!("nothing to push (no staged changes after add).");
        return Ok(());
    }

    let commit = std::process::Command::new("git")
        .args([
            "-C",
            &sync_dir.to_string_lossy(),
            "commit",
            "--no-verify",
            "-m",
            &commit_msg,
        ])
        .output()
        .context("running git commit")?;
    if !commit.status.success() {
        let stderr = String::from_utf8_lossy(&commit.stderr);
        let stdout = String::from_utf8_lossy(&commit.stdout);
        anyhow::bail!("git commit failed:\nstdout: {stdout}\nstderr: {stderr}");
    }

    let push = std::process::Command::new("git")
        .args(["-C", &sync_dir.to_string_lossy(), "push"])
        .output()
        .context("running git push")?;
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
            serde_json::to_string_pretty(&json!({ "episodes": eps.len(), "models": obj }))?
        );
    } else {
        println!("episodes: {}", eps.len());
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
