// テスト内の unwrap/expect は意図的な assert であって fail-open ではないので許可する。
// production 側は workspace の [workspace.lints.clippy] で deny のまま。
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]
//! specforge — the generation-side harness (sibling to specguard, DESIGN.md).
//!
//! This binary drives the generation pipeline end to end:
//!   ②normalize  → `draft`        (rigor pre-flight G1–G4; sentinel on shortfall)
//!   ③ratify     → `ratify`       (human consent ceremony, pins canon commit)
//!   ④prompt     → `impl-prompt`  (render one impl prompt per requirement)
//!   ⑤impl       → `implement`    (parallel impl agents, one git worktree each)
//!   ⑥evidence   → `evidence`/`agree`  (typed evidence gate + human agreement)
//!   ⑦merge      → `merge`        (merge passing worktrees after agreement)
//!
//! HOTL: the judgment (can this be rigorously specified? does the impl satisfy
//! the acceptance criteria?) lives in the agent; this binary is the deterministic
//! harness (scope, render, spawn, parse, gate). When the rigor gate is not met the
//! harness does NOT fabricate a draft — it raises a sentinel and pulls the human
//! in. A draft becomes canon only via `ratify`, and worktrees merge only after
//! `agree` — human consent at both boundaries; the middle is mechanical.
#![deny(clippy::panic)]

mod agent;
mod config;
mod evidence;
mod gather;
mod impl_prompt;
mod implement;
mod ir;
mod parse;
mod prompt;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use config::Config;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// Exit codes — kept disjoint from anything the agent returns, mirroring
/// specguard so a scheduler/hook sees one stable contract across both tools.
const EXIT_OK: u8 = 0;
const EXIT_USAGE: u8 = 2;
const EXIT_NO_MARKER: u8 = 3;
const EXIT_AGENT_FAILED: u8 = 4;
/// Reserved: a future `implement` refuses an unratified/changed spec (DESIGN.md
/// §5, §9-3). Defined here so the contract stays disjoint as the pipeline grows.
#[allow(dead_code)]
const EXIT_UNRATIFIED_SPEC: u8 = 6;
/// ① gather found no material to ground the topic (DESIGN-INTAKE.md §8): no
/// bundle is written and `needs_user: yes` is emitted so the human is pulled in
/// (principle 6 — don't fabricate a bundle). Disjoint from the reserved values.
const EXIT_INTAKE_SHORTFALL: u8 = 7;
/// ① gather could not READ one or more sources that EXIST (permission/IO error,
/// NOT NotFound) — intake is INCOMPLETE, distinct from the "nothing found"
/// shortfall (§7). Fails CLOSED: no bundle is written (a silently-partial bundle
/// would let the spec be forged missing material) and `needs_user: yes` +
/// `intake_incomplete: yes` are emitted so the human is pulled in. Mirror of
/// specguard's `EXIT_TESTAUDIT_UNDETERMINED=8` cannot-determine (round #7),
/// carried into the forge intake stage. Disjoint from every value above.
const EXIT_INTAKE_INCOMPLETE: u8 = 8;

