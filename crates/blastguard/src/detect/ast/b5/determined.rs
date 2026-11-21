//! The determined-program readings of `b5` (backlogs 40dc52c0 / 28019c35 /
//! d766b5fe), moved out of `b5.rs` unchanged (file-growth split).

use super::*;

/// The program `cmd` (argv from `k`) reads when [`literal_delivered_program`]
/// found no literal producer but the delivering text is a producer pipeline
/// whose output `payload/determined_output.rs` computes beyond a single plain
/// `echo` (backlog 40dc52c0): the process substitution operand `<(…)`, the
/// only stdin redirect `< <(…)`, or the pipeline stages before `cmd`
/// (`echo hi | cat | bash`). That computed output is the program, read by
/// the caller as a literal body.
///
/// `None` unless BOTH the user's top-level line and every enclosing derived
/// text ([`crate::detect::readings::chain_names_fixed`], only ever narrowed
/// by a `b5` / S5 re-entry on derived text, never widened) AND `text` itself
/// leave the modelled names unrebound
/// (`determined_output::resolution_fixed`), and unless the delivering text
/// is exactly such a pipeline. Every other shape keeps its Undetermined.
pub(super) fn determined_delivered_program(
    text: &str,
    commands: &[SimpleCommand],
    idx: usize,
    cmd: &SimpleCommand,
    source: &ProgramSource,
    k: usize,
) -> Option<String> {
    use crate::detect::payload::determined_output::{pipeline_output, resolution_fixed};
    if !crate::detect::readings::chain_names_fixed() || !resolution_fixed(text) {
        return None;
    }
    let procsub_inner = |range: &std::ops::Range<usize>| -> Option<String> {
        let open = range.start.checked_add(2)?;
        let close = range.end.checked_sub(1)?;
        if text.get(range.start..open)? != "<(" || text.get(close..range.end)? != ")" {
            return None;
        }
        Some(text.get(open..close)?.to_string())
    };
    let delivering = match source {
        ProgramSource::Undetermined(_) | ProgramSource::File(Value::Undetermined(_)) => {
            let w = cmd
                .argv
                .get(k + 1..)?
                .iter()
                .find(|w| !literal(w).is_ok_and(|s| s.starts_with('-')))?;
            procsub_inner(&w.span)?
        }
        ProgramSource::Stdin(StdinSource::Literal(_)) => return None,
        ProgramSource::Stdin(_) => {
            let ins: Vec<_> = cmd
                .redirects
                .iter()
                .filter(|r| {
                    matches!(r.fd, None | Some(0))
                        && matches!(
                            r.op,
                            RedirOp::In
                                | RedirOp::InOut
                                | RedirOp::DupIn
                                | RedirOp::HereDoc
                                | RedirOp::HereString
                        )
                })
                .collect();
            match ins.as_slice() {
                [r] if r.op == RedirOp::In && r.target == RedirTarget::ProcessSubstitution => {
                    let at = text.get(r.span.clone())?.find("<(")?;
                    procsub_inner(&(r.span.start + at..r.span.end))?
                }
                [] => upstream_stages(text, commands, idx, cmd)?,
                _ => return None,
            }
        }
        _ => return None,
    };
    pipeline_output(&delivering)
        .filter(|o| o.beyond_plain_echo)
        .map(|o| o.text)
}

/// The index in `args` (the words after the shell word) of the `-c` command
/// string, when the options before it are a shape read with certainty
/// (backlog c602d32e). Bash reads options up to the first operand, and after
/// a `-c` the first operand is the command string. Accepted: clusters of the
/// letters `shell_source` (ast/mod.rs) models, with `c` anywhere in one of them (`-c`,
/// `-ec`, `-xc`, `-ce`), options before `-c` (`-x -c`), an `-o` / `-O` taking
/// the next word (only as the last letter of its cluster), and `--` / `-`
/// ending the options (`-c -- W`). Long options, `+` options, `-s` and any
/// letter not modelled give `None`, as does a line without `-c`; the caller
/// then keeps today's verdict.
fn dash_c_operand(args: &[Word]) -> Option<usize> {
    let mut c_flag = false;
    let mut i = 0;
    while let Some(w) = args.get(i) {
        // An undetermined word ends the options only after `-c`: it is then
        // the command string (the caller checks its value does not begin
        // with `-`). Before `-c` it could be an option, so it keeps `None`.
        let Ok(v) = literal(w) else {
            break;
        };
        if v == "--" || v == "-" {
            i += 1;
            break;
        }
        let Some(flags) = v.strip_prefix('-').filter(|f| !f.is_empty()) else {
            break;
        };
        if flags.starts_with('-') {
            return None;
        }
        let mut takes_arg = false;
        for (k, ch) in flags.char_indices() {
            match ch {
                'c' => c_flag = true,
                'o' | 'O' if k + 1 == flags.len() => takes_arg = true,
                c if "abefhkmnptuvxBCEHPTilr".contains(c) => {}
                _ => return None,
            }
        }
        i += 1;
        if takes_arg {
            if literal(args.get(i)?).is_err() {
                return None;
            }
            i += 1;
        }
    }
    if !c_flag {
        return None;
    }
    Some(i)
}

