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
use super::{analyze, program_source, Analysis, RedirOp, RedirTarget, Value};
use crate::shape::{Facts, Target};
use harness_core::asklearn::UNRESOLVED_VERB;

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
                    name.rsplit('/').next().unwrap_or(name).to_string()
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