#[derive(Parser)]
#[command(
    name = "specforge",
    version,
    about = "Spec generation harness (entry gate: normalize + rigor)"
)]
struct Cli {
    #[arg(short, long, default_value = "specforge.toml", global = true)]
    config: PathBuf,
    /// Override the run date (YYYY-MM-DD); default today (also SPECFORGE_NOW).
    #[arg(long, global = true)]
    date: Option<String>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// ① Deterministically gather requirement material from 3 sources
    /// (Obsidian/repo/past-prompts) into a provenance-tagged bundle.
    Gather {
        /// Topic string to score sources against.
        #[arg(long)]
        topic: String,
        /// Optional id for the bundle filename (default: a slug of the topic).
        #[arg(long)]
        id: Option<String>,
    },
    /// Normalize a requirement into a draft Spec IR (rigor-gated).
    Draft {
        /// Spec id (becomes `<spec_dir>/<id>.toml`).
        #[arg(long)]
        id: String,
        /// Optional human title.
        #[arg(long, default_value = "")]
        title: String,
        /// Requirement file (omit to read the requirement from stdin).
        #[arg(long)]
        req: Option<PathBuf>,
        /// canon pointer(s) backing the requirement (repeatable).
        #[arg(long)]
        canon: Vec<String>,
    },
    /// Render the normalize prompt only (no agent call) — debug view.
    Prompt {
        #[arg(long)]
        id: String,
        #[arg(long, default_value = "")]
        title: String,
        #[arg(long)]
        req: Option<PathBuf>,
        #[arg(long)]
        canon: Vec<String>,
    },
    /// Promote a draft spec to ratified (human consent ceremony, DESIGN.md §5).
    Ratify {
        #[arg(long)]
        id: String,
        /// Why this spec is accepted (recorded in the ratification record).
        #[arg(short = 'm', long = "reason")]
        reason: String,
    },
    /// Clear the sentinel after a human has handled the escalation.
    Ack,
    /// ④ Render per-requirement impl prompts for a ratified spec (no agent call).
    ImplPrompt {
        /// Spec id to render prompts for.
        #[arg(long)]
        id: String,
    },
    /// ⑤ Spawn parallel impl agents (one git worktree per requirement).
    Implement {
        /// Spec id (must be ratified; exits 6 if not).
        #[arg(long)]
        id: String,
    },
    /// ⑥ Show the evidence gate for a completed implementation run.
    Evidence {
        /// Spec id to show evidence for.
        #[arg(long)]
        id: String,
    },
    /// ⑥ Record human agreement with the evidence gate so merges can proceed.
    Agree {
        #[arg(long)]
        id: String,
        /// Why this implementation is accepted.
        #[arg(short = 'm', long = "reason")]
        reason: String,
    },
    /// ⑦ Merge passing worktrees into main after agreement.
    Merge {
        #[arg(long)]
        id: String,
        /// Merge only a specific requirement (default: all passing).
        #[arg(long)]
        req: Option<String>,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(&cli) {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            eprintln!("specforge: error: {e:#}");
            ExitCode::from(EXIT_USAGE)
        }
    }
}

struct Loaded {
    cfg: Config,
    repo_root: PathBuf,
    config_dir: PathBuf,
    date: String,
}

fn load(cli: &Cli) -> Result<Loaded> {
    let cfg = Config::load(&cli.config)?;
    let config_dir = cli
        .config
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();
    let repo_root = canonicalize(&config_dir.join(&cfg.project.root))
        .with_context(|| "resolving project.root")?;
    let date = resolve_date(cli.date.clone());
    Ok(Loaded {
        cfg,
        repo_root,
        config_dir,
        date,
    })
}

fn run(cli: &Cli) -> Result<u8> {
    let l = load(cli)?;
    match &cli.command {
        Command::Gather { topic, id } => cmd_gather(&l, topic, id.as_deref()),
        Command::Draft {
            id,
            title,
            req,
            canon,
        } => draft(&l, id, title, req.as_deref(), canon),
        Command::Prompt {
            id,
            title,
            req,
            canon,
        } => {
            let _ = title;
            let requirement = read_requirement(req.as_deref())?;
            let template = load_template(&l)?;
            let out = prompt::render(
                &template,
                &l.cfg.project.name,
                id,
                &requirement,
                canon,
                &l.date,
            );
            print!("{out}");
            Ok(EXIT_OK)
        }
        Command::Ratify { id, reason } => ratify(&l, id, reason),
        Command::Ack => ack(&l),
        Command::ImplPrompt { id } => cmd_impl_prompt(&l, id),
        Command::Implement { id } => cmd_implement(&l, id),
        Command::Evidence { id } => cmd_evidence(&l, id),
        Command::Agree { id, reason } => cmd_agree(&l, id, reason),
        Command::Merge { id, req } => cmd_merge(&l, id, req.as_deref()),
    }
}

