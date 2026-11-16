//! The declaration builtins of `detect::ast::eval` (`export` / `declare` /
//! `typeset` / `local` / `readonly`): which names get the integer attribute,
//! the arithmetic checks of their assignments and compound keys, and the
//! bindings they make. A child module of `eval.rs`, so it reads the
//! evaluator's state (`Env`, `DeclArg`) directly.

use super::super::{arith, Cause, Value, Word};
use super::{is_ident, DeclArg, Env, Ev};
use std::collections::{BTreeMap, BTreeSet};

impl Ev {
    pub(super) fn declaration(
        &mut self,
        builtin: &str,
        args: &[Word],
        decl_names: &BTreeMap<usize, DeclArg>,
        env: &mut Env,
    ) -> Result<(), Cause> {
        // An `i` in any option may give the integer attribute (superset); so
        // may an undetermined operand that is not an assignment (`declare
        // $opt n=v` with opt=-i): its value is not known not to be `-i`.
        // A nameref (`declare -n r=n r=v` assigns `n`) needs no rule here: a
        // name gets into `integer` only through an option or an undetermined
        // operand, both of which make the state opaque, so a target that may
        // be integer was declared while opaque and `start.opaque` holds.
        let int_attr = args.iter().enumerate().any(|(i, a)| match &a.value {
            Value::Literal(s) => s.starts_with('-') && s.contains('i'),
            Value::Undetermined(_) => !decl_names.contains_key(&(i + 1)),
        });
        // The keys of a compound value `a=([k]=v)` are arithmetic, evaluated
        // when that operand is assigned. An option of this command (which
        // makes `env` opaque below) changes only the names this command
        // declares, so a key reads the state from before the command for a
        // name not yet operated on here, and the current state otherwise —
        // and always once an undetermined operand or a key's own arithmetic
        // (which may assign) ran.
        let start = env.clone();
        let mut touched: BTreeSet<String> = BTreeSet::new();
        let mut current_only = false;
        for (i, a) in args.iter().enumerate() {
            let decl = decl_names.get(&(i + 1));
            for k in decl.map_or(&[][..], |d| d.keys.as_slice()) {
                let lookup = |n: &str| {
                    if current_only || touched.contains(n) {
                        env.lookup(n)
                    } else {
                        start.lookup(n)
                    }
                };
                arith::check_text(k, &lookup)?;
                if !arith::plain_int(&Value::Literal(k.clone())) {
                    env.havoc_vars("an array key's arithmetic may assign variables");
                    current_only = true;
                }
            }
            match &a.value {
                Value::Literal(s) if !s.starts_with('-') && !s.starts_with('+') => {
                    let lhs = s.split_once('=').map_or(s.as_str(), |(l, _)| l);
                    let lhs = lhs.strip_suffix('+').unwrap_or(lhs);
                    touched.insert(lhs.split('[').next().unwrap_or(lhs).to_string());
                }
                Value::Literal(_) => {}
                Value::Undetermined(_) => match decl.and_then(|d| d.name.as_ref()) {
                    Some(n) => {
                        touched.insert(n.clone());
                    }
                    None => current_only = true,
                },
            }
            let s = match &a.value {
                Value::Literal(s) => s,
                Value::Undetermined(_) => {
                    match decl_names.get(&(i + 1)).and_then(|d| d.name.as_ref()) {
                        Some(n) => {
                            if int_attr {
                                env.integer.insert(n.clone());
                            }
                            env.int_assign_if(start.opaque, n, &a.value)?;
                            env.forget(n, &format!("assigned by {builtin}"));
                        }
                        None => env.make_opaque(&format!("{builtin} with an undetermined operand")),
                    }
                    continue;
                }
            };
            if s.starts_with('-') || s.starts_with('+') {
                env.make_opaque(&format!("{builtin} with an option"));
                continue;
            }
            let name = s.split_once('=').map_or(s.as_str(), |(l, _)| l);
            if int_attr {
                let base = name.strip_suffix('+').unwrap_or(name);
                env.integer
                    .insert(base.split('[').next().unwrap_or(base).to_string());
            }
            match s.split_once('=') {
                Some((lhs, rhs)) => {
                    // `n+=v` under `declare -i` evaluates `n + v`.
                    let expr = match lhs.strip_suffix('+') {
                        Some(n) => format!("{n} + {rhs}"),
                        None => rhs.to_string(),
                    };
                    let target = lhs.strip_suffix('+').unwrap_or(lhs);
                    env.int_assign_if(start.opaque, target, &Value::Literal(expr))?;
                    if let Some(n) = lhs.strip_suffix('+') {
                        env.forget_operand(n, "+= appends to a value")?;
                    } else if decl.is_some_and(|d| d.compound) {
                        // `a=(x y)` sets an array; `$a` is not the text `(x y)`.
                        env.forget_operand(lhs, "compound assignment")?;
                    } else if is_ident(lhs) {
                        env.bind(lhs, Value::Literal(rhs.to_string()));
                    } else {
                        env.forget_operand(lhs, "indexed assignment")?;
                    }
                    if builtin == "readonly" {
                        env.readonly.insert(lhs.to_string());
                    }
                }
                None if s.contains('[') => env.forget_operand(s, "declared element")?,
                None => {
                    if builtin == "readonly" {
                        env.readonly.insert(s.clone());
                    }
                }
            }
        }
        Ok(())
    }
}
