//! Shell functions on the derivation chain that may shadow a literal emitter
//! (`echo` / `printf`) whose arguments are read as a reader's program
//! (backlogs 1e9da081, f06ecb05).
//!
//! The stdin-program readings of [`super::analyze_interpreter_stdin_exec`]
//! (`echo 'x' | bash`, and every process substitution read as a program,
//! `bash <(echo x)`, which is judged as that pipeline) take the upstream
//! stage's quoted literals as the WHOLE program only because `echo` / `printf`
//! emit nothing but their own arguments
//! (`echo_output::stage_emits_only_its_arguments`). A shell function of the same
//! name defined on the line breaks that: the name runs the function body,
//! whose output is not the arguments:
//!
//! ```text
//! printf(){ echo 'chmod 777 /etc/hosts'; }; bash <(printf hi)
//! echo(){ echo 'chmod 777 /etc/hosts'; }; export -f echo; bash -c 'bash <(echo hi)'
//! ```
//!
//! Both were Allow: the program judged was `hi`. [`defined_functions`] reads
//! the function names a text defines; the chain value (the user's line AND
//! every derived text on the way to the one being judged, accumulated by
//! `readings::detect_bash_every_reading` and by `detect_bash` for each text it
//! judges, see [`crate::detect::Ctx::functions_widened_by`]) is what the
//! reader check consults. A function defined anywhere on the chain counts —
//! before or after the reader, exported or not: a conservative
//! over-approximation (`echo(){ :; }; bash -c 'echo hi | bash'` without
//! `export -f` is an Ask too), never an under-approximation.
//!
//! A text whose definitions cannot be read — it may define a function (it
//! holds a `(` … `)` pair with only blanks between, or the word `function`)
//! and the AST layer cannot evaluate it, or a function name is not a literal,
//! or it may name zsh's `functions` parameter (an occurrence of `functions`
//! not followed by an identifier character, `/`, `.` or `-`; see
//! [`may_name_zsh_functions_parameter`]) — is
//! [`Functions::Undetermined`]: every emitter name may be shadowed. A text
//! that holds neither shape defines no function (`NAME ( )` and the
//! `function` keyword are the only definition syntaxes of bash and zsh).
//!
//! Aliases (backlog cffe54c4) are the same class: `alias echo='echo chmod 777
//! /etc/hosts #'; bash <(echo hi)` runs the alias. [`defined_functions`] also
//! adds the `NAME` of every `NAME=VALUE` operand of a parsed simple command
//! whose command name (after quote removal, so `ali\as` and `a""lias` count)
//! is `alias` (`defined_aliases`), whether or not `expand_aliases` is enabled
//! there. `alias` as an argument or inside quotes adds nothing; a non-literal
//! command name or operand, or a text the AST cannot read, is Undetermined,
//! for texts whose letters can spell `alias` only.
//!
//! NOT modelled (no definition is seen, so nothing is added): a function
//! defined by a sourced FILE or by a startup file, inherited from the hook's
//! own environment, or defined by `eval` of a text that spells neither shape
//! literally (`eval $'echo\x28\x29 …'`).

use super::super::ast::{self, Analysis};

/// The function names a text — or a chain of texts — may define.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::detect) enum Functions {
    /// Exactly these names (possibly none).
    Known(Vec<String>),
    /// The definitions cannot be read: any name may be a function.
    Undetermined,
}

impl Functions {
    /// No function defined.
    pub(in crate::detect) fn none() -> Self {
        Functions::Known(Vec::new())
    }

    /// Whether `name` may run a function rather than the command of that
    /// name. `Undetermined` answers true (the restrictive side).
    pub(in crate::detect) fn may_shadow(&self, name: &str) -> bool {
        match self {
            Functions::Known(names) => names.iter().any(|n| n == name),
            Functions::Undetermined => true,
        }
    }

    /// The names of `self` and `other` together; `Undetermined` absorbs.
    pub(in crate::detect) fn union(&self, other: &Functions) -> Functions {
        match (self, other) {
            (Functions::Known(a), Functions::Known(b)) => {
                let mut names = a.clone();
                for n in b {
                    if !names.contains(n) {
                        names.push(n.clone());
                    }
                }
                Functions::Known(names)
            }
            _ => Functions::Undetermined,
        }
    }
}

