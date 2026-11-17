//! askrelay — the subagent Ask relay hooks (user ruling 2026-11-14,
//! 「sub agentのAskはmainで裁定する」). The queue format and operations are in
//! [`harness_core::relay`] (shared with blastguard, which enqueues).
//!
//! Subcommands:
//!
//! * `stop`          — the main-session Stop gate (HAS a verdict).
//! * `mark-executed` — PostToolUse / PostToolUseFailure on Bash.
//! * `mark-relayed`  — PostToolUse on SendMessage.
//! * `dismiss <id> --reason <text>` — record why an entry will not be re-run.
//!
//! None of these executes a command. The `cwd` stored in an entry is never
//! resolved, stat'ed or entered (user ruling 2026-11-14,
//! 「gateもconduktも絶対に勝手に他のworktreeを参照してはならない」).
//!
//! # Panic barriers
//!
//! `stop` is decision-bearing and runs under
//! `harness_core::gate::run::run_guarded` (a panic BLOCKS). `dismiss` is an
//! operator command whose exit status IS its result, so a panic exits non-zero
//! (Rust's default, exit 101) rather than being swallowed. The two `mark-*`
//! hooks decide nothing: a lost mark leaves the entry pending, which keeps the
//! Stop gate blocking (the restrictive side), and every failure is reported on
//! stderr, so they use `run_hook`. `run_guarded` would be the wrong protocol
//! there: it answers a panic with a Stop-style `{"decision":"block"}` line, and
//! PostToolUse has no such channel.

mod protection;

use harness_core::hook::{self, HookInput};
use harness_core::relay;
use std::process::exit;

const STOP_NAME: &str = "askrelay-stop";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match args.first().map(String::as_str) {
        // `run_hook` never returns — it exits 0 itself.
        Some("mark-executed") => hook::run_hook(mark_executed),
        Some("mark-relayed") => hook::run_hook(mark_relayed),
        Some("stop") => stop(),
        Some("dismiss") => dismiss(&args[1..]),
        Some("--protects") => {
            // The protection statement (backlog 3a8e3b73), readable without
            // a payload on stdin.
            let p = protection::PROTECTION;
            println!("PROTECTS: {}", p.protects);
            println!("AGAINST: {}", p.against);
            println!("GROUNDS: {}", p.grounds);
            0
        }
        Some("--version") | Some("-V") => {
            println!("askrelay {}", env!("CARGO_PKG_VERSION"));
            0
        }
        _ => {
            eprintln!(
                "usage: askrelay stop | mark-executed | mark-relayed | \
                 dismiss <id> --reason <text> | --protects | --version"
            );
            2
        }
    };
    exit(code);
}

fn bash_command(input: &HookInput) -> Option<String> {
    if input.tool_name != "Bash" {
        return None;
    }
    let command = input.tool_input.as_ref()?.get("command")?.as_str()?;
    if command.trim().is_empty() {
        return None;
    }
    Some(command.to_string())
}

/// Main-session Bash call finished (successfully or not): mark the matching
/// relay entry executed. A call made inside a subagent marks nothing — only
/// the main session adjudicates.
fn mark_executed() {
    let raw = hook::read_stdin();
    if raw.trim().is_empty() || hook::payload_is_subagent(&raw) {
        return;
    }
    let (Some(input), Ok(value)) = (
        HookInput::parse(&raw),
        serde_json::from_str::<serde_json::Value>(raw.trim()),
    ) else {
        eprintln!("askrelay mark-executed: unparseable payload; nothing marked");
        return;
    };
    let Some(command) = bash_command(&input) else {
        return;
    };
    let session = relay::session_of(&input.session_id);
    if let Err(e) = relay::mark_executed(
        &relay::relay_dir(),
        &session,
        &input.cwd,
        &command,
        relay::exit_code_of(&value),
    ) {
        eprintln!(
            "askrelay mark-executed: WARNING could not mark ({e}); the entry stays \
             pending and the Stop gate keeps listing it"
        );
    }
}

