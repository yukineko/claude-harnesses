//! compass — goal re-grounding + next-move derivation upstream of condukt.
//!
//! A thin, subscription-native orchestrator (DESIGN §2): the binary keeps state
//! and renders deterministic context; the LLM (skill) does the judging. This is
//! the **scaffold** — only `charter` (show/parse) and config loading have real
//! behavior. `nudge` / `breadcrumb` / `gap` / `route` are wired into the
//! dispatch but stubbed until later tasks (DESIGN §15-B implementation order).

mod breadcrumb;
mod c3screen;
mod carve;
mod charter;
mod config;
mod discovery;
mod freshness;
mod gap;
mod gates;
mod gather;
mod opportunity;
mod outcome;
mod route;

use std::io::Read as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};
use harness_core::hook::{read_stdin, HookInput};
use harness_core::interrogate::{self, Answer, CarveState};

use carve::CarveView;
use charter::Charter;
use config::Config;
use gates::CompassGates;
use route::Decomposition;

#[derive(Parser)]
#[command(
    name = "compass",
    version,
    about = "Goal re-grounding + next-move derivation upstream of condukt; subscription-native."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// SessionStart hook (§14): deterministic C1/C2 freshness nudge.
    Nudge(NudgeArgs),
    /// Stop hook (§14): write the next physical step into the charter.
    Breadcrumb,
    /// Carve floor (§11): print the deterministic C1/C2 open questions as JSON.
    /// The skill ADDS its own C3–C5 on top. Initializes-or-loads the CarveState.
    Evaluate,
    /// Carve step (§11): fold one human answer into the CarveState, re-check
    /// C1/C2, persist, and print the updated `{open_questions,status,round}`.
    Apply(ApplyArgs),
    /// Clear the persisted CarveState (start a fresh carve).
    CarveReset,
    /// Derive goal - current-state gap and candidate next moves (§3).
    Gap(GapArgs),
    /// Size triage (§13): route the right-sized move / park the rest.
    Route(RouteArgs),
    /// Show the parsed charter (+ resolved config), or (--write JSON) persist a
    /// skill-composed charter.
    Charter(CharterArgs),
    /// Record a completed move's judged outcome vs the charter `measuring_stick`
    /// (§7). Requires measured evidence (build is not validation). Surfaced as
    /// `last_outcome` in `compass gap`.
    Outcome(OutcomeArgs),
    /// Manage the opportunity layer (PDO OST): named bets sitting under the
    /// active outcome (charter `north_star`), the discovery-side layer between
    /// the goal and the single solution `route` hands to condukt.
    Opportunity(OpportunityArgs),
    /// Machine-scope shared discovery store (cross-session dedup): record/select/
    /// list discovered tasks so concurrent compass/scout sessions don't re-surface
    /// a task another session already found. Every subcommand is fail-soft and
    /// exits 0 (called from skills/hooks — must never break a turn).
    Discovery(DiscoveryArgs),
    /// Pivot-or-persevere signal from the trailing outcome streak (§7).
    /// Prints `{"recommendation":"persevere"|"pivot","streak":N,"threshold":N,"reason":"…"}` and exits 0.
    PivotCheck,
    /// Advisory deterministic screen for the C3 "observable DoD" gate: flag
    /// charter `definition_of_done` items whose wording is vague/non-observable.
    /// Prints `{"flagged":[{"index","item","reason"}...]}` (empty when no charter/
    /// DoD). SUPPLEMENTS the skill's C3 judgment — never blocks.
    C3Screen,
    /// Advisory deterministic DEFAULT verdict for `compass outcome`, derived from
    /// observed facts. Prints `{"suggested":"forward|unchanged|backward"}`. The
    /// skill can still override via `outcome --verdict`.
    SuggestVerdict(SuggestVerdictArgs),
}

#[derive(Args)]
struct NudgeArgs {
    /// Emit the freshness verdict as machine-readable JSON
    /// (`{"fresh": bool, "reason": string|null}`) instead of the human nudge
    /// line. Lets a downstream driver (e.g. autoflow) gate auto-driving on the
    /// same deterministic C1/C2 floor. Always exits 0.
    #[arg(long)]
    json: bool,
}