/// ②normalize + §5.3 rigor pre-flight. The agent decides; the harness gates.
fn draft(l: &Loaded, id: &str, title: &str, req: Option<&Path>, canon: &[String]) -> Result<u8> {
    let requirement = read_requirement(req)?;
    let template = load_template(l)?;
    let rendered = prompt::render(
        &template,
        &l.cfg.project.name,
        id,
        &requirement,
        canon,
        &l.date,
    );

    let out = agent::run(&l.cfg.agent, &l.repo_root, &rendered);
    if out.code != 0 {
        eprintln!(
            "specforge: normalize agent exited with code {}\n--- agent stderr ---\n{}",
            out.code,
            out.stderr.trim_end()
        );
        return Ok(EXIT_AGENT_FAILED);
    }

    let parsed = parse::parse(&out.stdout);
    if !parsed.marker_found {
        eprintln!(
            "specforge: WARN marker '{}' missing in agent output; cannot assess. No draft, no sentinel.",
            parse::MARKER
        );
        return Ok(EXIT_NO_MARKER);
    }

    // Escalation path (insufficiency §5.2 / conflict §5.1): the agent could not
    // rigorously specify. Do NOT fabricate a draft — raise a sentinel and pull
    // the human in. This is a *successful* HOTL outcome (exit 0).
    if !parsed.rigor_pass || parsed.needs_user {
        write_sentinel(l, id, &parsed.summary, &parsed.body)?;
        println!(
            "specforge: rigor 未達 — 厳格に仕様化できない。sentinel を上げた ({}); draft は書かない。\n  summary: {}",
            sentinel_path(l).display(),
            parsed.summary
        );
        return Ok(EXIT_OK);
    }

    // Rigor claimed pass: parse the requirement TOML the agent emitted.
    let agent_draft = match ir::AgentDraft::parse(&parsed.body) {
        Ok(d) => d,
        Err(e) => {
            eprintln!(
                "specforge: WARN rigor:pass but body is not valid requirement TOML: {e}\n  No draft written (agent contract violation)."
            );
            return Ok(EXIT_NO_MARKER);
        }
    };

    let head = current_head(&l.repo_root).unwrap_or_else(|_| "UNKNOWN".to_string());
    let spec = ir::Spec {
        spec: ir::SpecMeta {
            id: id.to_string(),
            title: title.to_string(),
            status: "draft".to_string(),
            provenance_commit: head,
            date: l.date.clone(),
            canon: canon.to_vec(),
            ratification: None,
        },
        requirements: agent_draft.requirements,
    };

    // Deterministic floor: the harness re-checks the rigor contract the agent
    // claimed. If the agent over-claimed (e.g. an acceptance with no canon),
    // escalate rather than persist an ungrounded draft — the machine guards the
    // gate even when the LLM says pass.
    let violations = spec.contract_violations();
    if !violations.is_empty() {
        let body = format!(
            "agent は rigor:pass を主張したが、ハーネスの契約チェックで以下に違反:\n{}",
            violations
                .iter()
                .map(|v| format!("- {v}\n"))
                .collect::<String>()
        );
        write_sentinel(l, id, "rigor:pass の過大主張をハーネスが棄却", &body)?;
        println!(
            "specforge: agent が rigor:pass を過大主張 — 契約違反を検出し sentinel を上げた ({}); draft は書かない。",
            sentinel_path(l).display()
        );
        for v in &violations {
            println!("  - {v}");
        }
        return Ok(EXIT_OK);
    }

    let path = write_spec(l, id, &spec)?;
    println!("specforge: draft を書いた -> {}", path.display());
    println!(
        "  内容を確認し、合意できるなら `specforge ratify --id {id} -m \"理由\"` で昇格 (HOTL)。"
    );
    Ok(EXIT_OK)
}

/// Promote draft → ratified: contract-check, then pin consent (DESIGN.md §5).
fn ratify(l: &Loaded, id: &str, reason: &str) -> Result<u8> {
    if reason.trim().is_empty() {
        anyhow::bail!("批准には理由が必要です (-m \"...\")");
    }
    let path = spec_path(l, id);
    if !path.exists() {
        anyhow::bail!(
            "spec が無い: {} (先に `specforge draft --id {id}`)",
            path.display()
        );
    }
    let mut spec = ir::Spec::load(&path)?;

    // Machine contract floor (G1/G4): refuse to ratify an ungrounded spec. The
    // policy judgment is the human's, recorded as the reason.
    let violations = spec.contract_violations();
    if !violations.is_empty() {
        let mut msg = String::from("spec が rigor 契約に違反 — 批准を拒否:");
        for v in &violations {
            msg.push_str(&format!("\n  - {v}"));
        }
        anyhow::bail!(msg);
    }

    let head = current_head(&l.repo_root).unwrap_or_else(|_| "UNKNOWN".to_string());
    let fingerprint = spec.fingerprint();
    spec.spec.status = "ratified".to_string();
    spec.spec.ratification = Some(ir::Ratification {
        canon_commit: head.clone(),
        date: l.date.clone(),
        reason: reason.to_string(),
        fingerprint,
    });
    write_spec(l, id, &spec)?;
    println!(
        "specforge: spec を批准した (draft -> ratified) -> {}",
        path.display()
    );
    println!("  canon commit に pin (canon_commit: {head})");
    println!("  理由: {reason}");
    println!("  以後、内容を編集すると fingerprint が変わり再批准が必要 (DESIGN.md §5)。");
    Ok(EXIT_OK)
}

