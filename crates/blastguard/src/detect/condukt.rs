//! Backlog 12ba5845 (scout 2026-11-12 S1): granting autonomy is a human-only
//! decision.
//!
//! Two command shapes switch condukt's human gates off:
//!
//! * `condukt state autonomy-set on` durably writes the shared autonomy switch
//!   for the repo, which condukt, ctxrot and autoflow all read.
//! * a `HARNESS_AUTONOMOUS=…` / `CONDUKT_AUTONOMOUS=…` assignment prefixed to a
//!   `condukt` invocation (`CONDUKT_AUTONOMOUS=1 condukt policy answer …`,
//!   `env CONDUKT_AUTONOMOUS=1 condukt …`). The env beats the switch file
//!   (`harness_core::autonomy`), so that one invocation runs autonomous —
//!   `policy answer` then auto-answers the question instead of returning it to
//!   a human.
//!
//! Both resolve to Ask here. Whether that Ask reaches a human or becomes a
//! Deny is not decided in this module: `main.rs` (`harden_for_subagent`)
//! turns every Ask into a Deny when the payload comes from a subagent, so a
//! subagent cannot grant itself autonomy and an interactive session gets a
//! confirmation prompt.
//!
//! Matching is on the parsed command word and its argv tokens, never on raw
//! command text: `echo "condukt state autonomy-set on"` is an `echo`, and
//! nothing here sees it. Wrappers blastguard already unwraps (`env`, `sudo`,
//! `bash -c`, …) reach this function through the normal command-word
//! dispatch.
//!
//! The env-assignment check applies to every `condukt` subcommand, not only
//! `policy answer`: the assignment is what grants autonomy, whatever the
//! subcommand then does with it. It does NOT see an assignment made in an
//! earlier statement (`export CONDUKT_AUTONOMOUS=1; condukt …`) — that
//! assignment is a separate segment and this rule judges one segment.

use super::words::{first_shell_word, is_assignment, is_bare_redirect_op, is_redirect_token};
use super::{has_unresolvable_expansion, Decision};

/// Phrase every reason this module emits contains; `rule_id` keys on it.
pub(crate) const AUTONOMY_SELF_GRANT: &str = "switching autonomy on is a human-only decision";

/// Env variables whose value overrides the autonomy switch
/// (`harness_core::autonomy`: `HARNESS_AUTONOMOUS`, then the legacy
/// `CONDUKT_AUTONOMOUS`).
const AUTONOMY_ENV: [&str; 2] = ["HARNESS_AUTONOMOUS", "CONDUKT_AUTONOMOUS"];

/// Judge a `condukt` invocation.
///
/// `prefix` is every token of the segment before the `condukt` command word
/// (leading assignments and any exec wrappers with their own assignments);
/// `rest` is the argv after it.
///
/// * An autonomy env assignment in `prefix` whose value is not a literal
///   false spelling (`0`/`false`/`no`/`off`) or empty is Ask. A value holding
///   an unresolved `$`/backtick expansion is Ask, because it may be truthy.
/// * `state autonomy-set <mode>` is Ask unless `<mode>` is literally `off`,
///   or no mode is given and a `--help`/`-h` flag is present (clap prints
///   help and exits without writing). Any other missing, non-literal (`$X`),
///   or unrecognised mode is Ask: the gate cannot tell it is not `on`.
/// * A `condukt` subcommand or `state` action that is not literal text is
///   Ask, for the same reason: it may be `state autonomy-set on`.
/// * Everything else is Allow.
pub(super) fn analyze_condukt(prefix: &[&str], rest: &[&str]) -> Decision {
    if let Some(ask) = autonomy_env_prefix(prefix) {
        return ask;
    }
    let args = positional_args(rest);
    let Some(group_tok) = args.first() else {
        return Decision::Allow;
    };
    let Some(group) = literal(group_tok) else {
        return Decision::ask(format!(
            "condukt {group_tok}: the subcommand is not literal text, so blastguard cannot tell \
it is not `state autonomy-set on` — {AUTONOMY_SELF_GRANT}"
        ));
    };
    if group != "state" {
        return Decision::Allow;
    }
    let Some(action_tok) = args.get(1) else {
        return Decision::Allow;
    };
    let Some(action) = literal(action_tok) else {
        return Decision::ask(format!(
            "condukt state {action_tok}: the action is not literal text, so blastguard cannot \
tell it is not `autonomy-set on` — {AUTONOMY_SELF_GRANT}"
        ));
    };
    if action != "autonomy-set" {
        return Decision::Allow;
    }
    let modes = &args[2..];
    match modes {
        [only] if literal(only).as_deref() == Some("off") => Decision::Allow,
        [] if rest.iter().any(|t| *t == "--help" || *t == "-h") => Decision::Allow,
        _ => Decision::ask(format!(
            "`condukt state autonomy-set {}` would turn the shared autonomy switch on (or \
blastguard cannot tell that it would not) — {AUTONOMY_SELF_GRANT}; a subagent must not grant \
itself autonomy. Run it yourself if you intend it.",
            modes.join(" ")
        )),
    }
}

/// Ask when `prefix` assigns an autonomy env variable a value that is not a
/// literal false spelling (or empty).
fn autonomy_env_prefix(prefix: &[&str]) -> Option<Decision> {
    for tok in prefix {
        // `env` reads `"CONDUKT_AUTONOMOUS=1"` as an assignment after the
        // shell removes the quotes, so judge the unquoted word.
        let word = first_shell_word(tok).unwrap_or_else(|| (*tok).to_string());
        if !is_assignment(&word) {
            continue;
        }
        let Some((name, value)) = word.split_once('=') else {
            continue;
        };
        if !AUTONOMY_ENV.contains(&name) {
            continue;
        }
        let lowered = value.trim().to_ascii_lowercase();
        let literal_false = !has_unresolvable_expansion(tok)
            && matches!(lowered.as_str(), "" | "0" | "false" | "no" | "off");
        if !literal_false {
            return Some(Decision::ask(format!(
                "`{tok}` runs this condukt invocation in autonomous mode (the env overrides the \
autonomy switch, so human gates such as `policy answer` are answered without a human) — \
{AUTONOMY_SELF_GRANT}; a subagent must not grant itself autonomy. Run it yourself if you \
intend it."
            )));
        }
    }
    None
}

/// The argv tokens that are neither flags nor shell redirections (a bare
/// redirect operator also owns the token after it).
fn positional_args<'a>(rest: &[&'a str]) -> Vec<&'a str> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < rest.len() {
        let t = rest[i];
        if t.is_empty() {
            i += 1;
        } else if is_bare_redirect_op(t) {
            i += 2;
        } else if is_redirect_token(t) || t.starts_with('-') {
            i += 1;
        } else {
            out.push(t);
            i += 1;
        }
    }
    out
}

/// The unquoted literal text of `tok`, or `None` when it holds an expansion
/// or glob blastguard cannot resolve.
fn literal(tok: &str) -> Option<String> {
    if has_unresolvable_expansion(tok) || tok.contains(['(', ')', '*', '?', '[', '{']) {
        return None;
    }
    first_shell_word(tok)
}
