//! The output of a producer pipeline whose every stage is modelled, computed
//! from the shell parser's AST (backlog 40dc52c0).
//!
//! `bash <(echo hi | cat)`, `bash <(seq 10 | sort)`, `bash <(echo "$(echo hi)")`
//! and `echo hi | cat | bash` hand a shell a program that is fully determined
//! by the command line, but the readings in [`super::procsub_inner_pipelines`]
//! and [`super::stdin_program_texts`] only read the literals of ONE `echo` /
//! `printf` stage: a later `cat` or `sort`, or a `$( )` inside the echoed
//! word, left the program unread (the "not on the command line" and the
//! "contains a shell expansion" Asks of `interpreter_program_verdict`). This
//! module computes that program text, and only for these shapes:
//!
//! * the text parses (brush-parser, the parser of [`super::super::ast`]) as
//!   ONE pipeline of simple commands — no list, `&&` / `||`, `&`, `!`, `time`,
//!   subshell, group, function, redirection, assignment or process
//!   substitution;
//! * its first stage is an emitter and every later stage a filter:
//!   * `echo` whose operands are determined words, none starting with `-` and
//!     none holding a backslash (zsh's and sh's `echo` decode escapes): the
//!     operands joined by spaces, then a newline. With a lone first operand
//!     `-e`, `\xHH` escapes (two hex digits, printable ASCII) are decoded;
//!     any other backslash stays undetermined;
//!   * `seq LAST`, `seq FIRST LAST` or `seq FIRST INCR LAST` with unsigned
//!     decimal literals (no leading zero, at most 9 digits, `INCR` > 0, at
//!     most [`MAX_SEQ_LINES`] lines): one number per line;
//!   * `cat` with no operand and no option: its stdin, unchanged;
//!   * `sort` with no operand and no option, over an input whose line order is
//!     the same in every locale — every line digits only, or all lines equal:
//!     the lines in byte order, each ending in a newline. Any other input's
//!     order depends on the locale's collation, which the text does not name,
//!     so it is not determined;
//! * a determined word is unquoted text of plain characters
//!   ([`plain_unquoted`]), single-quoted text, or a double-quoted sequence of
//!   text and `$( )` command substitutions whose program is itself such a
//!   pipeline (its output with trailing newlines removed, as the shell does),
//!   at most [`MAX_DEPTH`] substitutions deep. An unquoted `$( )` (word
//!   splitting, globbing), a backquote, a parameter, an escape, a tilde, `$'…'`
//!   or `$"…"` is not determined.
//!
//! Anything else is `None`, and the caller keeps today's reading and Ask.
//!
//! `cat`, `sort` and `seq` are external commands (and `echo` can be a
//! function), so the computed text is the program only if nothing on the
//! command line changes what those names run. [`resolution_fixed`] is that
//! precondition. Callers require it of the user's top-level line, of EVERY
//! derived text on the way to the one being judged, AND of that text: the
//! chain value starts from the user's line in `readings::detect_bash_every_reading`
//! and is only ever narrowed — by every re-entry there
//! (`readings::chain_names_fixed`, read by b5) and by `detect_bash` for each
//! text it judges (`Ctx::names_fixed`, [`Ctx::names_fixed_narrowed_by`]) — so
//! a function defined or exported before an `eval` / `bash -c` body, and a
//! MIDDLE body that rebinds a name (`bash -c 'PATH=…; bash -c "echo hi | cat
//! | bash"'`), are seen. They compute nothing otherwise.
//!
//! [`Ctx::names_fixed_narrowed_by`]: crate::detect::Ctx::names_fixed_narrowed_by

use super::super::ast::{self, Analysis, Value};
use super::{evaluate_printf_stage, plain_echo_output, PrintfStage};
use brush_parser::ast as bast;
use brush_parser::word::{WordPiece, WordPieceWithSource};
use std::panic::{catch_unwind, AssertUnwindSafe};

/// Nesting bound for `$( )` inside a determined word.
const MAX_DEPTH: usize = 3;
/// Bound on the lines a `seq` stage may emit (and so on the program size).
const MAX_SEQ_LINES: u64 = 1000;

/// What a modelled pipeline writes to stdout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::detect) struct Output {
    /// The exact bytes written.
    pub text: String,
    /// True when the computation needed more than the single literal
    /// `echo` the existing readings already read: a filter stage, `seq`, or a
    /// command substitution. Callers apply the computed text only then, so
    /// every shape the existing readings handle keeps exactly its reading.
    pub beyond_plain_echo: bool,
}

