//! Word-level helpers of `detect::ast::eval`: the value of a parameter
//! expansion (`param`, with the `${n:=v}` integer check in `assign_default`)
//! and the final value of an expanded word after the checks of its mode
//! (`finish`: tilde, word splitting, pathname / brace expansion, heredoc
//! expansions). A child module of `eval.rs`, so it reads the evaluator's
//! state (`Env`, `Acc`) directly.

use super::super::{arith, has_subst, Cause, Value};
use super::{undet, Acc, Env, Mode, Seg};
use brush_parser::word::{Parameter, ParameterExpr};

pub(super) fn param(expr: &ParameterExpr, praw: &str, env: &mut Env) -> Result<Value, Cause> {
    match expr {
        ParameterExpr::Parameter {
            parameter: Parameter::Named(n),
            indirect: false,
        } => Ok(env.lookup(n)),
        ParameterExpr::Parameter {
            parameter: Parameter::Positional(_) | Parameter::Special(_),
            indirect: false,
        } => Ok(undet(format!("{praw}: positional / special parameter"))),
        _ => {
            if has_subst(praw) {
                return Err(Cause::Unsupported(format!(
                    "command substitution inside the parameter operation {praw}"
                )));
            }
            arith::check_param(expr, &|n| env.lookup(n))?;
            assign_default(expr, env)?;
            if praw.contains('=') || praw.contains('[') {
                env.havoc_vars("a parameter operation may assign variables");
            }
            Ok(undet(format!("{praw}: parameter operation not evaluated")))
        }
    }
}

/// `${n:=v}` / `${n=v}` assign `v` (the raw word: a `$` in it is not
/// determined) to `n`: arithmetic if `n` may be integer. An indirect
/// `${!x:=v}` (or any target this does not name) may assign any name, so it
/// counts as integer while the state is opaque or any name is `declare -i`.
fn assign_default(expr: &ParameterExpr, env: &Env) -> Result<(), Cause> {
    let ParameterExpr::AssignDefaultValues {
        parameter,
        indirect,
        default_value,
        ..
    } = expr
    else {
        return Ok(());
    };
    let v = Value::Literal(default_value.clone().unwrap_or_default());
    match parameter {
        Parameter::Named(n) | Parameter::NamedWithIndex { name: n, .. } if !*indirect => {
            env.int_assign(n, &v)
        }
        _ => env.int_assign_if(env.opaque || !env.integer.is_empty(), "", &v),
    }
}

/// The value of an expanded word, after the checks of its `mode`.
pub(super) fn finish(acc: Acc, mode: Mode, env: &Env) -> Value {
    if let Some(r) = acc.undet {
        return undet(r);
    }
    let full: String = acc.segs.iter().map(|(s, _)| s.as_str()).collect();
    let unquoted_lits = || {
        acc.segs
            .iter()
            .filter(|(_, k)| *k == Some(Seg::UnquotedLit))
            .map(|(s, _)| s.as_str())
    };
    let starts_with_tilde = matches!(acc.segs.first(),
        Some((s, Some(Seg::UnquotedLit))) if s.starts_with('~'));
    // Bash also tilde-expands after `=` / `:` in an argv word shaped like an
    // assignment (`of=~/f`, `c=x:~/y`), so Field treats those like Assign
    // (also when the `=` / `:` ends the previous segment).
    let tilde = starts_with_tilde
        || unquoted_lits().any(|s| s.contains(":~") || s.contains("=~"))
        || acc.segs.windows(2).any(|w| {
            w[0].0.ends_with(['=', ':'])
                && matches!(&w[1], (s, Some(Seg::UnquotedLit)) if s.starts_with('~'))
        });
    if tilde && mode != Mode::Heredoc {
        return undet("~ expands to a home directory at run time");
    }
    match mode {
        Mode::Heredoc => {
            if acc.expansion {
                return undet("unquoted heredoc holds expansions");
            }
        }
        Mode::Assign => {}
        Mode::Field => {
            let ifs: Option<String> = if env.opaque {
                None
            } else {
                match env.vars.get("IFS") {
                    None => Some(" \t\n".to_string()),
                    Some(Value::Literal(v)) => Some(v.clone()),
                    Some(Value::Undetermined(_)) => None,
                }
            };
            for (s, k) in &acc.segs {
                if *k != Some(Seg::UnquotedExp) {
                    continue;
                }
                match &ifs {
                    None => return undet("unquoted expansion with an undetermined IFS"),
                    Some(ifs) if s.chars().any(|c| ifs.contains(c)) => {
                        return undet(format!("unquoted expansion `{s}` is word-split"));
                    }
                    _ => {}
                }
                if s.contains(['*', '?', '[']) {
                    return undet(format!("unquoted expansion `{s}` is pathname-expanded"));
                }
            }
            for s in unquoted_lits() {
                if s.contains(['*', '?', '(']) || (s.contains('[') && full.contains(']')) {
                    return undet(format!("`{full}` is a glob pattern"));
                }
                if s.contains('{')
                    && full.contains('}')
                    && (full.contains(',') || full.contains(".."))
                {
                    return undet(format!("`{full}` is brace-expanded"));
                }
            }
            if full.is_empty() && !acc.any_quote() {
                return undet("an empty unquoted expansion removes the word");
            }
        }
    }
    Value::Literal(full)
}