/// The Deny of the program a shell runs as `-c W` (`args` = the options of
/// [`dash_c_operand`] — `-c`, `-ec`, `-x -c`, `-c -- …` — then `W`) when its
/// source is Undetermined only because `W` is
/// a word whose value `determined_output::substituted_word_value` computes
/// from its `$( )` substitutions (backlog 28019c35): that value is judged by
/// the full detector as the `-c` program ([`judge_body`], depth-bounded).
///
/// `None` — the caller keeps today's verdict — when the shape differs, when
/// the user's line, an enclosing derived text, or `text` may rebind the
/// modelled names (`chain_names_fixed` / `resolution_fixed`, as for
/// [`determined_delivered_program`]; a function named `echo` on the chain
/// fails them), or when the judged program is not a Deny: an Ask or Allow on
/// the computed text never replaces today's Undetermined, so nothing is
/// loosened. `eval W` reuses `substituted_word_value` the same way
/// ([`determined_eval_program`]).
pub(super) fn determined_dash_c_program(
    text: &str,
    fam: Family,
    base: &str,
    args: &[Word],
    source: &ProgramSource,
    judge: &Probes<'_>,
) -> Option<Decision> {
    use crate::detect::payload::determined_output::{resolution_fixed, substituted_word_value};
    if fam != Family::Shell || !matches!(source, ProgramSource::Undetermined(_)) {
        return None;
    }
    let word = args.get(dash_c_operand(args)?)?;
    if !matches!(word.value, Value::Undetermined(_)) {
        return None;
    }
    if !crate::detect::readings::chain_names_fixed() || !resolution_fixed(text) {
        return None;
    }
    let program = substituted_word_value(&word.raw)?;
    // Bash reads a command string that begins with `-` as an option (`--`
    // ends them): such a value is not the program, so it is not judged here.
    if program.starts_with('-') {
        return None;
    }
    let verdict = judge_body(&program, Runner::Shell(base), judge);
    verdict.is_deny().then_some(verdict)
}

/// The Deny of the code `eval W…` runs (`args` = the operands after `eval`;
/// one leading `--` is dropped, as bash's `eval` does) when at least one
/// operand is a word whose value `substituted_word_value` computes from its
/// `$( )` substitutions (`eval "$(echo 'rm -rf /etc' | cat)"`, backlog
/// d766b5fe; `eval "$(…)" x` / `eval -- "$(…)"`, backlog bf40851f): the
/// operands' values — literal ones as they are, computed ones as computed —
/// joined by single spaces, as `eval` joins them, are judged by the full
/// detector as `bash -c '<value>'` ([`judge_body`]). One operand whose value
/// is not computed keeps today's Undetermined.
///
/// The precondition is [`determined_dash_c_program`]'s with one widening:
/// `text` — and only `text`, the enclosing chain is read strictly — may hold
/// ONE `eval` (bare, or as `command [-p] [--] eval` / `builtin [--] eval`),
/// this one (`chain_names_fixed_but_one_eval` /
/// `resolution_fixed_but_one_eval`), since `W`'s substitutions run before it.
/// A function, alias, assignment, any other unlisted command or a second
/// `eval` on the line keeps today's Undetermined. Raise-only: an Ask or Allow
/// on the computed code is not taken.
pub(super) fn determined_eval_program(
    text: &str,
    args: &[Word],
    judge: &Probes<'_>,
) -> Option<Decision> {
    use crate::detect::payload::determined_output::{
        resolution_fixed_but_one_eval, substituted_word_value,
    };
    let operands = match args.split_first() {
        Some((first, rest)) if literal(first).is_ok_and(|f| f == "--") => rest,
        _ => args,
    };
    if operands.is_empty()
        || !operands
            .iter()
            .any(|w| matches!(w.value, Value::Undetermined(_)))
        || !crate::detect::readings::chain_names_fixed_but_one_eval()
        || !resolution_fixed_but_one_eval(text)
    {
        return None;
    }
    let values = operands
        .iter()
        .map(|w| match &w.value {
            Value::Literal(s) => Some(s.clone()),
            Value::Undetermined(_) => substituted_word_value(&w.raw),
        })
        .collect::<Option<Vec<_>>>()?;
    let program = values.join(" ");
    let verdict = judge_body(&program, Runner::Shell("bash"), judge);
    verdict.is_deny().then_some(verdict)
}

/// The Deny of `builtin [--] eval W…`, which runs `eval W…` (backlog
/// bf40851f): [`determined_eval_program`] on the operands after `eval`.
/// `None` for every other `builtin` shape and whenever that reading is
/// `None`; the caller then keeps the no-opinion Allow `builtin` had before
/// (the legacy verdict stands), so nothing is loosened.
pub(super) fn determined_builtin_eval(
    text: &str,
    args: &[Word],
    judge: &Probes<'_>,
) -> Option<Decision> {
    let rest = match args.split_first() {
        Some((dd, rest)) if literal(dd).is_ok_and(|w| w == "--") => rest,
        _ => args,
    };
    let (head, operands) = rest.split_first()?;
    if !literal(head).is_ok_and(|w| w == "eval") || operands.is_empty() {
        return None;
    }
    determined_eval_program(text, operands, judge)
}
