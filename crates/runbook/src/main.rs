//! runbook — reusable procedure includes for Claude Code.
//!
//! `inject` is the UserPromptSubmit hook: it expands `!name` macros the user
//! typed into the matching repo-committed procedure (`.runbook/<name>.md`), so a
//! recurring workflow runs the same way every time — the same idea as Devin's
//! `!playbook` macros, rebuilt as a local, no-API-key hook. A macro only fires
//! when it resolves to an existing runbook, so stray `!` never injects anything.
//!
//! Unlike `playbook` (which scores atomic *facts* by relevance and injects them
//! automatically), runbook injects whole *procedures* only when explicitly asked
//! for by name.

mod config;
mod inject;
mod install;
mod model;
mod store;

use std::path::Path;

use clap::{Parser, Subcommand};

use harness_core::hook::{read_stdin, run_hook};

use config::Config;
use model::HookInput;
use store::{normalize_name, Store};

#[derive(Parser)]
#[command(
    name = "runbook",
    version,
    about = "Reusable procedure includes for Claude Code (UserPromptSubmit hook). Expand !name macros into repo-committed procedures."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// UserPromptSubmit hook: expand `!name` macros in the prompt.
    Inject,
    /// List available runbooks (project + global).
    List,
    /// Print a runbook's content.
    Show { name: String },
    /// Scaffold a new procedure (.runbook/<name>.md) from a template.
    New {
        name: String,
        /// One-line description stored in frontmatter.
        #[arg(long, default_value = "")]
        description: String,
        /// Create in the shared global directory instead of this project.
        #[arg(long)]
        global: bool,
        /// Overwrite if it already exists.
        #[arg(long)]
        force: bool,
    },
    /// Create the project `.runbook/` dir and a sample procedure.
    Init,
    /// Merge the UserPromptSubmit hook into ~/.claude/settings.json.
    Install {
        #[arg(long)]
        dry_run: bool,
    },
    /// Remove the runbook hook from ~/.claude/settings.json.
    Uninstall {
        #[arg(long)]
        dry_run: bool,
    },
    /// Show resolved config + directories + runbook count.
    Status,
}

fn main() {
    let cli = Cli::parse();
    match cli.command {
        Command::Inject => run_hook(inject_hook),
        Command::List => list(),
        Command::Show { name } => show(&name),
        Command::New {
            name,
            description,
            global,
            force,
        } => exit_on_err(new(&name, &description, global, force)),
        Command::Init => exit_on_err(init()),
        Command::Install { dry_run } => exit_on_err(install::install(dry_run)),
        Command::Uninstall { dry_run } => exit_on_err(install::uninstall(dry_run)),
        Command::Status => status(),
    }
}