#[derive(Args)]
struct ApplyArgs {
    /// The human [`Answer`] as JSON (`{gate, reference, value, defer}`). Omit to
    /// read the JSON from stdin.
    #[arg(long, value_name = "JSON")]
    answer: Option<String>,
}

#[derive(Args)]
struct CharterArgs {
    /// Persist a full skill-composed [`Charter`] as JSON
    /// (`{north_star, definition_of_done, measuring_stick, current_gap,
    /// next_action, parked}`). Omit to show the current parsed charter.
    #[arg(long, value_name = "JSON")]
    write: Option<String>,
}

#[derive(Args)]
struct GapArgs {
    /// Persist the skill-produced gap text into the charter `current_gap`
    /// instead of printing the assembled inputs. Deterministic write-back.
    #[arg(long, value_name = "TEXT")]
    write: Option<String>,
}

#[derive(Args)]
struct RouteArgs {
    /// Read the decomposition JSON from this file. Omit to read from stdin.
    #[arg(long, value_name = "PATH")]
    file: Option<PathBuf>,
}

#[derive(Args)]
struct OutcomeArgs {
    /// The judged outcome vs the charter `measuring_stick`:
    /// `forward` (前進) / `unchanged` (不変) / `backward` (後退).
    #[arg(long, value_enum)]
    verdict: outcome::Verdict,
    /// Measured evidence for the verdict. Repeatable; REQUIRED to be non-empty
    /// after trimming (build is not validation).
    #[arg(long, value_name = "STRING")]
    evidence: Vec<String>,
}

#[derive(Args)]
struct SuggestVerdictArgs {
    /// Net change in passing tests vs the baseline (may be negative).
    #[arg(long, value_name = "N", allow_hyphen_values = true)]
    tests_delta: i64,
    /// Count of observed regressions (newly failing tests / broken invariants).
    #[arg(long, value_name = "N", default_value_t = 0)]
    regressions: u64,
    /// Whether this move closed the charter `current_gap` it was judged against.
    #[arg(long)]
    gap_closed: bool,
}

#[derive(Args)]
struct OpportunityArgs {
    #[command(subcommand)]
    cmd: OpportunityCmd,
}

#[derive(Subcommand)]
enum OpportunityCmd {
    /// Record a named opportunity under the active outcome (charter
    /// `north_star`, unless `--outcome` overrides it). Title is required and
    /// must be non-empty.
    Add {
        /// The opportunity statement (a named bet / customer need).
        #[arg(long, value_name = "TITLE")]
        title: String,
        /// Override the active outcome this opportunity sits under. Defaults to
        /// the charter `north_star`.
        #[arg(long, value_name = "REF")]
        outcome: Option<String>,
        /// Relative weight (outcome-impact or confidence) driving ordering.
        /// Defaults to a neutral weight when omitted.
        #[arg(long, value_name = "WEIGHT")]
        weight: Option<f64>,
    },
    /// List opportunities under the active outcome (charter `north_star`, unless
    /// `--outcome` overrides it). `--json` prints a JSON array (empty store/no
    /// match => `[]`).
    List {
        /// Print the opportunities as a JSON array instead of human lines.
        #[arg(long)]
        json: bool,
        /// List under this outcome instead of the charter `north_star`.
        #[arg(long, value_name = "REF")]
        outcome: Option<String>,
    },
}

#[derive(Args)]
struct DiscoveryArgs {
    #[command(subcommand)]
    cmd: DiscoveryCmd,
}