/// Clear the sentinel (idempotent).
fn ack(l: &Loaded) -> Result<u8> {
    let path = sentinel_path(l);
    match std::fs::remove_file(&path) {
        Ok(()) => println!("specforge: sentinel をクリアした ({})", path.display()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            println!("specforge: sentinel は無い ({})", path.display());
        }
        Err(e) => {
            return Err(anyhow::Error::new(e))
                .with_context(|| format!("removing sentinel {}", path.display()));
        }
    }
    Ok(EXIT_OK)
}

// ── ① gather ───────────────────────────────────────────────────────────────

/// ① Deterministic intake (DESIGN-INTAKE.md §1–§3). Walk the 3 sources, score
/// against the topic, persist the bundle, and emit the §8 marker contract. No
/// LLM, no conflict resolution — authority only orders, never drops.
fn cmd_gather(l: &Loaded, topic: &str, id: Option<&str>) -> Result<u8> {
    if topic.trim().is_empty() {
        anyhow::bail!("--topic が空です");
    }
    let key = id.map(|s| s.to_string()).unwrap_or_else(|| slugify(topic));

    let input = gather::GatherInput {
        obsidian_vault: nonempty(&l.cfg.sources.obsidian_vault).map(expand_tilde),
        canon_root: l.repo_root.clone(),
        canon_globs: l.cfg.sources.canon.clone(),
        transcripts_dir: nonempty(&l.cfg.sources.transcripts)
            .map(|s| expand_tilde(s).join(gather::encode_cwd(&l.repo_root))),
        top_k: l.cfg.gather.top_k,
        min_score: l.cfg.gather.min_score,
    };

    let outcome = gather::gather(topic, &input);
    let bundle = &outcome.bundle;

    // Cannot-determine intake (mirror testaudit round #7): one or more sources
    // EXIST but could not be read (permission/IO error, not NotFound). Do NOT
    // report a clean gather — a silently-partial bundle would let the spec be
    // forged missing material it could not read. Fail closed: write no bundle and
    // pull the human in (principle 6), distinct from the "nothing found" shortfall.
    if !outcome.unreadable.is_empty() {
        eprintln!(
            "specforge: gather — 判定不能: 次のソースは存在するが読めない (intake が不完全 → 束は書かない):"
        );
        for u in &outcome.unreadable {
            eprintln!("  - {u}");
        }
        println!("{}", gather::MARKER);
        println!("needs_user: yes");
        println!("intake_incomplete: yes");
        println!("fragment_count: {}", bundle.fragments.len());
        return Ok(EXIT_INTAKE_INCOMPLETE);
    }

    // Material shortfall (principle 6): do NOT fabricate a bundle — pull the
    // human in. Emit the §8 trailer with needs_user: yes and no bundle_path.
    if bundle.fragments.is_empty() {
        eprintln!(
            "specforge: gather — トピック '{topic}' に接地する素材が3ソースから見つからない。束は書かない (intake 素材不足)。"
        );
        println!("{}", gather::MARKER);
        println!("needs_user: yes");
        println!("fragment_count: 0");
        return Ok(EXIT_INTAKE_SHORTFALL);
    }

    let path = write_bundle(l, &key, bundle)?;
    print_gather_summary(bundle);
    println!();
    println!("束を書いた -> {}", path.display());
    println!("{}", gather::MARKER);
    println!("bundle_path: {}", path.display());
    println!("fragment_count: {}", bundle.fragments.len());
    Ok(EXIT_OK)
}

fn print_gather_summary(bundle: &gather::Bundle) {
    println!(
        "specforge: gather '{}' — {} fragments (authority desc, score desc):",
        bundle.topic,
        bundle.fragments.len()
    );
    for f in &bundle.fragments {
        let auth = match f.authority {
            gather::Authority::High => "高",
            gather::Authority::Mid => "中",
            gather::Authority::Low => "低",
        };
        println!(
            "  [{auth} score={}] {} ({})",
            f.score, f.anchor, f.source_path
        );
    }
}

fn bundle_path(l: &Loaded, key: &str) -> PathBuf {
    l.repo_root
        .join(&l.cfg.output.spec_dir)
        .join(format!("{key}.gather.json"))
}