/// True when nothing on `cmd` can change what the modelled names (`echo`,
/// `seq`, `cat`, `sort`) run: the AST layer evaluates `cmd`
/// ([`ast::analyze`] is `Known`, so it defines no function), and every command
/// on it has no assignment (`PATH=…` in a prefix or on its own) and a
/// determined command word that is one of the modelled names or a shell /
/// code interpreter reading a program. Any other command (`alias`, `hash`,
/// `enable`, `export`, `cd`, …) — even a harmless one — makes it false: an
/// allowlist, not a list of the builtins known to rebind names, so an
/// unlisted way to rebind one cannot slip through.
pub(in crate::detect) fn resolution_fixed(cmd: &str) -> bool {
    let Analysis::Known(script) = ast::analyze(cmd) else {
        return false;
    };
    if script.has_function_definition {
        return false;
    }
    script.commands.iter().all(|c| {
        if !c.assignments.is_empty() {
            return false;
        }
        let Some(head) = c.argv.first() else {
            // A redirection-only entry (a compound command's own redirects).
            return true;
        };
        match &head.value {
            Value::Literal(name) => {
                !name.contains('/')
                    && (matches!(name.as_str(), "echo" | "seq" | "cat" | "sort")
                        || super::is_shell(name)
                        || super::is_code_interpreter(name))
            }
            Value::Undetermined(_) => false,
        }
    })
}

impl<'a> crate::detect::Ctx<'a> {
    /// The context for judging `cmd` when `cmd` is the first text on the
    /// derivation chain that fails [`resolution_fixed`] (a middle `-c` /
    /// `eval` / `$( )` body assigning `PATH`, defining a function, running
    /// `hash` / `alias` / `enable`, …): `names_fixed` turned off, so no layer
    /// below computes a producer's output. `None` (keep `self`) when it is
    /// already off or `cmd` passes. Only ever narrows (backlog 40dc52c0).
    pub(in crate::detect) fn names_fixed_narrowed_by(&self, cmd: &str) -> Option<Self> {
        (self.names_fixed && !resolution_fixed(cmd)).then(|| crate::detect::Ctx {
            names_fixed: false,
            ..self.clone()
        })
    }
}

/// The output of `text` when it is a modelled pipeline (see the module docs);
/// `None` otherwise.
pub(in crate::detect) fn pipeline_output(text: &str) -> Option<Output> {
    output_at(text, 0)
}

fn output_at(text: &str, depth: usize) -> Option<Output> {
    if depth > MAX_DEPTH {
        return None;
    }
    // The AST layer's size and nesting caps and its evaluation: a text it
    // cannot evaluate is not computed here either.
    if !matches!(ast::analyze(text), Analysis::Known(_)) {
        return None;
    }
    let options = brush_parser::ParserOptions::default();
    let program = catch_unwind(AssertUnwindSafe(|| {
        brush_parser::Parser::new(text.as_bytes(), &options).parse_program()
    }))
    .ok()?
    .ok()?;
    let [list] = program.complete_commands.as_slice() else {
        return None;
    };
    let [bast::CompoundListItem(and_or, bast::SeparatorOperator::Sequence)] = list.0.as_slice()
    else {
        return None;
    };
    if !and_or.additional.is_empty() || and_or.first.bang || and_or.first.timed.is_some() {
        return None;
    }
    let mut stdout: Option<String> = None;
    let mut beyond_plain_echo = and_or.first.seq.len() > 1;
    for (pos, command) in and_or.first.seq.iter().enumerate() {
        let bast::Command::Simple(simple) = command else {
            return None;
        };
        if simple.prefix.as_ref().is_some_and(|p| !p.0.is_empty()) {
            return None;
        }
        let mut argv = Vec::new();
        for raw in std::iter::once(simple.word_or_name.as_ref()?).chain(
            simple
                .suffix
                .iter()
                .flat_map(|s| s.0.iter())
                .map(|item| match item {
                    bast::CommandPrefixOrSuffixItem::Word(w) => Some(w),
                    _ => None,
                })
                .collect::<Option<Vec<_>>>()?,
        ) {
            let (value, substituted) = determined_word(&raw.value, depth)?;
            beyond_plain_echo |= substituted;
            argv.push(value);
        }
        let (name, operands) = argv.split_first()?;
        stdout = Some(match (pos, name.as_str()) {
            (0, "echo") => {
                let (text, decoded) = echo_output(operands)?;
                beyond_plain_echo |= decoded;
                text
            }
            (0, "seq") => {
                beyond_plain_echo = true;
                seq_output(operands)?
            }
            (p, "cat") if p > 0 && operands.is_empty() => stdout?,
            (p, "sort") if p > 0 && operands.is_empty() => sort_output(&stdout?)?,
            _ => return None,
        });
    }
    Some(Output {
        text: stdout?,
        beyond_plain_echo,
    })
}

