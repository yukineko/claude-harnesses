//! Arithmetic contexts of `detect::ast::eval` (user ruling 2026-11-16).
//!
//! Bash evaluates subscripts and arithmetic recursively: a name read in an
//! arithmetic expression is replaced by its value, which is evaluated as an
//! expression in turn, and a subscript met on the way is expanded — so
//! `x='a[$(cmd)]'; (( x ))` runs `cmd`. A value that comes from the
//! environment (not from the command text) in an arithmetic context could
//! therefore run a command the [`super::Script`] would not list.
//!
//! An arithmetic context is *determined* only when every name / `$x` it reads
//! has a `Literal` value assigned on the line whose own evaluation is
//! determined, checked recursively up to [`MAX_DEPTH`] values deep and within
//! [`MAX_STEPS`] tokens. Anything else makes the whole analysis
//! `Undetermined(Unsupported("arithmetic on a value not determined by the
//! text: ..."))` — there is no "probably harmless" case.
//!
//! The contexts the evaluator checks: the subscript of a builtin's name
//! operand (`read 'a[i]'`, `unset`, `printf -v`, the `declare` family) and of
//! an assignment (`a[i]=v`, the keys of `a=([i]=v)`), `$(( ))` / `$[ ]`,
//! `(( ))`, arithmetic `for`, the operands of `[[ a -eq b ]]` (and the other
//! arithmetic comparisons) and of `[[ -v a[i] ]]`, the operands of `let`,
//! assignments to a name with the `declare -i` attribute, and in a parameter
//! expansion the offset / length of `${x:off:len}`, the index of `${a[i]}` and
//! the name `${!x}` refers to.
//!
//! What an expression may hold: decimal / based numbers, names, operators and
//! whitespace; in text the shell expands first (raw `$(( ))`, `(( ))`,
//! `for (( ))`, subscripts, offsets) also `$name` / `${name}`, whose literal
//! values are spliced into the text before it is checked, as bash does. A
//! name directly followed by `=` (not `==`) is assigned, not read, so its old
//! value is not evaluated. Everything else — quotes, backslashes, other `$`
//! forms, positional / special parameters, reading an array element `a[i]`
//! (arrays are not tracked) — is undetermined.
//!
//! This module only decides whether the text determines the evaluation; the
//! caller still models the side effects (an arithmetic expression may assign,
//! so it makes the tracked variables `Undetermined`).

use super::{has_subst, Cause, Value};
use brush_parser::word::{Parameter, ParameterExpr};

/// Values evaluated inside values (`x=y; y=z; (( x ))` is depth 2) before
/// giving up.
const MAX_DEPTH: usize = 8;
/// Tokens visited in one check (all depths together) before giving up.
const MAX_STEPS: usize = 4096;

/// The value of a variable at the point of evaluation (`Env::lookup`).
pub(super) type Lookup<'a> = &'a dyn Fn(&str) -> Value;

fn undetermined(what: &str, why: &str) -> Cause {
    Cause::Unsupported(format!(
        "arithmetic on a value not determined by the text: `{what}`: {why}"
    ))
}

/// Raw arithmetic text that the shell expands before evaluating it (`$(( ))`,
/// `(( ))`, `for (( ))`, a subscript, `${x:off:len}`).
pub(super) fn check_text(expr: &str, lookup: Lookup) -> Result<(), Cause> {
    if has_subst(expr) {
        return Err(Cause::Unsupported(format!(
            "command substitution inside arithmetic `{expr}`"
        )));
    }
    let mut steps = 0;
    expand(expr, lookup)
        .and_then(|e| walk(&e, lookup, MAX_DEPTH, &mut steps))
        .map_err(|why| undetermined(expr, &why))
}

/// An already expanded value that the shell evaluates as arithmetic (an
/// operand of `let` / `[[ a -eq b ]]`, a key of `a=([k]=v)`, a value assigned
/// to a `declare -i` name).
pub(super) fn check_value(v: &Value, lookup: Lookup) -> Result<(), Cause> {
    match v {
        Value::Literal(s) => {
            let mut steps = 0;
            walk(s, lookup, MAX_DEPTH, &mut steps).map_err(|why| undetermined(s, &why))
        }
        Value::Undetermined(r) => Err(undetermined("<a run-time value>", r)),
    }
}

/// A builtin's name operand (`read`, `unset`, `printf -v`, `declare`, ...):
/// a `$( )` / `${` in its subscript is refused outright ([`subscript_subst`]);
/// otherwise the subscript, which the shell expands and evaluates as
/// arithmetic, must be determined. Every `[` counts, also in an operand that
/// only looks like a name (`read -p 'go [y/n]'`): which operand is a name is
/// not modelled.
pub(super) fn check_operand(operand: &str, lookup: Lookup) -> Result<(), Cause> {
    subscript_subst(operand)?;
    let Some((_, rest)) = operand.split_once('[') else {
        return Ok(());
    };
    check_text(rest.strip_suffix(']').unwrap_or(rest), lookup)
}

