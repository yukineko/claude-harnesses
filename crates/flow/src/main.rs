//! `flow` — binary half of the flow plugin.
//!
//! Subcommands:
//! * `stop-gate` — Stop hook keeping a registered /flow driver session running
//!   while backlog work remains (see `stop_gate`).
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

mod decide;
mod probes;
mod stop_gate;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("stop-gate") => stop_gate::main(),
        Some("--version") | Some("-V") => {
            println!("flow {}", env!("CARGO_PKG_VERSION"));
        }
        _ => {
            eprintln!("usage: flow stop-gate   (Stop hook; reads the Stop payload on stdin)");
            std::process::exit(2);
        }
    }
}