/// The value of one word, and whether a command substitution was computed.
fn determined_word(raw: &str, depth: usize) -> Option<(String, bool)> {
    let options = brush_parser::ParserOptions::default();
    let pieces = catch_unwind(AssertUnwindSafe(|| {
        brush_parser::word::parse(raw, &options)
    }))
    .ok()?
    .ok()?;
    let mut out = String::new();
    let mut substituted = false;
    for p in &pieces {
        match &p.piece {
            WordPiece::Text(s) if plain_unquoted(s) => out.push_str(s),
            WordPiece::SingleQuotedText(s) => out.push_str(s),
            WordPiece::DoubleQuotedSequence(inner) => {
                substituted |= double_quoted(inner, depth, &mut out)?;
            }
            _ => return None,
        }
    }
    Some((out, substituted))
}

/// Unquoted text that the shell neither globs, brace-expands, tilde-expands
/// nor (with extglob) pattern-matches: ASCII letters, digits and `_ . / : , %
/// + = - @ ^` only.
fn plain_unquoted(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "_./:,%+=-@^".contains(c))
}

/// Append a double-quoted sequence's value to `out`; true when it held a
/// command substitution.
fn double_quoted(inner: &[WordPieceWithSource], depth: usize, out: &mut String) -> Option<bool> {
    let mut substituted = false;
    for p in inner {
        match &p.piece {
            WordPiece::Text(s) if !s.contains('\\') => out.push_str(s),
            WordPiece::CommandSubstitution(program) => {
                let sub = output_at(program, depth + 1)?;
                out.push_str(sub.text.trim_end_matches('\n'));
                substituted = true;
            }
            _ => return None,
        }
    }
    Some(substituted)
}

/// `echo` output, and whether `-e` escapes were decoded to get it. A first
/// operand `-e` (alone, not a cluster) decodes ONLY `\xHH` with exactly two hex
/// digits naming a printable ASCII byte or a space (every echo that decodes at
/// all, bash `-e` and zsh, reads that the same way); any other backslash,
/// any other option, or a control/non-ASCII result is not determined (`None`,
/// the caller keeps its restrictive reading).
fn echo_output(operands: &[String]) -> Option<(String, bool)> {
    let (decode, operands) = match operands.split_first() {
        Some((first, rest)) if first == "-e" => (true, rest),
        _ => (false, operands),
    };
    if operands.first().is_some_and(|a| a.starts_with('-')) {
        return None;
    }
    let mut words = Vec::new();
    let mut decoded = false;
    for a in operands {
        if !a.contains('\\') {
            words.push(a.clone());
            continue;
        }
        if !decode {
            return None;
        }
        words.push(decode_hex_escapes(a)?);
        decoded = true;
    }
    Some((format!("{}\n", words.join(" ")), decoded))
}

fn decode_hex_escapes(a: &str) -> Option<String> {
    let b = a.as_bytes();
    let mut out = String::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'\\' {
            let ch = a[i..].chars().next()?;
            out.push(ch);
            i += ch.len_utf8();
            continue;
        }
        if b.get(i + 1) != Some(&b'x') {
            return None;
        }
        let h = a.get(i + 2..i + 4)?;
        if !h.bytes().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        let v = u8::from_str_radix(h, 16).ok()?;
        if !(v == b' ' || v.is_ascii_graphic()) || v == b'\\' {
            return None;
        }
        out.push(v as char);
        i += 4;
    }
    Some(out)
}

/// An unsigned decimal literal without a leading zero, at most 9 digits.
fn seq_number(s: &str) -> Option<u64> {
    let ok = !s.is_empty()
        && s.len() <= 9
        && s.bytes().all(|b| b.is_ascii_digit())
        && (s == "0" || !s.starts_with('0'));
    if ok {
        s.parse().ok()
    } else {
        None
    }
}

