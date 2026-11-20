//! `kill` aimed at pid `-1` (backlog e93d3701, spec 596ef57d Q5).
//!
//! `kill -9 -1` sends the signal to EVERY process the user may signal — the
//! user's whole session, editors, agents and this harness included. Standing
//! ruling "system harm always deny": that is Deny, not Ask.
//!
//! The rule is deliberately narrow (it runs on every Bash call; a false
//! positive is a bug). It fires only when a PID operand is literally `-1`
//! and the signal is not the literal probe signal `0`. It follows the
//! option grammar of bash's `kill` builtin (`builtins/kill.def`), which the
//! BSD/procps/util-linux `kill` programs agree with for these forms:
//!
//! - `-l` / `-L` / `--list` / `--table`: list mode, nothing is signalled.
//! - `-s SIG`, `-n NUM`, `--signal SIG`, `--signal=SIG`, `-sSIG`, `-nNUM`
//!   name the signal.
//! - the FIRST other `-WORD` before any signal was named is the signal
//!   (`-9`, `-KILL`, `-SIGKILL`, and `-1` = SIGHUP); once a signal is named a
//!   later `-N` is a pid (process group / all), which is how `kill -9 -1`
//!   reaches every process while bare `kill -1` is just "SIGHUP, no pid".
//! - `--` ends options; everything after it is a pid.
//! - the first non-option word ends options; every later word is a pid.
//!
//! An operand that is an expansion (`$pid`) is not this rule's business: it
//! is not literally `-1`, so the existing behaviour is left unchanged.

use super::words::{first_shell_word, is_bare_redirect_op, is_redirect_token};

/// The deny reason. Mapped to the `kill-all` rule id in `crate::rule_id`.
pub(super) const KILL_ALL_REASON: &str =
    "kill aimed at pid -1 signals every process the user owns (system harm)";

/// `Some(reason)` when `kill <rest>` signals pid `-1`; `None` otherwise.
pub(super) fn kill_all_processes(rest: &[&str]) -> Option<&'static str> {
    let words = operand_words(rest);
    let mut signal: Option<String> = None;
    let mut i = 0;
    // Option phase.
    while let Some(w) = words.get(i) {
        let w = w.as_str();
        match w {
            "-l" | "-L" | "--list" | "--table" => return None,
            "-s" | "-n" | "--signal" => {
                signal = Some(words.get(i + 1)?.clone());
                i += 2;
            }
            // util-linux `kill -q VALUE` / `--queue VALUE`: the value is data.
            "-q" | "--queue" => i += 2,
            "--" => {
                i += 1;
                break;
            }
            _ if w.starts_with("--signal=") => {
                signal = Some(w["--signal=".len()..].to_string());
                i += 1;
            }
            _ if w.starts_with("--list=") => return None,
            _ if w.starts_with("-s") && w[2..].starts_with(|c: char| c.is_ascii_alphabetic()) => {
                signal = Some(w[2..].to_string());
                i += 1;
            }
            _ if w.starts_with("-n") && w[2..].starts_with(|c: char| c.is_ascii_digit()) => {
                signal = Some(w[2..].to_string());
                i += 1;
            }
            _ if w.starts_with('-') && w.len() > 1 && signal.is_none() => {
                signal = Some(w[1..].to_string());
                i += 1;
            }
            _ => break,
        }
    }
    // Signal 0 only probes for existence; it delivers nothing.
    if signal.as_deref() == Some("0") {
        return None;
    }
    let pids = words.get(i..).unwrap_or(&[]);
    if pids.iter().any(|p| is_all_processes_pid(p)) {
        Some(KILL_ALL_REASON)
    } else {
        None
    }
}

/// `-1` as `kill` reads it numerically (`-1`, `-01`, `-001`).
fn is_all_processes_pid(word: &str) -> bool {
    word.strip_prefix('-')
        .is_some_and(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()))
        && word[1..].trim_start_matches('0') == "1"
}

/// The command's words with redirections removed and quotes resolved.
fn operand_words(rest: &[&str]) -> Vec<String> {
    let mut out = Vec::new();
    let mut skip_next = false;
    for tok in rest {
        if skip_next {
            skip_next = false;
            continue;
        }
        if is_bare_redirect_op(tok) {
            skip_next = true;
            continue;
        }
        if is_redirect_token(tok) {
            continue;
        }
        out.push(first_shell_word(tok).unwrap_or_else(|| (*tok).to_string()));
    }
    out
}
