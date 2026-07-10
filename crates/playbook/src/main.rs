//! playbook — project knowledge retrieval + injection for Claude Code.
//!
//! `inject` is the UserPromptSubmit hook: it scores the curated notes against
//! the prompt and prints the most relevant ones (under a char budget) as added
//! context, so a project's conventions and gotchas resurface without the user
//! re-typing them. The rest of the subcommands curate the store.

mod classifier;
mod config;
mod install;
mod model;
mod retrieve;
mod store;

use std::path::Path;

use clap::{Parser, Subcommand};

use harness_core::hook::{read_stdin, run_hook};

use config::Config;
use model::HookInput;
use store::{slugify, Meta, Store};

#[derive(Parser)]
#[command(
    name = "playbook",
    version,
    about = "Project knowledge retrieval + injection for Claude Code (UserPromptSubmit hook)."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// UserPromptSubmit hook: inject notes relevant to the prompt.
    Inject,
    /// Add a knowledge note (body from --body or stdin).
    Add {
        #[arg(long)]
        title: String,
        /// Comma-separated high-weight trigger terms.
        #[arg(long, default_value = "")]
        trigger: String,
        /// Comma-separated tags.
        #[arg(long, default_value = "")]
        tags: String,
        /// Note body (otherwise read from stdin).
        #[arg(long)]
        body: Option<String>,
        /// Store in the shared global store instead of this project's.
        #[arg(long)]
        global: bool,
        /// Always inject this note regardless of relevance.
        #[arg(long)]
        always: bool,
    },
    /// List notes visible from the cwd (project + global).
    List,
    /// Show how notes score for a query (debug retrieval).
    Search { query: Vec<String> },
    /// Remove a note by slug.
    Rm { slug: String },
    /// Merge the UserPromptSubmit hook into ~/.claude/settings.json.
    Install {
        #[arg(long)]
        dry_run: bool,
    },
    /// Remove the playbook hook from ~/.claude/settings.json.
    Uninstall {
        #[arg(long)]
        dry_run: bool,
    },
    /// Create store dirs and a sample note.
    Init,
    /// Show resolved config + store locations.
    Status,
}

fn main() {
    let cli = Cli::parse();
    match cli.command {
        Command::Inject => run_hook(inject),
        Command::Add {
            title,
            trigger,
            tags,
            body,
            global,
            always,
        } => exit_on_err(add(title, trigger, tags, body, global, always)),
        Command::List => list(),
        Command::Search { query } => search(query.join(" ")),
        Command::Rm { slug } => rm(slug),
        Command::Install { dry_run } => exit_on_err(install::install(dry_run)),
        Command::Uninstall { dry_run } => exit_on_err(install::uninstall(dry_run)),
        Command::Init => exit_on_err(init()),
        Command::Status => status(),
    }
}