/// Main-session SendMessage: mark executed entries whose relay id the message
/// names. The whole `tool_input` is searched, so the id may sit in any field.
fn mark_relayed() {
    let raw = hook::read_stdin();
    if raw.trim().is_empty() || hook::payload_is_subagent(&raw) {
        return;
    }
    let Some(input) = HookInput::parse(&raw) else {
        eprintln!("askrelay mark-relayed: unparseable payload; nothing marked");
        return;
    };
    if input.tool_name != "SendMessage" {
        return;
    }
    let Some(tool_input) = input.tool_input.as_ref() else {
        return;
    };
    let text = tool_input.to_string();
    let session = relay::session_of(&input.session_id);
    if let Err(e) = relay::mark_relayed(&relay::relay_dir(), &session, &text) {
        eprintln!(
            "askrelay mark-relayed: WARNING could not mark ({e}); the entry stays \
             pending and the Stop gate keeps listing it"
        );
    }
}

/// The main-session Stop gate. Runs under the shared fail-closed Stop barrier
/// (`run_guarded`): a panic blocks. An unparseable payload from a hook (stdin
/// not a terminal) blocks too — the session could not be identified, so its
/// queue could not be read.
fn stop() -> i32 {
    let terminal = std::io::IsTerminal::is_terminal(&std::io::stdin());
    let raw = harness_core::gate::run::read_stop_payload_guarded(
        STOP_NAME,
        terminal,
        hook::try_read_stdin(),
    );
    let input = HookInput::parse(&raw);
    let stop_hook_active = input.as_ref().is_some_and(|h| h.stop_hook_active);
    harness_core::gate::run::run_guarded(STOP_NAME, terminal, stop_hook_active, move || {
        let Some(input) = input else {
            if terminal {
                eprintln!("askrelay stop: expects a Stop hook payload on stdin");
                exit(2);
            }
            println!(
                "{}",
                serde_json::json!({
                    "decision": "block",
                    "reason": "askrelay: the Stop payload could not be parsed, so this \
                               session's subagent Ask relay queue could not be checked; the stop \
                               is blocked (fail-closed)."
                })
            );
            exit(0);
        };
        let session = relay::session_of(&input.session_id);
        match relay::stop_outcome(&relay::relay_dir(), &session) {
            relay::StopOutcome::Block(reason) => println!(
                "{}",
                serde_json::json!({ "decision": "block", "reason": reason })
            ),
            relay::StopOutcome::Allow(Some(msg)) => {
                println!("{}", serde_json::json!({ "systemMessage": msg }))
            }
            relay::StopOutcome::Allow(None) => {}
        }
        exit(0)
    })
}

/// `askrelay dismiss <id> --reason <text>`. Exit 0 on success, 2 for a usage
/// error / blank reason / unknown id, 3 when a queue could not be read or
/// written.
fn dismiss(rest: &[String]) -> i32 {
    let mut id: Option<&str> = None;
    let mut reason: Option<&str> = None;
    let mut it = rest.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--reason" => match it.next() {
                Some(r) => reason = Some(r),
                None => {
                    eprintln!("askrelay dismiss: --reason needs a value");
                    return 2;
                }
            },
            other if id.is_none() && !other.starts_with("--") => id = Some(other),
            other => {
                eprintln!("askrelay dismiss: unexpected argument `{other}`");
                return 2;
            }
        }
    }
    let Some(id) = id else {
        eprintln!("usage: askrelay dismiss <id> --reason <text>");
        return 2;
    };
    let Some(reason) = reason else {
        eprintln!("askrelay dismiss: --reason <text> is required");
        return 2;
    };
    match relay::dismiss(&relay::relay_dir(), id, reason) {
        Ok(path) => {
            println!(
                "dismissed {id} ({}); the dismissal is recorded in the queue file and is no longer shown on Stop",
                path.display()
            );
            0
        }
        Err(relay::DismissError::EmptyReason) => {
            eprintln!("askrelay dismiss: the reason must not be blank");
            2
        }
        Err(relay::DismissError::UnknownId(id)) => {
            eprintln!("askrelay dismiss: no relay entry {id}");
            2
        }
        Err(relay::DismissError::Io(e)) => {
            eprintln!("askrelay dismiss: {e}");
            3
        }
    }
}