impl<'a> crate::detect::Ctx<'a> {
    /// The context for judging `cmd`: the chain's functions (`Ctx.functions`)
    /// widened by the functions `cmd` itself may define. `None` (keep
    /// `self`) when that adds nothing. Only ever widens.
    pub(in crate::detect) fn functions_widened_by(&self, cmd: &str) -> Option<Self> {
        let widened = self.functions.union(&defined_functions(cmd));
        (widened != self.functions).then(|| crate::detect::Ctx {
            functions: widened,
            ..self.clone()
        })
    }
}

/// The name, if any, that may run a shell function in the literal emitter
/// stage `upstream` piping a program into a reader — the stage whose literals
/// `stdin_program_texts` takes as the program because
/// `stage_emits_only_its_arguments` holds for it. Every command head of the
/// stage counts (the emitter and the exec-wrappers before it: a function
/// named `env` runs instead of `env` too). With `group_feeder`, the program
/// was read from the `echo` / `printf` members of a brace group
/// (`group_feeder_texts`), whose stages are not at hand: `echo` and `printf`
/// are checked. `None` when no such name may be a function.
///
/// The functions are those of the chain as `ctx` holds them
/// ([`crate::detect::Ctx::functions_widened_by`]). `upstream` is
/// `(stage, group_feeder)`; `shape` opens the Ask reason.
pub(super) fn shadowed_emitter_ask(
    shape: &str,
    (stage, group_feeder): (&str, bool),
    ctx: &crate::detect::Ctx<'_>,
) -> Option<crate::model::Decision> {
    let name = shadowed_emitter_head(stage, group_feeder, &ctx.functions)?;
    Some(crate::model::Decision::ask(format!(
        "{shape}, and `{name}` in the stage piping into it may be a shell function or alias defined on \
this command line (or on a line enclosing it) — the function's output, not the stage's \
arguments, is the program, so blastguard cannot read it"
    )))
}

fn shadowed_emitter_head(
    upstream: &str,
    group_feeder: bool,
    functions: &Functions,
) -> Option<String> {
    if group_feeder {
        return ["echo", "printf"]
            .into_iter()
            .find(|n| functions.may_shadow(n))
            .map(str::to_string);
    }
    if !super::echo_output::stage_emits_only_its_arguments(upstream) {
        return None;
    }
    let tokens: Vec<&str> = upstream.split_whitespace().collect();
    super::wrapped_command_heads(&tokens)
        .into_iter()
        .map(|idx| super::normalized_command(tokens[idx]))
        .find(|name| functions.may_shadow(name))
}

/// The names `text` defines as shell functions or as aliases (backlog
/// cffe54c4: `alias echo='…'` rebinds the emitter like a function does). See
/// the module docs.
pub(in crate::detect) fn defined_functions(text: &str) -> Functions {
    defined_function_names(text).union(&defined_aliases(text))
}

/// True when the letters of `alias` occur in `text` in order (not
/// necessarily adjacent): the only texts whose command name can, after quote
/// removal or expansion, spell `alias` (`ali\as`, `a""lias`, `a=al; ${a}ias`).
/// A text failing this cannot run the alias builtin and is not parsed.
fn may_spell_alias(text: &str) -> bool {
    let mut want = "alias".chars();
    let mut next = want.next();
    for c in text.chars() {
        if Some(c) == next {
            next = want.next();
        }
    }
    next.is_none()
}

enum Peeled<'a> {
    /// The words peeled down to the `alias` builtin; its operands.
    Alias(&'a [ast::Word]),
    /// Runs another literal command, or nothing (`command -v`).
    OtherCommand,
    /// An unrecognised shape: it may run `alias`.
    Undetermined,
}

/// The words after a `builtin` / `command` head, peeled structurally:
/// nested `builtin` / `command` words, `-p` and `--` are skipped; `-v` / `-V`
/// run nothing; any other option, or a non-literal word, is `Undetermined`.
fn peel_builtin_command(words: &[ast::Word]) -> Peeled<'_> {
    for (i, w) in words.iter().enumerate() {
        let ast::Value::Literal(s) = &w.value else {
            return Peeled::Undetermined;
        };
        match s.as_str() {
            "builtin" | "command" | "-p" | "--" => {}
            "-v" | "-V" => return Peeled::OtherCommand,
            "alias" => return Peeled::Alias(&words[i + 1..]),
            s if s.starts_with('-') => return Peeled::Undetermined,
            _ => return Peeled::OtherCommand,
        }
    }
    Peeled::OtherCommand
}

