//! The learning-shape facts of a Bash command line (spec
//! `docs/specs/blastguard-ask-learning.md`), read from the SAME evaluated
//! [`super::Script`] the detectors judge: the command words of every simple
//! command, and the write targets the evaluator resolved (file redirects and
//! the Table B B.1 target rows). Nothing here parses the command text.
//!
//! An unresolved value is [`Target::Undetermined`] / the unresolved verb —
//! never dropped, never read as "no target" (CLAUDE.md §3).

use super::eval::resolve_at;
use super::judge::table_b::{extract, Row};
use super::{
    analyze, program_source, Analysis, ProgramSource, RedirOp, RedirTarget, SimpleCommand,
    StdinSource, Value,
};
use crate::shape::{Facts, Target};
use harness_core::asklearn::UNRESOLVED_VERB;
use std::collections::BTreeSet;

fn target_of(v: Value) -> Target {
    match v {
        Value::Literal(s) if !s.is_empty() => Target::Path(s),
        Value::Literal(_) | Value::Undetermined(_) => Target::Undetermined,
    }
}

/// The facts of `command`, or `None` when its evaluation is `Undetermined`
/// (the caller then uses [`crate::shape::undetermined_facts`]).
pub(crate) fn shape_facts(command: &str) -> Option<Facts> {
    let Analysis::Known(script) = analyze(command) else {
        return None;
    };
    let mut facts = Facts::default();
    for cmd in &script.commands {
        if let Some(first) = cmd.argv.first() {
            facts.verbs.push(match &first.value {
                Value::Literal(name) if !name.is_empty() => {
                    verb_token(cmd, name.rsplit('/').next().unwrap_or(name))
                }
                _ => UNRESOLVED_VERB.to_string(),
            });
        }
        for r in &cmd.redirects {
            if matches!(
                r.op,
                RedirOp::Out
                    | RedirOp::Append
                    | RedirOp::Clobber
                    | RedirOp::InOut
                    | RedirOp::OutErr
                    | RedirOp::AppendOutErr
            ) {
                if let RedirTarget::Path(v) = &r.target {
                    facts.targets.push(target_of(v.clone()));
                }
            }
        }
        match extract(cmd) {
            Row::None => {}
            Row::Undetermined(_) => facts.targets.push(Target::Undetermined),
            Row::Targets(pairs) => {
                for (_, v) in pairs {
                    facts.targets.push(target_of(resolve_at(&cmd.cwd, v)));
                }
            }
        }
        // A shell / interpreter / wrapper runs a program whose own targets
        // this evaluation does not list.
        if program_source(cmd).is_some() {
            facts.targets.push(Target::Undetermined);
        }
    }
    Some(facts)
}

/// Multi-command tools: the first non-option operand names what is run
/// (`git push`, `cargo test`, `docker rm`). Without it `git status` and
/// `git push --force` would share a shape (user ruling R1, 2026-11-28).
const SUBCOMMAND_TOOLS: &[&str] = &[
    "git",
    "cargo",
    "gh",
    "docker",
    "podman",
    "kubectl",
    "helm",
    "npm",
    "npx",
    "pnpm",
    "yarn",
    "bun",
    "deno",
    "pip",
    "pip3",
    "uv",
    "brew",
    "systemctl",
    "launchctl",
    "terraform",
    "go",
    "rustup",
    "apt",
    "apt-get",
    "dnf",
    "aws",
    "gcloud",
    "az",
    "condukt",
    "overwatch",
    "backlog",
];

/// Tools whose SECOND operand is part of the verb too (`gh pr merge`,
/// `docker compose down`, `npm run x`): the first operand alone is a group.
const TWO_LEVEL_TOOLS: &[&str] = &[
    "gh",
    "docker",
    "podman",
    "kubectl",
    "helm",
    "npm",
    "pnpm",
    "yarn",
    "bun",
    "aws",
    "gcloud",
    "az",
    "terraform",
    "launchctl",
    "condukt",
    "overwatch",
    "backlog",
];

/// `git` subcommands that are themselves groups (`git stash drop`).
const GIT_GROUPS: &[&str] = &[
    "remote",
    "stash",
    "worktree",
    "submodule",
    "notes",
    "reflog",
    "bisect",
    "sparse-checkout",
    "config",
];

fn short_cluster(s: &str, flags: &mut BTreeSet<String>) {
    for c in s.chars().take_while(|c| *c != '=') {
        flags.insert(format!("-{c}"));
    }
}

/// The verb of one simple command (spec R1/R2, user rulings 2026-11-28):
///
/// `basename [subcommand..] [option..] [#body-hash]`, space separated.
///
/// * every OPTION token is part of the verb, normalised: a short cluster is
///   split (`-rf` -> `-f -r`), a long option loses its `=value`, the set is
///   sorted. A finer key is always safe; a coarser one would let an approval
///   of `git branch -d` cover `git branch -D`. Operands that are not a
///   subcommand (paths, refs) are NOT in the verb, so `rm a` and `rm b` still
///   share a shape. An operand the evaluator could not resolve adds nothing
///   (`rm "$f"` is `rm`): that is the pre-existing, pinned shape of such a
///   call.
/// * an inline interpreter program (`-c` / `-e` body, a literal heredoc or
///   here-string on stdin) adds `#<fnv1a64 of the body>`; a program the
///   evaluator cannot pin (wrapper, `eval`, piped from a command) adds the
///   unresolved token `?`, which makes the shape never learnable as Allow.
fn verb_token(cmd: &SimpleCommand, base: &str) -> String {
    let mut flags: BTreeSet<String> = BTreeSet::new();
    let mut positionals: Vec<&str> = Vec::new();
    let mut option_before_first_operand = false;
    let mut options_ended = false;
    for a in cmd.argv.iter().skip(1) {
        let Value::Literal(s) = &a.value else {
            continue;
        };
        if !options_ended && s == "--" {
            options_ended = true;
        } else if !options_ended && s.starts_with("--") {
            flags.insert(s.split('=').next().unwrap_or(s).to_string());
            option_before_first_operand |= positionals.is_empty();
        } else if !options_ended && s.starts_with('-') && s.len() > 1 {
            short_cluster(&s[1..], &mut flags);
            option_before_first_operand |= positionals.is_empty();
        } else {
            positionals.push(s.as_str());
        }
    }
    let mut parts: Vec<String> = vec![base.to_string()];
    if SUBCOMMAND_TOOLS.contains(&base) {
        let first = positionals.first().copied().unwrap_or("");
        let mut depth = 1;
        // A global option before the subcommand (`git -C dir push`) may have
        // consumed an operand: take one more so the subcommand is included.
        if option_before_first_operand || first.starts_with('+') {
            depth += 1;
        }
        if TWO_LEVEL_TOOLS.contains(&base) || (base == "git" && GIT_GROUPS.contains(&first)) {
            depth += 1;
        }
        parts.extend(positionals.iter().take(depth).map(|p| p.to_string()));
    }
    parts.extend(flags);
    match program_source(cmd) {
        Some(ProgramSource::InlineLiteral(body))
        | Some(ProgramSource::Stdin(StdinSource::Literal(body))) => {
            parts.push(format!(
                "#{:016x}",
                harness_core::hash::fnv1a64(body.as_bytes())
            ));
        }
        Some(ProgramSource::File(_)) | Some(ProgramSource::Stdin(StdinSource::File(_))) | None => {}
        Some(_) => parts.push(UNRESOLVED_VERB.to_string()),
    }
    parts.join(" ")
}