#[derive(Subcommand)]
enum DiscoveryCmd {
    /// Append a `Discovered` row for `--title` (fingerprinted) under the given
    /// session (`--session-id` else `CLAUDE_CODE_SESSION_ID` else `local`).
    /// Fail-soft: store errors are swallowed and the command still exits 0.
    Record {
        /// The discovered task title (fingerprinted deterministically).
        #[arg(long, value_name = "TITLE")]
        title: String,
        /// The discovering session id. Defaults to `CLAUDE_CODE_SESSION_ID` (or
        /// `local` when unset/blank).
        #[arg(long, value_name = "ID")]
        session_id: Option<String>,
    },
    /// Mark a discovered task `Selected`, resolving the fingerprint from
    /// `--fingerprint` else `fingerprint(--title)`. With neither, prints a usage
    /// note and exits 0 (never breaks a caller turn). Fail-soft.
    Select {
        /// The fingerprint to select (used verbatim if given).
        #[arg(long, value_name = "FP")]
        fingerprint: Option<String>,
        /// Resolve the fingerprint from this title when `--fingerprint` is absent.
        #[arg(long, value_name = "TITLE")]
        title: Option<String>,
        /// Accepted for symmetry with `record`; selection is session-agnostic.
        #[arg(long, value_name = "ID")]
        session_id: Option<String>,
    },
    /// List discovery rows (`--json` => a JSON array, `[]` when empty/absent).
    /// Fail-soft; always exits 0.
    List {
        /// Print the rows as a JSON array instead of human lines.
        #[arg(long)]
        json: bool,
    },
}

fn main() {
    let cli = Cli::parse();
    let r = match cli.command {
        Command::Nudge(args) => nudge_command(args),
        Command::Breadcrumb => breadcrumb_command(),
        Command::Evaluate => evaluate_command(),
        Command::Apply(args) => apply_command(args),
        Command::CarveReset => carve_reset_command(),
        Command::Gap(args) => gap_command(args),
        Command::Route(args) => route_command(args),
        Command::Charter(args) => charter_command(args),
        Command::Outcome(args) => outcome_command(args),
        Command::Opportunity(args) => opportunity_command(args),
        Command::Discovery(args) => discovery_command(args),
        Command::PivotCheck => pivot_check_command(),
        Command::C3Screen => c3_screen_command(),
        Command::SuggestVerdict(args) => suggest_verdict_command(args),
    };
    if let Err(e) = r {
        eprintln!("compass: {e}");
        std::process::exit(1);
    }
}

fn project_root() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| Path::new(".").to_path_buf())
}

/// The composed C1/C2 freshness verdict for the current project's charter.
///
/// `fresh` is the single bit a downstream driver needs ("is the charter sharp
/// enough to auto-act on?"); `reason` is the human-readable nudge text, present
/// iff `!fresh`. The C1 existence/blurry checks and the C2 deterministic floor
/// all fold into this one verdict so the human `nudge` and the `--json` machine
/// gate share exactly one source of truth.
struct NudgeVerdict {
    fresh: bool,
    reason: Option<String>,
}

impl NudgeVerdict {
    fn fresh() -> Self {
        NudgeVerdict {
            fresh: true,
            reason: None,
        }
    }
    fn stale(reason: impl Into<String>) -> Self {
        NudgeVerdict {
            fresh: false,
            reason: Some(reason.into()),
        }
    }
}

/// Compose the C1 (existence/blurry) + C2 (deterministic freshness floor)
/// verdict for the charter under `root`. A parse failure is treated as `fresh`
/// (tolerant by contract: compass must never wedge a session start or a
/// downstream driver because the charter momentarily failed to parse).
fn nudge_verdict(root: &Path) -> NudgeVerdict {
    let cfg = Config::load(root);
    let path = Charter::project_path(root);

    // C1 existence (deterministic): absent file => loudest nudge.
    if !path.exists() {
        return NudgeVerdict::stale(
            "no .compass/charter.md yet — run `/compass` to carve a north star.",
        );
    }

    // Parse is tolerant; a failure shouldn't break session start / wedge a driver.
    let Ok(charter) = Charter::load(&path) else {
        return NudgeVerdict::fresh();
    };

    verdict_for_charter(root, &path, &charter, &cfg)
}

/// The C1-blurry + C2-stale checks for an already-loaded charter (kept pure so
/// it is unit-testable with a constructed [`Charter`], like `freshness`).
fn verdict_for_charter(root: &Path, path: &Path, charter: &Charter, cfg: &Config) -> NudgeVerdict {
    // C1: blurry charter (empty north_star / DoD).
    if charter.north_star.trim().is_empty() || charter.definition_of_done.is_empty() {
        return NudgeVerdict::stale(
            "charter is blurry (north_star/DoD incomplete) — run `/compass`.",
        );
    }

    // C2: deterministic freshness floor.
    let fresh = freshness::check(root, path, charter, cfg);
    if fresh.stale {
        return NudgeVerdict::stale(format!(
            "charter may be stale ({}) — run `/compass` to re-ground.",
            fresh.reasons.join("; ")
        ));
    }

    NudgeVerdict::fresh()
}