fn write_bundle(l: &Loaded, key: &str, bundle: &gather::Bundle) -> Result<PathBuf> {
    let path = bundle_path(l, key);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("creating bundle dir {}", dir.display()))?;
    }
    let json = serde_json::to_string_pretty(bundle).context("serializing gather bundle")?;
    std::fs::write(&path, json).with_context(|| format!("writing bundle {}", path.display()))?;
    Ok(path)
}

/// `None` for an empty/blank config string, else the trimmed value.
fn nonempty(s: &str) -> Option<&str> {
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        Some(t)
    }
}

/// Expand a leading `~` to $HOME (best-effort; left as-is if HOME is unset).
fn expand_tilde(s: &str) -> PathBuf {
    if let Some(rest) = s.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home).join(rest);
        }
    } else if s == "~" {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home);
        }
    }
    PathBuf::from(s)
}

/// Slug of a topic for the default bundle filename: lowercase alnum runs joined
/// by `-`, CJK kept verbatim (so Japanese topics produce readable filenames).
fn slugify(topic: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for c in topic.chars() {
        if c.is_alphanumeric() {
            out.extend(c.to_lowercase());
            prev_dash = false;
        } else if !prev_dash && !out.is_empty() {
            out.push('-');
            prev_dash = true;
        }
    }
    let s = out.trim_matches('-').to_string();
    if s.is_empty() {
        "topic".to_string()
    } else {
        s
    }
}

// --- helpers ---

fn read_requirement(req: Option<&Path>) -> Result<String> {
    match req {
        Some(p) => std::fs::read_to_string(p)
            .with_context(|| format!("reading requirement {}", p.display())),
        None => {
            use std::io::Read;
            let mut s = String::new();
            std::io::stdin()
                .read_to_string(&mut s)
                .context("reading requirement from stdin")?;
            if s.trim().is_empty() {
                anyhow::bail!("要望が空です (--req <file> か stdin で渡してください)");
            }
            Ok(s)
        }
    }
}

fn load_template(l: &Loaded) -> Result<String> {
    if l.cfg.prompt.normalize_template.trim().is_empty() {
        return Ok(prompt::DEFAULT_TEMPLATE.to_string());
    }
    let p = l.config_dir.join(&l.cfg.prompt.normalize_template);
    let template = std::fs::read_to_string(&p)
        .with_context(|| format!("reading normalize template {}", p.display()))?;
    // The template is meta-canon: contract-check it (required placeholders) so a
    // custom template can't silently break the render/parse contract.
    let missing = prompt::missing_placeholders(&template);
    if !missing.is_empty() {
        anyhow::bail!(
            "normalize template {} は必須 placeholder を欠く: {}",
            p.display(),
            missing.join(", ")
        );
    }
    Ok(template)
}

fn spec_path(l: &Loaded, id: &str) -> PathBuf {
    l.repo_root
        .join(&l.cfg.output.spec_dir)
        .join(format!("{id}.toml"))
}

fn write_spec(l: &Loaded, id: &str, spec: &ir::Spec) -> Result<PathBuf> {
    let path = spec_path(l, id);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("creating spec dir {}", dir.display()))?;
    }
    std::fs::write(&path, spec.to_toml()?)
        .with_context(|| format!("writing spec {}", path.display()))?;
    Ok(path)
}

fn sentinel_path(l: &Loaded) -> PathBuf {
    l.repo_root.join(&l.cfg.output.sentinel)
}

/// Raise the escalation sentinel (HOTL pull). Same spirit as specguard's: a
/// date/spec/summary header plus the agent's request body, for a SessionStart
/// hook to surface.
fn write_sentinel(l: &Loaded, id: &str, summary: &str, body: &str) -> Result<()> {
    let path = sentinel_path(l);
    let content = format!(
        "date: {}\nspec: {}\nsummary: {}\n\n{}\n",
        l.date,
        id,
        summary,
        body.trim_end()
    );
    std::fs::write(&path, content)
        .with_context(|| format!("writing sentinel {}", path.display()))?;
    Ok(())
}