/// The arithmetic inside a parameter expansion: the index of `${a[i]}` (in
/// every operation), the offset / length of `${x:off:len}`, and for `${!x}`
/// the subscript of the name `x` holds (a run-time `x` is undetermined).
pub(super) fn check_param(expr: &ParameterExpr, lookup: Lookup) -> Result<(), Cause> {
    use ParameterExpr as P;
    let (parameter, indirect) = match expr {
        P::Parameter {
            parameter,
            indirect,
        }
        | P::UseDefaultValues {
            parameter,
            indirect,
            ..
        }
        | P::AssignDefaultValues {
            parameter,
            indirect,
            ..
        }
        | P::IndicateErrorIfNullOrUnset {
            parameter,
            indirect,
            ..
        }
        | P::UseAlternativeValue {
            parameter,
            indirect,
            ..
        }
        | P::ParameterLength {
            parameter,
            indirect,
        }
        | P::RemoveSmallestSuffixPattern {
            parameter,
            indirect,
            ..
        }
        | P::RemoveLargestSuffixPattern {
            parameter,
            indirect,
            ..
        }
        | P::RemoveSmallestPrefixPattern {
            parameter,
            indirect,
            ..
        }
        | P::RemoveLargestPrefixPattern {
            parameter,
            indirect,
            ..
        }
        | P::Substring {
            parameter,
            indirect,
            ..
        }
        | P::Transform {
            parameter,
            indirect,
            ..
        }
        | P::UppercaseFirstChar {
            parameter,
            indirect,
            ..
        }
        | P::UppercasePattern {
            parameter,
            indirect,
            ..
        }
        | P::LowercaseFirstChar {
            parameter,
            indirect,
            ..
        }
        | P::LowercasePattern {
            parameter,
            indirect,
            ..
        }
        | P::ReplaceSubstring {
            parameter,
            indirect,
            ..
        } => (parameter, *indirect),
        P::VariableNames { .. } | P::MemberKeys { .. } => return Ok(()),
    };
    if let Parameter::NamedWithIndex { index, .. } = parameter {
        check_text(index, lookup)?;
    }
    if let P::Substring { offset, length, .. } = expr {
        check_text(&offset.value, lookup)?;
        if let Some(l) = length {
            check_text(&l.value, lookup)?;
        }
    }
    if indirect {
        let target = match parameter {
            Parameter::Named(n) | Parameter::NamedWithIndex { name: n, .. } => lookup(n),
            _ => Value::Undetermined("${!..} of a positional / special parameter".into()),
        };
        match &target {
            Value::Literal(s) => {
                if let Some((_, rest)) = s.split_once('[') {
                    let idx = rest.strip_suffix(']').unwrap_or(rest);
                    check_value(&Value::Literal(idx.to_string()), lookup)?;
                }
            }
            Value::Undetermined(r) => {
                return Err(undetermined(
                    "${!..}",
                    &format!("the name it refers to: {r}"),
                ))
            }
        }
    }
    Ok(())
}

/// Splice the literal values of `$name` / `${name}` into `expr`; any other
/// `$` form, or a value not determined, is an error.
fn expand(expr: &str, lookup: Lookup) -> Result<String, String> {
    let mut out = String::new();
    let mut rest = expr;
    while let Some(i) = rest.find('$') {
        out.push_str(rest.get(..i).unwrap_or(""));
        let after = rest.get(i + 1..).unwrap_or("");
        let (name, len) = match after.strip_prefix('{') {
            Some(b) => {
                let end = b.find('}').ok_or("unterminated ${")?;
                (b.get(..end).unwrap_or(""), end + 2)
            }
            None => {
                let end = after
                    .find(|c: char| !(c == '_' || c.is_ascii_alphanumeric()))
                    .unwrap_or(after.len());
                (after.get(..end).unwrap_or(""), end)
            }
        };
        if !is_ident(name) {
            return Err(format!("`${name}` is not a plain variable"));
        }
        match lookup(name) {
            Value::Literal(v) => out.push_str(&v),
            Value::Undetermined(r) => return Err(r),
        }
        rest = after.get(len..).unwrap_or("");
    }
    out.push_str(rest);
    Ok(out)
}

fn skip(b: &[u8], mut i: usize, pred: impl Fn(u8) -> bool) -> usize {
    while b.get(i).is_some_and(|&c| pred(c)) {
        i += 1;
    }
    i
}

/// The index of the `]` that closes the `[` at `open`.
fn matching_bracket(b: &[u8], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (k, &c) in b.iter().enumerate().skip(open) {
        match c {
            b'[' => depth += 1,
            b']' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(k);
                }
            }
            _ => {}
        }
    }
    None
}