/// SessionStart hook (§14): the deterministic C1/C2 freshness nudge.
///
/// Thin path: run the deterministic floor only (NO LLM, no C3–C5). By default
/// print a one-line nudge to stdout iff the charter is absent/blurry (C1) or
/// drift-suspect (C2). With `--json`, emit the structured verdict instead so a
/// downstream driver can gate on the same floor. Non-blocking by contract: this
/// always exits 0, even on error, so a re-grounding hook never breaks a session
/// start.
fn nudge_command(args: NudgeArgs) -> Result<()> {
    let verdict = nudge_verdict(&project_root());
    if args.json {
        println!(
            "{}",
            serde_json::json!({ "fresh": verdict.fresh, "reason": verdict.reason })
        );
    } else if let Some(reason) = verdict.reason {
        println!("compass: {reason}");
    }
    Ok(())
}

/// Stop hook (§14 / §5⑤): write "the next physical action" into the charter so
/// the ②"blank after a checkpoint" pain doesn't recur. Reads the Stop payload
/// from stdin, finds the assistant's last message via `transcript_path`, and
/// extracts an explicit ```` ```compass-next ```` block; if found, sets
/// `charter.next_action`. No LLM, never guesses. ALWAYS exits 0 — a breadcrumb
/// must never block a turn.
fn breadcrumb_command() -> Result<()> {
    let raw = read_stdin();
    let Some(input) = HookInput::parse(&raw) else {
        return Ok(()); // empty / invalid payload: stay silent.
    };

    // The assistant's final message lives in the JSONL transcript. Pull a
    // generous slice so a fenced block isn't truncated mid-body.
    let message = match last_assistant_message(&input.transcript_path) {
        Some(m) => m,
        None => return Ok(()), // no text to inspect: do nothing.
    };

    let Some(next_action) = breadcrumb::extract_next_action(&message) else {
        return Ok(()); // no explicit block: do NOT guess.
    };

    let root = input.cwd_or_current();
    let path = Charter::project_path(&root);
    // Best-effort write; swallow errors so the hook never breaks a turn.
    let _ = breadcrumb::write_next_action(&path, &next_action);
    Ok(())
}

/// Best-effort: the last assistant text turn from a JSONL transcript path. Each
/// line is a JSON object; an `assistant` turn carries `message.content[]` with
/// `{type:"text", text}` parts. Any read/parse error yields `None` (the hook
/// then does nothing). Returns the full joined text (untruncated) so a fenced
/// block survives intact.
fn last_assistant_message(transcript_path: &str) -> Option<String> {
    if transcript_path.is_empty() {
        return None;
    }
    let text = std::fs::read_to_string(transcript_path).ok()?;
    for line in text.lines().rev() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if v.get("type").and_then(|t| t.as_str()) != Some("assistant") {
            continue;
        }
        let Some(parts) = v
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_array())
        else {
            continue;
        };
        let joined: String = parts
            .iter()
            .filter(|p| p.get("type").and_then(|t| t.as_str()) == Some("text"))
            .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join("\n");
        if joined.trim().is_empty() {
            continue; // tool-only turn: keep scanning earlier turns.
        }
        return Some(joined);
    }
    None
}