fn seq_output(operands: &[String]) -> Option<String> {
    let nums: Vec<u64> = operands
        .iter()
        .map(|o| seq_number(o))
        .collect::<Option<_>>()?;
    let (first, incr, last) = match nums.as_slice() {
        [last] => (1, 1, *last),
        [first, last] => (*first, 1, *last),
        [first, incr, last] => (*first, *incr, *last),
        _ => return None,
    };
    if incr == 0 {
        return None;
    }
    let lines = if last >= first {
        (last - first) / incr + 1
    } else {
        0
    };
    if lines > MAX_SEQ_LINES {
        return None;
    }
    let mut out = String::new();
    let mut n = first;
    for _ in 0..lines {
        out.push_str(&n.to_string());
        out.push('\n');
        n += incr;
    }
    Some(out)
}

fn sort_output(input: &str) -> Option<String> {
    let body = input.strip_suffix('\n').unwrap_or(input);
    if input.is_empty() {
        return Some(String::new());
    }
    let mut lines: Vec<&str> = body.split('\n').collect();
    let digits_only = lines.iter().all(|l| l.bytes().all(|b| b.is_ascii_digit()));
    let all_equal = lines.windows(2).all(|w| w[0] == w[1]);
    if !digits_only && !all_equal {
        return None;
    }
    lines.sort_unstable();
    Some(lines.iter().map(|l| format!("{l}\n")).collect())
}

// The members and the combined output of a `;` / `&&` / newline list of
// literal emitters (moved here from `payload.rs`, file-growth split).

/// The members of `inner` when it is a list of commands joined only by `;`,
/// `&&` or newlines outside quotes, each without a pipe, `||`, `&` or a
/// parenthesis (`None` otherwise). Empty members are dropped.
pub(super) fn simple_list_members(inner: &str) -> Option<Vec<String>> {
    let chars: Vec<char> = inner.chars().collect();
    let mut members = Vec::new();
    let mut cur = String::new();
    let (mut in_s, mut in_d) = (false, false);
    let mut k = 0;
    while k < chars.len() {
        let c = chars[k];
        if c == '\\' && !in_s {
            cur.push(c);
            if let Some(&n) = chars.get(k + 1) {
                cur.push(n);
            }
            k += 2;
            continue;
        }
        if c == '\'' && !in_d {
            in_s = !in_s;
        } else if c == '"' && !in_s {
            in_d = !in_d;
        } else if !in_s && !in_d {
            if c == ';' || c == '\n' || (c == '&' && chars.get(k + 1) == Some(&'&')) {
                members.push(std::mem::take(&mut cur));
                k += if c == '&' { 2 } else { 1 };
                continue;
            }
            if matches!(c, '|' | '&' | '(' | ')') {
                return None;
            }
        }
        cur.push(c);
        k += 1;
    }
    if in_s || in_d {
        return None;
    }
    members.push(cur);
    let members: Vec<String> = members
        .into_iter()
        .map(|m| m.trim().to_string())
        .filter(|m| !m.is_empty())
        .collect();
    if members.is_empty() {
        None
    } else {
        Some(members)
    }
}

/// Every reading of what the list `members` writes to stdout, in order, when
/// each member is a plain `echo` ([`plain_echo_output`]) or a `printf` whose
/// output [`evaluate_printf_stage`] determines (one reading per combination of
/// its dialect outputs, at most 16). Otherwise `Err` naming why the output is
/// undetermined (the caller's Ask).
pub(super) fn concatenated_outputs(members: &[String]) -> Result<Vec<String>, String> {
    const MAX_READINGS: usize = 16;
    let mut readings = vec![String::new()];
    for member in members {
        let outputs = match plain_echo_output(member) {
            Some(text) => vec![text],
            None => match evaluate_printf_stage(member) {
                PrintfStage::Outputs(texts) => texts,
                PrintfStage::Undetermined(why) => {
                    return Err(format!("the printf `{member}` cannot be evaluated ({why})"))
                }
                PrintfStage::NotPrintf => {
                    return Err(format!(
                        "`{member}` is not a plain `echo` or a `printf` whose output is static"
                    ))
                }
            },
        };
        let mut next = Vec::new();
        for prefix in &readings {
            for text in &outputs {
                let joined = format!("{prefix}{text}");
                if !next.contains(&joined) {
                    next.push(joined);
                }
            }
        }
        if next.len() > MAX_READINGS {
            return Err(format!(
                "it has more than {MAX_READINGS} printf dialect readings"
            ));
        }
        readings = next;
    }
    Ok(readings)
}
