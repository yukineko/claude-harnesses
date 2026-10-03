// テスト内の unwrap/expect/panic は意図的な assert であって fail-open ではないので許可する。
// production 側は workspace の [workspace.lints.clippy] で deny のまま。
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]
//! `jev` — the CLI face of the advisory-only jev wrapper.
//!
//! Three subcommands and no more. Each one is deliberately narrow, because
//! every added surface is another place a caller could try to make jev decide
//! something:
//!
//! - `jev check` — is jev usable? Three-valued, with distinct exit codes.
//! - `jev ask` — send a request read from stdin; print the answers.
//! - `jev ledger` — what has this machine spent?
//!
//! There is no subcommand that returns a pass/fail, and none that takes the
//! API key as an argument (see `availability::KEY_ENV`).

use clap::{Parser, Subcommand};
use jev::{Availability, Client, Config, CurlTransport, JevRequest};

#[derive(Parser)]
#[command(
    name = "jev",
    about = "Advisory-only wrapper for jev (TypeSafe AI System One). Disabled without TYPESAFE_API_KEY.",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Report whether jev is usable. Exit 0 configured, 1 observably absent,
    /// 10 undetermined. Both non-zero codes mean "do not use jev"; they are
    /// separate so a broken environment is never read as a deliberate opt-out.
    Check,
    /// Read a request body from stdin (`{"state":…,"questions":{…}}`) and
    /// print jev's answers as JSON. Without a configured key this performs no
    /// network call and exits non-zero.
    Ask {
        /// Override the model route for this call only.
        #[arg(long)]
        model: Option<String>,
    },
    /// Summarize the local usage ledger.
    Ledger,
}

fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Check => cmd_check(),
        Command::Ask { model } => cmd_ask(model),
        Command::Ledger => cmd_ledger(),
    }
}

fn load_config() -> Result<Config, String> {
    Config::load()
}

fn cmd_check() -> std::process::ExitCode {
    let availability = Availability::detect();
    let mut json = availability.to_json();
    // Config problems are reported alongside, not merged into, the
    // availability verdict: a broken config does not mean the key is absent.
    match load_config() {
        Ok(cfg) => {
            if let Some(obj) = json.as_object_mut() {
                obj.insert("model".into(), serde_json::json!(cfg.model));
                obj.insert(
                    "model_pinned".into(),
                    serde_json::json!(!cfg.model_is_unpinned()),
                );
                obj.insert("max_time_secs".into(), serde_json::json!(cfg.max_time_secs));
                obj.insert(
                    "ledger".into(),
                    serde_json::json!(cfg.resolved_ledger_path().map(|p| p.display().to_string())),
                );
            }
        }
        Err(e) => {
            if let Some(obj) = json.as_object_mut() {
                obj.insert("config_error".into(), serde_json::json!(e));
            }
        }
    }
    println!("{json}");
    exit_from(availability.exit_code())
}

fn cmd_ask(model_override: Option<String>) -> std::process::ExitCode {
    let mut cfg = match load_config() {
        Ok(c) => c,
        Err(e) => {
            // A config that exists but does not parse is not run around. The
            // operator wrote something; honouring a default instead would be
            // the silent substitution CLAUDE.md §4 forbids.
            eprintln!("jev: {e}");
            return exit_from(2);
        }
    };
    if let Some(m) = model_override {
        let m = m.trim().to_string();
        if !m.is_empty() {
            cfg.model = m;
        }
    }

    // Resolve availability BEFORE reading stdin, so an unconfigured
    // environment is reported without the caller's payload being consumed.
    let availability = Availability::detect();
    if !matches!(availability, Availability::Configured(_)) {
        println!("{}", availability.to_json());
        eprintln!(
            "jev: not called (no usable TYPESAFE_API_KEY). This is the default state; \
             jev is advisory and its absence changes nothing."
        );
        return exit_from(availability.exit_code());
    }

    let mut input = String::new();
    if let Err(e) = std::io::Read::read_to_string(&mut std::io::stdin(), &mut input) {
        eprintln!("jev: reading stdin: {e}");
        return exit_from(2);
    }
    let mut req: JevRequest = match serde_json::from_str::<serde_json::Value>(&input) {
        Ok(v) => match build_request(v, &cfg.model) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("jev: {e}");
                return exit_from(2);
            }
        },
        Err(e) => {
            eprintln!("jev: stdin is not JSON: {e}");
            return exit_from(2);
        }
    };
    req.model = cfg.model.clone();

    let client = Client::new(CurlTransport::new(), cfg);
    let outcome = client.ask(&req);
    if let Some(e) = &outcome.ledger_error {
        // Loud: a lost ledger write means spend stopped being observable.
        eprintln!("jev: WARNING ledger write failed: {e}");
    }
    match outcome.result.require() {
        harness_core::verdict::Required::Determined(resp) => {
            match serde_json::to_string_pretty(&resp) {
                Ok(s) => println!("{s}"),
                Err(e) => {
                    eprintln!("jev: encoding response: {e}");
                    return exit_from(2);
                }
            }
            exit_from(0)
        }
        harness_core::verdict::Required::Blocked(why) => {
            // "Did not answer" is never printed as an answer.
            eprintln!("jev: no answer: {why}");
            exit_from(10)
        }
    }
}

/// Accept `{"state":…,"questions":{…}}`, filling in the model from config.
fn build_request(v: serde_json::Value, model: &str) -> Result<JevRequest, String> {
    let obj = v.as_object().ok_or("stdin must be a JSON object")?;
    let state = obj
        .get("state")
        .cloned()
        .ok_or("stdin object needs a \"state\" field")?;
    let questions = obj
        .get("questions")
        .cloned()
        .ok_or("stdin object needs a \"questions\" field")?;
    let questions = serde_json::from_value(questions).map_err(|e| format!("questions: {e}"))?;
    Ok(JevRequest {
        state,
        model: model.to_string(),
        questions,
    })
}

fn cmd_ledger() -> std::process::ExitCode {
    let cfg = match load_config() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("jev: {e}");
            return exit_from(2);
        }
    };
    let Some(path) = cfg.resolved_ledger_path() else {
        eprintln!("jev: no ledger path (HOME is unset and JEV_LEDGER is not set)");
        return exit_from(2);
    };
    match jev::ledger::summarize(&path) {
        Ok(s) => {
            println!(
                "{}",
                serde_json::json!({
                    "ledger": path.display().to_string(),
                    "records": s.records,
                    "answered": s.answered,
                    "unanswered": s.unanswered,
                    "unparseable": s.unparseable,
                    "input_tokens": s.input_tokens,
                    "output_tokens": s.output_tokens,
                    "estimated_usd": s.estimated_usd,
                })
            );
            exit_from(0)
        }
        Err(e) => {
            eprintln!("jev: {e}");
            exit_from(2)
        }
    }
}

fn exit_from(code: i32) -> std::process::ExitCode {
    std::process::ExitCode::from(u8::try_from(code).unwrap_or(2))
}