/// Carve floor (§11): build the C1/C2 deterministic open questions, initialize-
/// or-load the persisted [`CarveState`] (max_rounds from `[carve]`), persist it,
/// and print `{open_questions, status, round}` as JSON. The skill reads this
/// floor and ADDS its own C3–C5 questions on top.
fn evaluate_command() -> Result<()> {
    let root = project_root();
    let cfg = Config::load(&root);
    let path = Charter::project_path(&root);
    let charter = Charter::load(&path).unwrap_or_default();

    let bundle = gather::gather(&root, &charter, &cfg)?;
    let gates = CompassGates {
        cfg: &cfg,
        repo_root: &root,
        charter_path: &path,
        charter: &charter,
    };

    // Initialize-or-load: a fresh evaluate re-seeds the bundle from gather (so
    // it reflects the current repo state); a resumed carve keeps its answers.
    let mut state = match carve::load(&root) {
        Some(mut s) => {
            // Refresh the freshly-gathered material but keep recorded answers
            // folded back in (they were appended as High fragments by `apply`).
            s.bundle = bundle.clone();
            for ans in &s.answers {
                if !ans.defer {
                    s.bundle.fragments.push(interrogate::Fragment {
                        text: ans.value.clone(),
                        source_path: format!("interrogate:answer:{}:{}", ans.gate, ans.reference),
                        authority: interrogate::Authority::High,
                        score: 0,
                        anchor: None,
                    });
                }
            }
            s
        }
        None => CarveState::new(bundle, cfg.carve.max_rounds),
    };

    let open = interrogate::evaluate(&gates, &state.bundle);
    // Reflect emptiness in status without consuming a round (round bumps only on
    // `apply`): a clean floor at round 0 reads as resolved for the skill.
    if open.is_empty() && state.round == 0 {
        state.status = interrogate::CarveStatus::Resolved;
    }
    carve::save(&root, &state)?;

    println!(
        "{}",
        serde_json::to_string_pretty(&CarveView::new(open, &state))?
    );
    Ok(())
}

/// Carve step (§11): deserialize one human [`Answer`] (from `--answer` or
/// stdin), load the persisted [`CarveState`], fold the answer in + re-check
/// C1/C2 via [`interrogate::apply`], persist, and print the updated view. C3–C5
/// answers from the skill are recorded here too (they become High-authority
/// fragments); only C1/C2 are re-evaluated by Rust — that's expected.
fn apply_command(args: ApplyArgs) -> Result<()> {
    let root = project_root();
    let cfg = Config::load(&root);
    let path = Charter::project_path(&root);
    let charter = Charter::load(&path).unwrap_or_default();

    let json = read_arg_or_stdin(args.answer.unwrap_or_default())?;
    let answer: Answer = serde_json::from_str(&json).context("parsing answer JSON")?;

    // Resume the carve; if none persisted, start one over the current gather.
    let mut state = match carve::load(&root) {
        Some(s) => s,
        None => {
            let bundle = gather::gather(&root, &charter, &cfg)?;
            CarveState::new(bundle, cfg.carve.max_rounds)
        }
    };

    let gates = CompassGates {
        cfg: &cfg,
        repo_root: &root,
        charter_path: &path,
        charter: &charter,
    };

    let open = interrogate::apply(&gates, &mut state, answer);
    carve::save(&root, &state)?;

    println!(
        "{}",
        serde_json::to_string_pretty(&CarveView::new(open, &state))?
    );
    Ok(())
}

/// Clear the persisted [`CarveState`] (start fresh). Small helper for the skill.
fn carve_reset_command() -> Result<()> {
    let root = project_root();
    carve::reset(&root)?;
    println!("compass: carve state cleared.");
    Ok(())
}

/// Resolve a JSON-bearing arg: a non-empty value is used verbatim; an empty
/// value means "read it from stdin" (so the skill can pipe large payloads).
fn read_arg_or_stdin(value: String) -> Result<String> {
    if !value.trim().is_empty() {
        return Ok(value);
    }
    let mut buf = String::new();
    std::io::stdin()
        .read_to_string(&mut buf)
        .context("reading JSON from stdin")?;
    Ok(buf)
}

/// Show the resolved config and the parsed charter for the cwd, OR (`--write
/// <JSON>`) persist a skill-composed charter. The write path lets the skill save
/// the sharpened charter (north_star / observable DoD / measuring_stick / gap /
/// next_action / parked) after the carve resolves. Deterministic; no LLM.
fn charter_command(args: CharterArgs) -> Result<()> {
    let root = project_root();

    if let Some(json) = args.write {
        let json = read_arg_or_stdin(json)?;
        let charter: Charter =
            serde_json::from_str(&json).context("parsing charter JSON for --write")?;
        let path = Charter::project_path(&root);
        charter.save(&path)?;
        println!("compass: wrote charter to {}", path.display());
        return Ok(());
    }

    let cfg = Config::load(&root);
    let path = Charter::project_path(&root);

    println!("config:        {}", Config::project_path(&root).display());
    println!("  stale_commits  = {}", cfg.freshness.stale_commits);
    println!("  stale_days     = {}", cfg.freshness.stale_days);
    println!("  check_dod_refs = {}", cfg.freshness.check_dod_refs);
    println!("  max_rounds     = {}", cfg.carve.max_rounds);
    println!("  right_size     = {:?}", cfg.routing.right_size);
    println!();

    if !path.exists() {
        println!("charter:       {} (absent)", path.display());
        return Ok(());
    }
    let charter = Charter::load(&path)?;
    println!("charter:       {}", path.display());
    print!("{}", charter.to_markdown());
    Ok(())
}

