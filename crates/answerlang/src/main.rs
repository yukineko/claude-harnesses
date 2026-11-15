//! answerlang — keeps answers and conclusions in the user's language.
//!
//! * layer 2 (`session-start`, `prompt`): inject the target language into
//!   `additionalContext`;
//! * layer 3 (`stop`): block a Stop whose final answer is written in another
//!   script (bounded re-entry; panics fail closed via `run_guarded`);
//! * `set` / `show`: manage `~/.answerlang/config.toml`.
//!
//! Layer 1 (Claude Code's own settings.json `"language"` key) is not part of
//! this crate. See README.md for the full contract.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

mod cli;
mod config;
mod detect;
mod hooks;
mod lang;
mod measure;
mod state;
mod transcript;

const USAGE: &str = "usage: answerlang <session-start|prompt|stop|set|show>\n  set [--language <name|tag|auto>] [--inject on|off] [--enforce on|off] [--enabled on|off]\n  show";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match args.first().map(String::as_str) {
        Some("session-start") => hooks::inject(hooks::InjectEvent::SessionStart),
        Some("prompt") => hooks::inject(hooks::InjectEvent::UserPromptSubmit),
        Some("stop") => hooks::stop(),
        Some("set") => cli::set(&args[1..]),
        Some("show") => cli::show(),
        Some("--version") | Some("version") => {
            println!("answerlang {}", env!("CARGO_PKG_VERSION"));
            0
        }
        Some("-h") | Some("--help") | Some("help") => {
            println!("{USAGE}");
            0
        }
        _ => {
            eprintln!("{USAGE}");
            2
        }
    };
    std::process::exit(code)
}