/// The alias names `text` may define, read from the parsed script (not from
/// the text): the `NAME=VALUE` operands of every simple command whose command
/// name, after quote removal, is `alias` (also `builtin alias` /
/// `command alias`). `alias` as an argument (`grep alias f`, `echo alias`) or
/// inside quotes adds nothing. A command name that is not a literal (it may
/// spell `alias`), an operand that is not a literal, or a text the AST cannot
/// read is [`Functions::Undetermined`] — but only for a text that
/// [`may_spell_alias`]. Whether `expand_aliases` is on is not asked (the
/// restrictive reading). An indirect spelling is covered only through that
/// non-literal-command-name rule (`a=al; ${a}ias echo=x` is Undetermined).
fn defined_aliases(text: &str) -> Functions {
    if !may_spell_alias(text) {
        return Functions::none();
    }
    let script = match ast::analyze_with_function_definitions(text) {
        Analysis::Known(script) => script,
        Analysis::Undetermined(_) => return Functions::Undetermined,
    };
    let mut names = Vec::new();
    for cmd in &script.commands {
        let Some(head) = cmd.argv.first() else {
            continue;
        };
        let operands = match &head.value {
            ast::Value::Literal(h) if h == "alias" => &cmd.argv[1..],
            ast::Value::Literal(h) if h == "builtin" || h == "command" => {
                match peel_builtin_command(&cmd.argv[1..]) {
                    Peeled::Alias(rest) => rest,
                    Peeled::OtherCommand => continue,
                    Peeled::Undetermined => return Functions::Undetermined,
                }
            }
            ast::Value::Literal(_) => continue,
            ast::Value::Undetermined(_) => return Functions::Undetermined,
        };
        for w in operands {
            let ast::Value::Literal(op) = &w.value else {
                return Functions::Undetermined;
            };
            if let Some((name, _)) = op.split_once('=') {
                if !names.iter().any(|n| n == name) {
                    names.push(name.to_string());
                }
            }
        }
    }
    Functions::Known(names)
}

fn defined_function_names(text: &str) -> Functions {
    if !may_define_function(text) {
        return Functions::none();
    }
    if may_name_zsh_functions_parameter(text) {
        return Functions::Undetermined;
    }
    match ast::analyze_with_function_definitions(text) {
        Analysis::Known(script) => Functions::Known(script.function_names),
        Analysis::Undetermined(_) => Functions::Undetermined,
    }
}

/// True when `text` may name zsh's `functions` parameter (or `dis_functions`)
/// as a whole parameter name. zsh's `zsh/parameter` module makes
/// `functions` an associative array whose assignment DEFINES a function
/// (`functions[echo]='…'`, `functions+=(echo '…')`, `functions=(…)`,
/// `set -A functions …`, `typeset -A functions …`, `: ${functions[echo]::=…}`);
/// those spellings are not read here, so any of them is Undetermined.
///
/// An occurrence counts unless the character right after `functions` makes
/// it part of a different word that cannot be that parameter: an identifier
/// character (`functions_source`, read-only; `functionsx`, another name),
/// `/`, `.` or `-` (`functions/`, `functions.py`, `functions-x`: a path or
/// file name, not a parameter name — a parameter name holds none of them).
/// Every other following character (`[`, `=`, `+`, `:`, `}`, a blank, a
/// quote, an operator, the end of the text) counts, so a bare word
/// `functions` (`grep functions f`) is Undetermined too: the restrictive side.
fn may_name_zsh_functions_parameter(text: &str) -> bool {
    text.match_indices("functions").any(|(i, m)| {
        !text[i + m.len()..]
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '/' | '.' | '-'))
    })
}

/// True when `text` holds a function-definition shape: the word `function`,
/// or a `(` followed by only spaces, tabs, newlines or carriage returns and a
/// `)`. A carriage return is not a blank to bash; counting it only widens
/// this gate (more texts go on to [`ast::analyze_with_function_definitions`]).
fn may_define_function(text: &str) -> bool {
    if text.contains("function") {
        return true;
    }
    let bytes = text.as_bytes();
    bytes.iter().enumerate().any(|(i, &b)| {
        b == b'('
            && bytes[i + 1..]
                .iter()
                .find(|c| !matches!(c, b' ' | b'\t' | b'\n' | b'\r'))
                .is_some_and(|&c| c == b')')
    })
}