/// gap (§3): deterministically assemble the inputs the skill reasons over, or
/// (with `--write <text>`) persist the skill-produced gap into the charter.
/// No LLM, no semantic derivation here.
fn gap_command(args: GapArgs) -> Result<()> {
    let root = project_root();
    let path = Charter::project_path(&root);
    let mut charter = Charter::load(&path)
        .with_context(|| "loading .compass/charter.md (run `/compass` to carve one)")?;

    // Write-back path: persist the skill-produced gap text.
    if let Some(text) = args.write {
        gap::persist_gap(&path, &mut charter, &text)?;
        println!("compass: wrote current_gap to {}", path.display());
        return Ok(());
    }

    // Default path: print the assembled inputs as JSON for the skill to read.
    let cfg = Config::load(&root);
    let bundle = gather::gather(&root, &charter, &cfg)?;
    let mut inputs = gap::assemble_gap_inputs(&charter, &bundle);
    // Close the measurement loop: surface the latest judged outcome (§7).
    inputs.last_outcome = outcome::latest(&root)?;
    // OST (§3): scaffold one gap slot per opportunity under the active outcome
    // so the skill derives a gap per named bet, not one flat gap. Ranked by
    // weight descending (load-bearing — heaviest bet leads). Tolerant load — a
    // missing/corrupt store must not break `gap`.
    // Cross-session dedup (DoD#2): drop any opportunity another session already
    // discovered, so this session doesn't re-surface a duplicate. Byte-equivalent
    // fallback (DoD#4): an absent/corrupt discovery store drops nothing.
    let ranked = opportunity::list_under_ranked(&root, &charter.north_star).unwrap_or_default();
    let my_session = discovery::session_from_env();
    inputs.opportunities = discovery::filter_undiscovered_by_others(&root, &my_session, ranked)
        .into_iter()
        .map(|o| gap::OpportunityGap {
            id: o.id,
            title: o.title,
            weight: o.weight,
            gap: None,
        })
        .collect();
    println!("{}", serde_json::to_string_pretty(&inputs)?);
    Ok(())
}

/// outcome (§7): record a completed move's judged outcome against the charter
/// `measuring_stick`, requiring measured evidence, and append it to the
/// `.compass/outcomes.json` store. The latest record is surfaced by `gap`
/// (`last_outcome`). No LLM — the verdict/evidence are the human's judgment.
fn outcome_command(args: OutcomeArgs) -> Result<()> {
    let root = project_root();
    let path = Charter::project_path(&root);
    let charter = Charter::load(&path)
        .with_context(|| "loading .compass/charter.md (run `/compass` to carve one)")?;

    let recorded = outcome::record(&root, &charter, args.verdict, args.evidence)?;
    println!(
        "compass: recorded outcome #{} ({:?}) vs measuring_stick — {}",
        recorded.seq,
        recorded.verdict,
        outcome::store_path(&root).display()
    );
    Ok(())
}

/// pivot-or-persevere signal from the trailing outcome streak. Loads the
/// `.compass/outcomes.json` store, runs the deterministic `pivot_signal()`
/// aggregation, and prints the result as JSON. Always exits 0 so downstream
/// drivers (flow) can gate on the `recommendation` field without error handling.
fn pivot_check_command() -> Result<()> {
    let root = project_root();
    let outcomes = outcome::load(&root).unwrap_or_default();
    let sig = outcome::pivot_signal(&outcomes, outcome::DEFAULT_PIVOT_THRESHOLD);
    let rec = match sig.recommendation {
        outcome::Recommendation::Pivot => "pivot",
        outcome::Recommendation::Persevere => "persevere",
    };
    println!(
        "{{\"recommendation\":\"{rec}\",\"streak\":{streak},\"threshold\":{threshold},\"reason\":{reason}}}",
        rec = rec,
        streak = sig.streak,
        threshold = sig.threshold,
        reason = serde_json::to_string(&sig.reason).unwrap_or_else(|_| "\"\"".to_string()),
    );
    Ok(())
}