fn current_head(repo_root: &Path) -> Result<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["rev-parse", "HEAD"])
        .output()
        .context("running git rev-parse")?;
    if !out.status.success() {
        anyhow::bail!("git rev-parse failed");
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn resolve_date(cli_date: Option<String>) -> String {
    if let Some(d) = cli_date {
        return d;
    }
    if let Ok(d) = std::env::var("SPECFORGE_NOW") {
        if !d.trim().is_empty() {
            return d.trim().to_string();
        }
    }
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

fn canonicalize(p: &Path) -> Result<PathBuf> {
    std::fs::canonicalize(p).or_else(|_| Ok(p.to_path_buf()))
}

// ── ④ impl-prompt ────────────────────────────────────────────────────────────

fn load_impl_template(l: &Loaded) -> Result<String> {
    if l.cfg.prompt.impl_template.trim().is_empty() {
        return Ok(impl_prompt::DEFAULT_TEMPLATE.to_string());
    }
    let p = l.config_dir.join(&l.cfg.prompt.impl_template);
    let template = std::fs::read_to_string(&p)
        .with_context(|| format!("reading impl template {}", p.display()))?;
    let missing = impl_prompt::missing_placeholders(&template);
    if !missing.is_empty() {
        anyhow::bail!(
            "impl template {} は必須 placeholder を欠く: {}",
            p.display(),
            missing.join(", ")
        );
    }
    Ok(template)
}

fn impl_prompt_dir(l: &Loaded) -> PathBuf {
    l.repo_root.join(&l.cfg.output.impl_prompt_dir)
}

fn impl_dir(l: &Loaded) -> PathBuf {
    l.repo_root.join(&l.cfg.output.impl_dir)
}

fn worktree_base(l: &Loaded) -> PathBuf {
    l.repo_root.join(&l.cfg.output.worktree_base)
}

/// ④ Render per-requirement impl prompts for a ratified spec (no agent call).
fn cmd_impl_prompt(l: &Loaded, id: &str) -> Result<u8> {
    let spec = ir::Spec::load(&spec_path(l, id))?;
    if spec.spec.status != "ratified" {
        anyhow::bail!(
            "spec '{}' は ratified ではない (status: {}) — 先に `specforge ratify --id {id}` を実行。",
            id,
            spec.spec.status
        );
    }
    let template = load_impl_template(l)?;
    let prompts = impl_prompt::render_all(&template, &l.cfg.project.name, &spec, &l.date);
    let dir = impl_prompt_dir(l);
    let paths = impl_prompt::write_prompts(&dir, id, &prompts)?;
    println!("specforge: {} 件の impl prompt を書いた:", paths.len());
    for p in &paths {
        println!("  {}", p.display());
    }
    println!("次: `specforge implement --id {id}` で並列実装を起動する。");
    Ok(EXIT_OK)
}

// ── ⑤ implement ──────────────────────────────────────────────────────────────

/// ⑤ Spawn parallel impl agents (one git worktree per requirement).
fn cmd_implement(l: &Loaded, id: &str) -> Result<u8> {
    let spec = ir::Spec::load(&spec_path(l, id))?;
    if spec.spec.status != "ratified" {
        eprintln!(
            "specforge: spec '{}' は ratified ではない — 実装を拒否 (EXIT_UNRATIFIED_SPEC)。",
            id
        );
        return Ok(EXIT_UNRATIFIED_SPEC);
    }

    // Verify fingerprint hasn't changed since ratification.
    if let Some(ref rat) = spec.spec.ratification {
        let current_fp = spec.fingerprint();
        if current_fp != rat.fingerprint {
            anyhow::bail!(
                "spec '{}' は批准後に変更された (fingerprint 不一致)。再批准してください。",
                id
            );
        }
    }

    let template = load_impl_template(l)?;
    let prompts = impl_prompt::render_all(&template, &l.cfg.project.name, &spec, &l.date);
    if prompts.is_empty() {
        anyhow::bail!("spec '{}' に requirement が無い", id);
    }

    let wb = worktree_base(l);
    println!(
        "specforge: {} requirements を並列実装 (cap {})...",
        prompts.len(),
        implement::MAX_PARALLEL
    );

    let tasks: Vec<implement::TaskInput> = prompts
        .into_iter()
        .map(|(req_id, prompt)| implement::TaskInput {
            spec_id: id.to_string(),
            req_id,
            prompt,
        })
        .collect();

    let results = implement::run_parallel(
        &l.repo_root,
        tasks,
        &wb,
        &l.cfg.agent,
        l.cfg.evidence.require_verification,
    );

    // Persist results and build evidence.
    let idir = impl_dir(l);
    let results_path = implement::write_results(&idir, id, &results)?;
    let report = evidence::build(id, &l.date, &results);
    let ev_path = evidence::write(&idir, &report)?;

    println!();
    evidence::print_summary(&report);
    println!();
    println!(
        "結果: {}  証拠: {}",
        results_path.display(),
        ev_path.display()
    );
    Ok(EXIT_OK)
}

// ── ⑥ evidence / agree ───────────────────────────────────────────────────────

/// Load the persisted evidence report, or regenerate a fresh (pending) one from
/// the raw impl results if the evidence file is missing. A persisted report wins
/// because it carries the human's gate decision; the fallback keeps the gate
/// usable if the evidence file was deleted but impl results survive.
fn load_or_build_evidence(l: &Loaded, id: &str) -> Result<evidence::EvidenceReport> {
    let dir = impl_dir(l);
    match evidence::load(&dir, id) {
        Ok(r) => Ok(r),
        Err(_) => {
            let results = implement::load_results(&dir, id).with_context(|| {
                format!("no evidence or impl results for spec '{id}' — run `specforge implement --id {id}` first")
            })?;
            Ok(evidence::build(id, &l.date, &results))
        }
    }
}

fn cmd_evidence(l: &Loaded, id: &str) -> Result<u8> {
    let report = load_or_build_evidence(l, id)?;
    evidence::print_summary(&report);
    Ok(EXIT_OK)
}

fn cmd_agree(l: &Loaded, id: &str, reason: &str) -> Result<u8> {
    if reason.trim().is_empty() {
        anyhow::bail!("合意には理由が必要です (-m \"...\")");
    }
    // Ensure an evidence report exists on disk (regenerate from impl results if
    // needed) before recording agreement, so `agree` works even if only the raw
    // impl results survived.
    let report = load_or_build_evidence(l, id)?;
    evidence::write(&impl_dir(l), &report)?;
    let report = evidence::record_agreement(&impl_dir(l), id, reason)?;
    println!(
        "specforge: 合意を記録した (gate: agreed) — {} requirements",
        report.total
    );
    println!("  次: `specforge merge --id {id}` で passing worktrees を統合できます。");
    Ok(EXIT_OK)
}

// ── ⑦ merge ──────────────────────────────────────────────────────────────────

fn cmd_merge(l: &Loaded, id: &str, req_filter: Option<&str>) -> Result<u8> {
    let report = load_or_build_evidence(l, id)?;
    if !report.is_gate_open() {
        anyhow::bail!(
            "evidence gate が closed (status: {}) — 先に `specforge agree --id {id} -m \"理由\"` を実行。",
            report.gate_status
        );
    }

    let items_to_merge: Vec<_> = report
        .items
        .iter()
        .filter(|item| item.is_done())
        .filter(|item| req_filter.is_none_or(|f| item.req_id == f))
        .collect();

    if items_to_merge.is_empty() {
        println!("specforge: merge 対象の done requirement が無い。");
        return Ok(EXIT_OK);
    }

    println!("specforge: {} worktrees を merge...", items_to_merge.len());
    let mut merged = 0usize;
    for item in &items_to_merge {
        let wt_path = match &item.worktree {
            Some(p) => std::path::PathBuf::from(p),
            None => {
                eprintln!("  skip {} — worktree path が記録されていない", item.req_id);
                continue;
            }
        };
        let branch = format!("specforge/{id}/{}", item.req_id);
        // git merge --no-ff the worktree branch into HEAD.
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&l.repo_root)
            .args(["merge", "--no-ff", "-m"])
            .arg(format!(
                "specforge: merge {id}/{} (evidence agreed)",
                item.req_id
            ))
            .arg(&branch)
            .output()
            .context("git merge")?;
        if out.status.success() {
            println!("  ✔ merged {}", item.req_id);
            // Clean up the worktree.
            let _ = std::process::Command::new("git")
                .arg("-C")
                .arg(&l.repo_root)
                .args(["worktree", "remove", "--force"])
                .arg(&wt_path)
                .output();
            merged += 1;
        } else {
            let stderr = String::from_utf8_lossy(&out.stderr);
            eprintln!("  ✗ merge {} failed: {}", item.req_id, stderr.trim());
        }
    }
    println!(
        "specforge: {merged}/{} worktrees をマージした。",
        items_to_merge.len()
    );
    Ok(EXIT_OK)
}
