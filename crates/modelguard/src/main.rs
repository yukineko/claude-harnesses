//! modelguard — PreToolUse gate on `Agent|Task` subagent spawns.
//!
//! **STUB — NOT IMPLEMENTED. This binary currently ALLOWS EVERY SPAWN.**
//! It reads the payload and exits 0 with no output, which Claude Code reads
//! as an allow. It enforces nothing yet; the integration tests under `tests/`
//! are expected to be RED against it until the real gate is written.
//!
//! Intended behaviour (user ruling 2026-11-27, verbatim:
//! 「fableの使用は禁止、実装担当は今後つくるagentではsonnetにして、これはhookで強制して」):
//! * Rule A: deny any spawn whose `model` contains "fable" or "mythos".
//! * Rule B: implementer spawns (everything outside a fixed control list of
//!   subagent types) must use a Sonnet model.
//! * Unparseable payload / missing `tool_input` / non-string `model` deny.

fn gate() {
    // STUB: allow-all. Drains stdin and returns without a decision.
    let _ = harness_core::hook::read_stdin();
}

fn main() {
    let sub = std::env::args().nth(1).unwrap_or_default();
    match sub.as_str() {
        "gate" => harness_core::hook::run_pretooluse_guarded("modelguard", gate),
        _ => {
            eprintln!("usage: modelguard gate");
            std::process::exit(2);
        }
    }
}