fn exit_on_err(r: anyhow::Result<()>) {
    if let Err(e) = r {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn inject_hook() {
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
    let store = Store::new(&cfg, &root);
    let books = store.load_all();
    let exp = inject::expand(&input.prompt, &books, &cfg);
    if let Some(text) = inject::render(&exp, &books, &cfg) {
        // UserPromptSubmit: plain stdout is injected as additional context.
        harness_core::inject_metrics::record(
            "runbook",
            &input.session_id,
            &input.prompt,
            text.chars().count(),
        );
        println!("{text}");
    }
}

fn load(root: &Path) -> (Config, Store) {
    let cfg = Config::load(root);
    let store = Store::new(&cfg, root);
    (cfg, store)
}

fn cwd() -> std::path::PathBuf {
    std::env::current_dir().unwrap_or_else(|_| Path::new(".").to_path_buf())
}

fn list() {
    let root = cwd();
    let (cfg, store) = load(&root);
    let books = store.load_all();
    if books.is_empty() {
        println!(
            "(no runbooks — create one with `runbook new <name>` in {})",
            cfg.project_dir
        );
        return;
    }
    for r in &books {
        let scope = if r.global { "global " } else { "project" };
        let desc = if r.meta.description.is_empty() {
            String::new()
        } else {
            format!("  — {}", r.meta.description)
        };
        let macro_name = format!("{}{}", cfg.prefix, r.name);
        println!("[{scope}] {macro_name:<22}{desc}");
    }
}

fn show(name: &str) {
    let root = cwd();
    let (_cfg, store) = load(&root);
    let books = store.load_all();
    let key = normalize_name(name);
    match books.iter().find(|r| r.matches(&key)) {
        Some(r) => {
            let scope = if r.global { "global" } else { "project" };
            println!("# {} ({})  {}\n", r.name, scope, r.path.display());
            println!("{}", r.body);
        }
        None => {
            eprintln!("no runbook named '{name}'. Try `runbook list`.");
            std::process::exit(1);
        }
    }
}

const TEMPLATE: &str = r#"+++
description = "%DESC%"
aliases = []
+++

# %NAME%

## Overview
<!-- この手順の目的と、いつ使うか -->

## Procedure
1.
2.
3.

## Specifications
<!-- 守るべき仕様・前提・完了条件 -->

## Forbidden Actions
<!-- やってはいけないこと（例: main に直接 push しない） -->
"#;

fn scaffold(name: &str, description: &str) -> String {
    TEMPLATE
        .replace("%DESC%", &description.replace('"', "'"))
        .replace("%NAME%", name)
}

fn new(name: &str, description: &str, global: bool, force: bool) -> anyhow::Result<()> {
    let root = cwd();
    let (_cfg, store) = load(&root);
    let slug = normalize_name(name);
    let dir = store.dir_for(global).to_path_buf();
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{slug}.md"));
    if path.exists() && !force {
        anyhow::bail!(
            "{} already exists (use --force to overwrite)",
            path.display()
        );
    }
    std::fs::write(&path, scaffold(&slug, description))?;
    println!("wrote {}", path.display());
    println!("Invoke it in any prompt with `!{slug}`. Edit the Procedure section to fill it in.");
    Ok(())
}

fn init() -> anyhow::Result<()> {
    let root = cwd();
    let (cfg, store) = load(&root);
    let dir = store.project_dir.clone();
    std::fs::create_dir_all(&dir)?;
    let sample = dir.join("example.md");
    if !sample.exists() {
        std::fs::write(
            &sample,
            "+++\ndescription = \"サンプル手順。`runbook rm` 相当はファイル削除で\"\naliases = [\"ex\"]\n+++\n\n\
             # example\n\n\
             ## Overview\nrunbook の動作確認用サンプル。プロンプトに `!example` と書くとこの手順が注入される。\n\n\
             ## Procedure\n1. `.runbook/` に `<name>.md` を作る（`runbook new <name>`）\n2. プロンプトで `!<name>` と呼び出す\n3. このサンプルは消してよい\n\n\
             ## Forbidden Actions\n- 秘密情報を手順ファイルに直接書かない\n",
        )?;
    }
    println!("runbook dir ready: {}", dir.display());
    println!("sample: {}", sample.display());
    println!(
        "Try a prompt containing `!example`, or `!{}` for the index.",
        cfg.index_token
    );
    Ok(())
}

/// Mask a display value as `<first-2-chars>***` (or `***` if too short to
/// leave a meaningful prefix).
///
/// `index_token` is *not* a secret/credential — it is just the macro trigger
/// string used to fire `!<index_token>` (see `config::Config::index_token`,
/// default `"runbooks"`). This masking exists solely because the user
/// explicitly requested it for `status()` output; it is not a security
/// control. Kept here as a single, reusable convention in case future status
/// fields need the same consistent masking.
fn mask_display(value: &str) -> String {
    let mut chars = value.chars();
    match (chars.next(), chars.next()) {
        (Some(a), Some(b)) => format!("{a}{b}***"),
        _ => "***".to_string(),
    }
}

fn status() {
    let root = cwd();
    let (cfg, store) = load(&root);
    let books = store.load_all();
    let src = if Config::project_path(&root).exists() {
        Config::project_path(&root)
    } else if Config::home_path().exists() {
        Config::home_path()
    } else {
        Path::new("(defaults — no config file)").to_path_buf()
    };
    println!("config:            {}", src.display());
    println!("enabled:           {}", cfg.enabled);
    println!("prefix:            {}", cfg.prefix);
    // index_token is not a secret (see mask_display's doc comment above); it
    // is masked here only because the user explicitly asked for masked
    // display in status() output.
    println!(
        "index_token:       {}{}",
        cfg.prefix,
        mask_display(&cfg.index_token)
    );
    println!("project dir:       {}", store.project_dir.display());
    println!("global dir:        {}", store.global_dir.display());
    println!("include_global:    {}", cfg.include_global);
    println!("max_chars:         {}", cfg.max_chars);
    println!("per_runbook_chars: {}", cfg.per_runbook_chars);
    println!("runbooks visible:  {}", books.len());
}

#[cfg(test)]
mod tests {
    use super::mask_display;

    #[test]
    fn mask_display_keeps_first_two_chars_then_stars() {
        assert_eq!(mask_display("runbooks"), "ru***");
    }

    #[test]
    fn mask_display_handles_short_values() {
        assert_eq!(mask_display("a"), "***");
        assert_eq!(mask_display(""), "***");
    }
}