/// Check an (already expanded) expression; `Err` says why it is not
/// determined.
fn walk(expr: &str, lookup: Lookup, depth: usize, steps: &mut usize) -> Result<(), String> {
    let b = expr.as_bytes();
    let mut i = 0;
    while let Some(&c) = b.get(i) {
        *steps += 1;
        if *steps > MAX_STEPS {
            return Err("expression too large to check".into());
        }
        if c.is_ascii_digit() {
            i = skip(b, i, |c| {
                c.is_ascii_alphanumeric() || matches!(c, b'_' | b'#' | b'@')
            });
        } else if c.is_ascii_alphabetic() || c == b'_' {
            let end = skip(b, i, |c| c.is_ascii_alphanumeric() || c == b'_');
            let name = expr.get(i..end).unwrap_or("");
            let mut j = end;
            let element = b.get(j) == Some(&b'[');
            if element {
                let close = matching_bracket(b, j).ok_or("unterminated subscript")?;
                walk(expr.get(j + 1..close).unwrap_or(""), lookup, depth, steps)?;
                j = close + 1;
            }
            let k = skip(b, j, |c| matches!(c, b' ' | b'\t' | b'\n'));
            let assigned = b.get(k) == Some(&b'=') && b.get(k + 1) != Some(&b'=');
            if !assigned {
                if element {
                    return Err(format!(
                        "reads an element of the array {name} (not tracked)"
                    ));
                }
                read(name, lookup, depth, steps)?;
            }
            i = j;
        } else if b" \t\n+-*/%<>=!&|^~?:,()".contains(&c) {
            i += 1;
        } else {
            return Err(format!(
                "`{}` in an arithmetic expression is not modelled",
                c as char
            ));
        }
    }
    Ok(())
}

/// A name read in an expression: its value is evaluated as an expression.
fn read(name: &str, lookup: Lookup, depth: usize, steps: &mut usize) -> Result<(), String> {
    match lookup(name) {
        Value::Literal(v) => {
            let d = depth
                .checked_sub(1)
                .ok_or_else(|| format!("the value of {name} nests too deep"))?;
            walk(&v, lookup, d, steps)
        }
        Value::Undetermined(r) => Err(r),
    }
}

fn is_ident(s: &str) -> bool {
    let mut c = s.chars();
    matches!(c.next(), Some(ch) if ch == '_' || ch.is_ascii_alphabetic())
        && c.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

/// `[[ a OP b ]]` operators whose operands bash evaluates as arithmetic.
pub(super) fn arith_test_op(op: &brush_parser::ast::BinaryPredicate) -> bool {
    use brush_parser::ast::BinaryPredicate as P;
    matches!(
        op,
        P::ArithmeticEqualTo
            | P::ArithmeticNotEqualTo
            | P::ArithmeticLessThan
            | P::ArithmeticLessThanOrEqualTo
            | P::ArithmeticGreaterThan
            | P::ArithmeticGreaterThanOrEqualTo
    )
}

/// A literal decimal integer: evaluating it as arithmetic has no side effect.
/// Anything else (a name, `x=1`, `x++`, an undetermined value) may assign.
pub(super) fn plain_int(v: &Value) -> bool {
    let Value::Literal(s) = v else { return false };
    let d = s.trim();
    let d = d.strip_prefix(['-', '+']).unwrap_or(d);
    !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit())
}

/// Whether a builtin's name operand (`read`, `printf -v`, `unset`, `declare`
/// family, ...) carries a subscript the shell evaluates as arithmetic that
/// may assign variables: `read 'a[x=8]'` sets `x` (bash and zsh). Only no
/// subscript or exactly `name[<decimal digits>]` is side-effect free; any
/// other `[` (including text that only looks like an operand) counts as
/// possibly assigning.
pub(super) fn subscript_may_assign(operand: &str) -> bool {
    let Some((_, rest)) = operand.split_once('[') else {
        return false;
    };
    let Some(idx) = rest.strip_suffix(']') else {
        return true;
    };
    idx.is_empty() || !idx.bytes().all(|b| b.is_ascii_digit())
}

/// A builtin's name operand whose subscript holds `$( )`, `$(( ))`, `$[ ]`,
/// `${ }`, a backquote or `<( )` / `>( )`: the shell expands it when it
/// evaluates the subscript, so `read 'a[$(cmd)]'` runs `cmd` (bash and zsh;
/// `unset`, `declare`, `printf -v` in bash). The command would be invisible,
/// so the whole analysis is `Undetermined(Unsupported)` — the same as a
/// prefix assignment `a[$(cmd)]=v`.
pub(super) fn subscript_subst(operand: &str) -> Result<(), Cause> {
    let Some((_, idx)) = operand.split_once('[') else {
        return Ok(());
    };
    if has_subst(idx) || idx.contains("${") {
        return Err(Cause::Unsupported(format!(
            "command substitution in the subscript of {operand}"
        )));
    }
    Ok(())
}