fn exit_on_err(r: anyhow::Result<()>) {
    if let Err(e) = r {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn inject() {
    if Config::disabled_env() {
        return;
    }
    let raw = read_stdin();
    let Some(input) = HookInput::parse(&raw) else {
        return;
    };
    let root = input.cwd_or_current();
    let cfg = Config::load(&root);
    if !cfg.enabled {
        return;
    }

    // Check for complex problems that warrant /flow routing.
    if classifier::is_complex_problem(&input.prompt) {
        let flow_guidance = format!(
            "⚡複雑な問題として検出されました。問題記述を `/flow <problem>` に渡し、parallel/serial化スケジューリングを活用することをお勧めします:\n\n→ /flow {}\n",
            input.prompt
        );
        harness_core::inject_metrics::record(
            "playbook.flow-classifier",
            &input.session_id,
            &input.prompt,
            flow_guidance.chars().count(),
        );
        println!("{flow_guidance}");
        // Still try to inject playbook notes alongside the flow guidance.
        // (Fail-soft: if notes are empty, we'll return below anyway.)
    }

    let store = Store::new(&cfg);
    let notes = store.load_visible(&root);
    if notes.is_empty() {
        return;
    }
    let chosen = retrieve::select(&notes, &input.prompt, &cfg);
    if chosen.is_empty() {
        return;
    }
    // UserPromptSubmit: plain stdout is injected as additional context.
    let injected = retrieve::render_injection(&chosen);
    harness_core::inject_metrics::record(
        "playbook",
        &input.session_id,
        &input.prompt,
        injected.chars().count(),
    );
    println!("{injected}");
}

fn split_csv(s: &str) -> Vec<String> {
    s.split(',')
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .collect()
}

fn add(
    title: String,
    trigger: String,
    tags: String,
    body: Option<String>,
    global: bool,
    always: bool,
) -> anyhow::Result<()> {
    let root = std::env::current_dir()?;
    let cfg = Config::load(&root);
    let store = Store::new(&cfg);
    let body = body.unwrap_or_else(read_stdin);
    let body = body.trim().to_string();
    if body.is_empty() {
        anyhow::bail!("empty body (pass --body or pipe text on stdin)");
    }
    let meta = Meta {
        title: title.clone(),
        tags: split_csv(&tags),
        triggers: split_csv(&trigger),
        scope: if global {
            "global".into()
        } else {
            "project".into()
        },
        always,
        created: chrono::Local::now().to_rfc3339(),
    };
    let slug = slugify(&title);
    let path = store.write(&root, &slug, &meta, &body, global)?;
    println!("wrote {} (slug: {slug})", path.display());
    Ok(())
}

fn list() {
    let root = std::env::current_dir().unwrap_or_else(|_| Path::new(".").to_path_buf());
    let cfg = Config::load(&root);
    let store = Store::new(&cfg);
    let notes = store.load_visible(&root);
    if notes.is_empty() {
        println!("(no notes — add one with `playbook add --title ...`)");
        return;
    }
    for n in &notes {
        let scope = if n.global { "global " } else { "project" };
        let flags = if n.meta.always { " *always" } else { "" };
        let trig = if n.meta.triggers.is_empty() {
            String::new()
        } else {
            format!("  triggers: {}", n.meta.triggers.join(","))
        };
        println!("[{scope}] {:<24} {}{flags}{trig}", n.slug, n.meta.title);
    }
}

fn search(query: String) {
    let root = std::env::current_dir().unwrap_or_else(|_| Path::new(".").to_path_buf());
    let cfg = Config::load(&root);
    let store = Store::new(&cfg);
    let notes = store.load_visible(&root);
    if notes.is_empty() {
        println!("(no notes)");
        return;
    }
    println!(
        "query: {query}\nmin_score={}  top_k={}\n",
        cfg.min_score, cfg.top_k
    );
    for sc in retrieve::scored_for(&notes, &query) {
        let mark = if sc.score >= cfg.min_score || sc.note.meta.always {
            "✓"
        } else {
            " "
        };
        println!(
            "{mark} {:>3}  {:<24} {}",
            sc.score, sc.note.slug, sc.note.meta.title
        );
    }
}

fn rm(slug: String) {
    let root = std::env::current_dir().unwrap_or_else(|_| Path::new(".").to_path_buf());
    let cfg = Config::load(&root);
    let store = Store::new(&cfg);
    match store.remove(&root, &slug) {
        Some(p) => println!("removed {}", p.display()),
        None => {
            eprintln!("no note with slug '{slug}' in project or global store");
            std::process::exit(1);
        }
    }
}

fn init() -> anyhow::Result<()> {
    let root = std::env::current_dir()?;
    let cfg = Config::load(&root);
    let store = Store::new(&cfg);
    std::fs::create_dir_all(store.project_dir(&root))?;
    std::fs::create_dir_all(store.global_dir())?;
    let sample = Meta {
        title: "example: keep diffs scoped".into(),
        tags: vec!["convention".into()],
        triggers: vec![],
        scope: "project".into(),
        always: false,
        created: chrono::Local::now().to_rfc3339(),
    };
    let slug = slugify(&sample.title);
    let p = store.write(
        &root,
        &slug,
        &sample,
        "これは playbook のサンプルノート。`playbook rm example-keep-diffs-scoped` で削除可。\n\
         本物のナレッジは `playbook add --title \"...\" --trigger \"lightgbm,memory\"` で追加してください。",
        false,
    )?;
    println!(
        "store ready.\n  project: {}\n  global:  {}",
        store.project_dir(&root).display(),
        store.global_dir().display()
    );
    println!("sample note: {}", p.display());
    Ok(())
}

fn status() {
    let root = std::env::current_dir().unwrap_or_else(|_| Path::new(".").to_path_buf());
    let cfg = Config::load(&root);
    let store = Store::new(&cfg);
    let src = if Config::project_path(&root).exists() {
        Config::project_path(&root)
    } else if Config::home_path().exists() {
        Config::home_path()
    } else {
        Path::new("(defaults — no config file)").to_path_buf()
    };
    let notes = store.load_visible(&root);
    println!("config:         {}", src.display());
    println!("enabled:        {}", cfg.enabled);
    println!("top_k:          {}", cfg.top_k);
    println!("min_score:      {}", cfg.min_score);
    println!("max_chars:      {}", cfg.max_chars);
    println!("include_global: {}", cfg.include_global);
    println!("project store:  {}", store.project_dir(&root).display());
    println!("global store:   {}", store.global_dir().display());
    println!("visible notes:  {}", notes.len());
}