/// C3 lexical screen (advisory): load the current charter's `definition_of_done`
/// (tolerant — a missing/unparseable charter yields no items) and print the vague/
/// non-observable items as `{"flagged":[{"index","item","reason"}...]}`. This
/// SUPPLEMENTS the skill's C3 judgment; it is deterministic and never blocks.
fn c3_screen_command() -> Result<()> {
    let root = project_root();
    let path = Charter::project_path(&root);
    let dod = Charter::load(&path)
        .map(|c| c.definition_of_done)
        .unwrap_or_default();
    let flagged = c3screen::c3_lexical_screen(&dod);
    println!("{}", serde_json::json!({ "flagged": flagged }));
    Ok(())
}

/// suggest-verdict (advisory): derive the deterministic DEFAULT verdict from the
/// supplied observed facts and print `{"suggested":"forward|unchanged|backward"}`.
/// The skill can still override this via `compass outcome --verdict`. Reuses the
/// `Verdict` snake_case token mapping from `outcome`.
fn suggest_verdict_command(args: SuggestVerdictArgs) -> Result<()> {
    let facts = outcome::OutcomeFacts {
        tests_delta: args.tests_delta,
        regressions: args.regressions,
        gap_closed: args.gap_closed,
    };
    let verdict = outcome::suggest_verdict(&facts);
    println!("{}", serde_json::json!({ "suggested": verdict }));
    Ok(())
}

/// opportunity (PDO OST): record/list named bets under the active outcome. The
/// "active outcome" is the charter `north_star` snapshot (mirroring how
/// `outcome` snapshots its goal), overridable per-call with `--outcome`. The
/// charter is loaded tolerantly: a missing/blank north_star still works (the
/// opportunity is filed under the empty active outcome) so `add`/`list` never
/// wedge on a not-yet-carved charter.
fn opportunity_command(args: OpportunityArgs) -> Result<()> {
    let root = project_root();
    let active_outcome = Charter::load(&Charter::project_path(&root))
        .map(|c| c.north_star)
        .unwrap_or_default();

    match args.cmd {
        OpportunityCmd::Add {
            title,
            outcome,
            weight,
        } => {
            let outcome_ref = outcome.unwrap_or(active_outcome);
            let weight = weight.unwrap_or(opportunity::DEFAULT_WEIGHT);
            let rec = opportunity::record(&root, &title, &outcome_ref, weight)?;
            // DoD#1: a named bet is also a discovered task — emit a discovery row
            // for it under the current session. Fail-soft: this must NOT change
            // the `opportunity add` behavior/output/exit if the store errors.
            discovery::append_discovered(&root, &rec.title, &discovery::session_from_env());
            println!(
                "compass: recorded opportunity [{}] \"{}\" (weight {}) under outcome — {}",
                rec.id,
                rec.title,
                rec.weight,
                opportunity::store_path(&root).display()
            );
        }
        OpportunityCmd::List { json, outcome } => {
            let outcome_ref = outcome.unwrap_or(active_outcome);
            let found = opportunity::list_under(&root, &outcome_ref)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&found)?);
            } else if found.is_empty() {
                println!("(no opportunities under active outcome)");
            } else {
                for o in &found {
                    println!("- [{}] (weight {}) {}", o.id, o.weight, o.title);
                }
            }
        }
    }
    Ok(())
}

/// discovery (cross-session dedup): record/select/list rows in the machine-scope
/// shared discovery store. All three subcommands are fail-soft — the store calls
/// swallow their own errors — so a skill/hook invocation always exits 0. The
/// project cwd ([`project_root`]) keys the per-project store.
fn discovery_command(args: DiscoveryArgs) -> Result<()> {
    let cwd = project_root();
    match args.cmd {
        DiscoveryCmd::Record { title, session_id } => {
            discovery::record_command(&cwd, title, session_id);
        }
        DiscoveryCmd::Select {
            fingerprint,
            title,
            session_id: _,
        } => {
            discovery::select_command(&cwd, fingerprint, title);
        }
        DiscoveryCmd::List { json } => {
            discovery::list_command(&cwd, json)?;
        }
    }
    Ok(())
}

/// route (§13): consume an already-produced condukt decomposition JSON and
/// triage it deterministically by `size`. Prints the routing as JSON, writes
/// parked tasks to taskprog, and prints the condukt handoff text. No LLM.
fn route_command(args: RouteArgs) -> Result<()> {
    let root = project_root();
    let cfg = Config::load(&root);

    // Read the decomposition from --file or stdin.
    let raw = match &args.file {
        Some(path) => std::fs::read_to_string(path)
            .with_context(|| format!("reading decomposition {}", path.display()))?,
        None => {
            let mut buf = String::new();
            std::io::stdin()
                .read_to_string(&mut buf)
                .context("reading decomposition from stdin")?;
            buf
        }
    };
    let dec: Decomposition = serde_json::from_str(&raw).context("parsing decomposition JSON")?;

    let routing = route::route(&dec, &cfg);

    // Print the routing (+ edge) as JSON.
    println!("{}", serde_json::to_string_pretty(&routing)?);

    // Write parked tasks to the taskprog "残り" sink (self-feeding loop, §6).
    route::write_parked_to_taskprog(&root, &routing.parked)?;

    // Print the condukt handoff 課題 text (context for the chosen move), with the
    // named opportunities under the active outcome (charter north_star) so the
    // handed-off solution carries its opportunity refs (DoD#2).
    let charter = Charter::load(&Charter::project_path(&root)).unwrap_or_default();
    // Cross-session dedup (DoD#2) on the handed-off opportunity refs; byte-equiv
    // fallback (DoD#4) when the discovery store is absent/corrupt.
    let ranked = opportunity::list_under_ranked(&root, &charter.north_star).unwrap_or_default();
    let my_session = discovery::session_from_env();
    let opportunities = discovery::filter_undiscovered_by_others(&root, &my_session, ranked);
    println!();
    print!(
        "{}",
        route::condukt_handoff(&routing.to_condukt, &charter, &dec.goal, &opportunities)
    );
    Ok(())
}

#[cfg(test)]
mod nudge_tests {
    use super::*;

    #[test]
    fn absent_charter_is_not_fresh() {
        let dir = tempfile::tempdir().expect("tempdir");
        let v = nudge_verdict(dir.path());
        assert!(!v.fresh, "no charter file => not fresh");
        assert!(
            v.reason.as_deref().unwrap_or("").contains("no .compass"),
            "reason should point at the missing charter, got {:?}",
            v.reason
        );
    }

    #[test]
    fn blurry_charter_is_not_fresh() {
        let dir = tempfile::tempdir().expect("tempdir");
        let charter = Charter {
            north_star: "x".to_string(),
            definition_of_done: vec![], // empty DoD => blurry
            ..Charter::default()
        };
        let path = Charter::project_path(dir.path());
        let v = verdict_for_charter(dir.path(), &path, &charter, &Config::default());
        assert!(!v.fresh, "empty DoD => blurry => not fresh");
        assert!(v.reason.as_deref().unwrap_or("").contains("blurry"));
    }

    #[test]
    fn complete_unstale_charter_is_fresh() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Non-git tempdir, just-created: commit-divergence/elapsed-days produce
        // no signal; a DoD with no path tokens and an empty next_action keep the
        // C2 floor silent — so the verdict is fresh.
        let charter = Charter {
            north_star: "ship the measurement loop".to_string(),
            definition_of_done: vec!["users can close the loop".to_string()],
            ..Charter::default()
        };
        let path = Charter::project_path(dir.path());
        let v = verdict_for_charter(dir.path(), &path, &charter, &Config::default());
        assert!(
            v.fresh,
            "complete, non-stale charter => fresh (reason {:?})",
            v.reason
        );
        assert!(v.reason.is_none());
    }
}
