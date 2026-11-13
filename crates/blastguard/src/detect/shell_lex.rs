//! The shell-lexed reading of a command line (backlog e2b9ffb1, critical).
//!
//! The segment splitters and line-level scans of [`super::detect_bash`] track
//! `'`/`"` naively: they know neither `#` comments nor backslash escapes. A
//! quote inside a comment (`echo $HOME #e"`) or an escaped quote
//! (`echo x\'a`) therefore opened a quoted span in their reading that the
//! shell never opens, and that span swallowed the NEXT line — `rm -rf /etc`
//! there was read as quoted operand text and the line came out Allow
//! (observed in /bin/bash 3.2, /bin/sh and /bin/zsh, see
//! `tests/heredoc_paren_bash32_verifier_20261107.rs`).
//!
//! Since backlog 9a2f5a08 the three segment splitters themselves honour
//! backslash-escaped quotes ([`super::segment::quote_escape_pair`]); they
//! still know no `#` comments, and the other line-level quote trackers of
//! [`super::detect_bash`] know neither, so this reading is still needed.
//!
//! [`shell_lexed_reading`] re-spells the line the way the shell tokenises it,
//! so that the naive trackers read it correctly: every `#` comment is removed
//! (its newline kept), every backslash-escaped `'`/`"`/`` ` `` — one the
//! shell reads as a literal character, never as a quote — becomes `_`, and
//! every line continuation (backslash-newline) outside single quotes, `$'…'`,
//! comments and here-document bodies is removed (backlog 040b959b);
//! [`line_continuation_reading`] removes only those line continuations. The
//! caller ([`super::readings::detect_bash_every_reading`]) judges that text IN
//! ADDITION to the original, the most restrictive verdict winning, so this
//! reading can only add a verdict, never remove one.
//!
//! Modelled: single quotes, `$'…'` (ANSI-C, where `\'` is escaped), double
//! quotes, backticks, `$(…)`/`<(…)`/`>(…)`/`(…)` nesting (each a fresh code
//! context, as in the shell), `$((…))` arithmetic (no comments there), and
//! here-document bodies: a QUOTED-delimiter body (`<<'EOF'`) is copied
//! verbatim (it is data, an apostrophe in it is not a quote — the existing
//! body handling of the splitters still applies); an UNQUOTED one (`<<EOF`) is
//! expanded by the shell, so its `'`/`"` are literal (spelt `_`) while its
//! `$(…)`/backtick spans are live code, lexed as such — a stray `)'` earlier
//! in the body no longer opens a span that hides a later backtick or `$(…)`
//! substitution (backlog 5916d385). An unquoted body this lexer does not model
//! makes the reading [`ShellLexed::Undetermined`] (Ask), never clean.
//! Where the model guesses (a `case` pattern `)` closing a `$(…)` early, a
//! `<<` that is really an arithmetic shift outside `$((…))`), the guess only
//! leaves text verbatim or mis-nests a context; that can make this reading
//! less corrective, never hide anything the original reading still judges.
//!
//! [`bash32_lexed_reading`] (backlog 089bedd4) is the same lexer for the macOS
//! `/bin/bash` 3.2 reading, where a `)` in a here-document body inside `$(…)`
//! closes the substitution early and the text after it is top-level code; it
//! answers [`Bash32Lexed::Undetermined`] (Ask) for every shape it does not
//! model.

use super::substitution::{
    backtick_body_as_run, entry_comments, scan_balanced_strict_modern, scan_bash32, Scan32, Stop,
    MAX_QUOTE_PAREN_DEPTH,
};

/// The lexer context the current character is read in.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Lex {
    /// Ordinary shell code: top level, `$(…)`, `(…)`.
    Paren,
    /// `<(…)` / `>(…)`: ordinary shell code, a process substitution.
    ProcSub,
    /// `$((…))`: arithmetic, where `#` and `<<` are operators.
    Arith,
    /// Inside a backtick command substitution.
    Backtick,
    /// Inside a double-quoted string.
    Double,
}

/// A here-document whose body starts after the next code newline.
struct Pending {
    delimiter: String,
    strips_tabs: bool,
    /// The delimiter word held a `'`, `"` or `\`: the body is not expanded.
    quoted: bool,
}

/// The modern (bash 4+ / zsh) shell-lexed reading of a command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ShellLexed {
    /// The spelling is identical to the command: nothing to add.
    Same,
    /// The command re-spelled as the shell tokenises it.
    Text(String),
    /// An unquoted here-document body holds a shape this lexer does not model
    /// (see [`copy_here_doc_bodies`]): the caller must resolve this to Ask,
    /// never to clean.
    Undetermined,
}

/// `cmd` re-spelled as the shell tokenises it (see the module doc).
pub(super) fn shell_lexed_reading(cmd: &str) -> ShellLexed {
    let chars: Vec<char> = cmd.chars().collect();
    let lexed = lex(&chars, false, true, 0);
    if lexed.undetermined {
        ShellLexed::Undetermined
    } else if lexed.text == cmd {
        ShellLexed::Same
    } else {
        ShellLexed::Text(lexed.text)
    }
}

/// `cmd` with every line continuation the shell removes removed, and nothing
/// else changed (backlog 040b959b, critical).
///
/// The shell deletes a backslash-newline before it splits words — unquoted,
/// inside double quotes, inside `$(…)`, backticks and `$((…))` — but not inside
/// single quotes, `$'…'`, a `#` comment or a here-document body (observed in
/// /bin/bash 3.2, /bin/sh and /bin/zsh). The splitters and word scans of
/// [`super::detect_bash`] split at every newline, so a command word written
/// `chm\<NL>od` was judged as the two harmless lines `chm\` and `od …` while
/// the shell runs `chmod …`. The callers
/// ([`super::readings::detect_bash_every_reading`];
/// [`super::readings::shell_payload_verdict`] for a payload a nested shell
/// re-parses — `bash -c 'chm\<NL>od …'`; and
/// [`super::readings::shell_program_readings`] for a shell program on stdin —
/// `bash <<< 'chm\<NL>od …'`) judge this text IN ADDITION to the original, so
/// it can only add a verdict. Unlike [`shell_lexed_reading`] it keeps
/// comments and escaped quotes as they are, so that a backslash-escaped
/// program operand (`python3 -c o\<NL>pen\(\'…\',\'w\'\)`) is judged with its
/// quotes intact.
///
/// [`ShellLexed::Same`] when `cmd` holds no backslash-newline. A here-document
/// whose body joins lines in a shape the lexer does not model, and the depth
/// cap, give [`ShellLexed::Undetermined`]: the caller resolves it to Ask.
pub(super) fn line_continuation_reading(cmd: &str) -> ShellLexed {
    if !cmd.contains("\\\n") {
        return ShellLexed::Same;
    }
    let chars: Vec<char> = cmd.chars().collect();
    let lexed = lex(&chars, false, false, 0);
    if lexed.undetermined {
        ShellLexed::Undetermined
    } else if lexed.text == cmd {
        ShellLexed::Same
    } else {
        ShellLexed::Text(lexed.text)
    }
}

/// The bash 3.2 shell-lexed reading of a command (backlog 089bedd4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Bash32Lexed {
    /// The spelling is identical to the command: nothing to add.
    Same,
    /// The command re-spelled as macOS `/bin/bash` 3.2 tokenises it.
    Text(String),
    /// A shape whose bash 3.2 tokenisation is not modelled: the caller must
    /// resolve this to Ask, never to clean.
    Undetermined,
}

/// `cmd` re-spelled as macOS `/bin/bash` 3.2 (and `/bin/sh`) tokenises it.
///
/// bash 3.2's parser does not recognise a here-document inside `$(...)` or a
/// backtick: it finds the end of the substitution by matching parens and
/// quotes over the body text, so a `)` (or a backtick) in the body can close
/// the substitution EARLY, and the text after that close is ordinary shell
/// code (observed, `tests/heredoc_paren_bash32_verifier_20261107.rs` A1-A8:
/// `echo $(cat <<'EOF'` / `x) #e"` / `rm …` runs `rm`). The naive quote
/// trackers of the splitters, run over the ORIGINAL text, then read the
/// comment's `"` (or an escaped `\'`, …) as an opening quote that swallows
/// the next line, which hid it.
///
/// This lexer is [`shell_lexed_reading`]'s, except that every `$(` (not
/// `$((`) and every backtick substitution is delimited by [`scan_bash32`] —
/// the same end the bash 3.2 analysis pass uses
/// ([`super::substitution::under_bash32_reading`]) — instead of by the modern
/// rules. The substitution's content is re-lexed as the standalone script
/// bash 3.2 later runs it as (a fresh parse, where a here-document IS
/// recognised), and each of its here-documents still open when the content
/// ends — bash 3.2 delimits it by the end of the substitution text — is closed
/// by an appended delimiter line, so that no later line can be read as its
/// body. The text after the close is lexed as top-level code: `#` comments
/// removed, escaped quotes spelt `_`, `<(`/`>(` opened as live code contexts.
///
/// [`Bash32Lexed::Undetermined`] for every shape not modelled: a substitution
/// whose [`scan_bash32`] end is [`Scan32::Undetermined`]; a backtick body
/// holding a backslash (bash 3.2 backslash-processes it before parsing) whose
/// processed text holds a `<<` (without one the span is copied verbatim); a
/// substitution whose re-lexed content ends inside a quote or a nested
/// context; an open here-document there whose delimiter is empty; a
/// here-document opened inside `<(`/`>(`; and the depth cap. A substitution
/// that bash 3.2 never closes ([`Scan32::Unclosed`], a syntax error: nothing
/// from it on runs) is lexed with the modern rules, which only
/// over-approximates.
pub(super) fn bash32_lexed_reading(cmd: &str) -> Bash32Lexed {
    let chars: Vec<char> = cmd.chars().collect();
    let lexed = lex(&chars, true, true, 0);
    if lexed.undetermined {
        Bash32Lexed::Undetermined
    } else if lexed.text == cmd {
        Bash32Lexed::Same
    } else {
        Bash32Lexed::Text(lexed.text)
    }
}

/// One lexer run over a script.
struct Lexed {
    /// The re-spelled script.
    text: String,
    /// The script ended inside a quote or a nested context.
    open: bool,
    /// Delimiters of the here-documents still open when the script ended.
    unterminated: Vec<String>,
    /// A shape this lexer does not model was met: in bash 3.2 mode (see
    /// [`bash32_lexed_reading`]) or in an unquoted here-document body (both
    /// modes, see [`copy_here_doc_bodies`]).
    undetermined: bool,
}

impl Lexed {
    fn undetermined(text: String) -> Lexed {
        Lexed {
            text,
            open: true,
            unterminated: Vec::new(),
            undetermined: true,
        }
    }
}

/// The lexer behind [`shell_lexed_reading`] (`bash32 == false`),
/// [`bash32_lexed_reading`] (`bash32 == true`) and, with `respell == false`,
/// [`line_continuation_reading`].
///
/// In every mode a backslash-newline outside single quotes, `$'…'`, comments
/// and here-document bodies is removed, as the shell removes it before it
/// splits words (backlog 040b959b). With `respell == false` nothing else is
/// changed: comments, escaped quotes, `$'…'` and here-document bodies are
/// copied verbatim (`bash32` must then be `false`).
fn lex(chars: &[char], bash32: bool, respell: bool, depth: usize) -> Lexed {
    let mut out = String::with_capacity(chars.len());
    if depth >= MAX_QUOTE_PAREN_DEPTH {
        return Lexed::undetermined(out);
    }
    let mut unterminated: Vec<String> = Vec::new();
    // The bottom context is the top level; it is never popped.
    let mut stack: Vec<Lex> = vec![Lex::Paren];
    let mut pending: Vec<Pending> = Vec::new();
    let mut open_quote = false;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let top = *stack.last().unwrap_or(&Lex::Paren);
        let next = chars.get(i + 1).copied();
        // bash 3.2: a `$(` / backtick substitution is delimited by its
        // paren-and-quote scan (here-documents not recognised) and its
        // content re-lexed as a standalone script.
        let opens_sub = match c {
            '$' => next == Some('(') && chars.get(i + 2) != Some(&'('),
            '`' => top != Lex::Backtick,
            _ => false,
        };
        if bash32 && opens_sub && top != Lex::Arith {
            match lex_bash32_substitution(chars, i, depth, &mut out) {
                Sub32::Closed(after) => {
                    i = after;
                    continue;
                }
                Sub32::Unclosed => {}
                Sub32::Undetermined => return Lexed::undetermined(out),
            }
        }
        if top == Lex::Double {
            match c {
                '"' => {
                    stack.pop();
                    out.push(c);
                }
                // A line continuation: removed inside double quotes too.
                '\\' if next == Some('\n') => {
                    i += 2;
                    continue;
                }
                '\\' if respell && matches!(next, Some('"' | '`')) => {
                    out.push('_');
                    i += 2;
                    continue;
                }
                '\\' => {
                    out.push(c);
                    if let Some(n) = next {
                        out.push(n);
                    }
                    i += 2;
                    continue;
                }
                '`' => {
                    stack.push(Lex::Backtick);
                    out.push(c);
                }
                '$' if next == Some('(') => {
                    i = open_dollar_paren(chars, i, &mut stack, &mut out);
                    continue;
                }
                _ => out.push(c),
            }
            i += 1;
            continue;
        }
        // Code contexts: Paren, ProcSub, Arith, Backtick.
        match c {
            '\'' => {
                // A single-quoted span is copied whole; an unterminated one
                // runs to the end (the original reading sees the same).
                let end = match chars[i + 1..].iter().position(|&q| q == '\'') {
                    Some(p) => i + 1 + p + 1,
                    None => {
                        open_quote = true;
                        chars.len()
                    }
                };
                out.extend(&chars[i..end]);
                i = end;
                continue;
            }
            '$' if next == Some('\'') => {
                let (after, closed) = copy_ansi_c(chars, i, respell, &mut out);
                open_quote |= !closed;
                i = after;
                continue;
            }
            '"' => {
                stack.push(Lex::Double);
                out.push(c);
            }
            // A line continuation: the shell deletes the pair, so a word
            // split by it is one word (`chm\<NL>od` is `chmod`).
            '\\' if next == Some('\n') => {
                i += 2;
                continue;
            }
            '\\' if respell && matches!(next, Some('\'' | '"' | '`')) => {
                out.push('_');
                i += 2;
                continue;
            }
            '\\' => {
                out.push(c);
                if let Some(n) = next {
                    out.push(n);
                }
                i += 2;
                continue;
            }
            '`' => {
                if top == Lex::Backtick {
                    stack.pop();
                } else {
                    stack.push(Lex::Backtick);
                }
                out.push(c);
            }
            '$' if next == Some('(') => {
                i = open_dollar_paren(chars, i, &mut stack, &mut out);
                continue;
            }
            '(' => {
                let procsub = i > 0 && matches!(chars[i - 1], '<' | '>');
                stack.push(match top {
                    Lex::Arith => Lex::Arith,
                    _ if procsub => Lex::ProcSub,
                    _ => Lex::Paren,
                });
                out.push(c);
            }
            ')' => {
                if stack.len() > 1 && matches!(top, Lex::Paren | Lex::ProcSub | Lex::Arith) {
                    stack.pop();
                }
                out.push(c);
            }
            '#' if top != Lex::Arith && starts_word(&out) => {
                // A comment runs to the newline (kept), or — inside
                // backticks, whose body the shell cuts out first — to the
                // closing backtick. A backslash at its end continues nothing
                // (observed in /bin/bash 3.2, /bin/sh and /bin/zsh:
                // `echo a # b \` / `echo c` prints `a` and `c`).
                let mut j = i;
                while j < chars.len()
                    && chars[j] != '\n'
                    && !(top == Lex::Backtick && chars[j] == '`')
                {
                    j += 1;
                }
                if !respell {
                    out.extend(&chars[i..j]);
                }
                i = j;
                continue;
            }
            '<' if top != Lex::Arith && next == Some('<') => {
                if chars.get(i + 2) == Some(&'<') {
                    out.extend(&chars[i..i + 3]);
                    i += 3;
                    continue;
                }
                if bash32 && stack.contains(&Lex::ProcSub) {
                    // How bash 3.2 reads a here-document inside `<(`/`>(`
                    // was not observed.
                    return Lexed::undetermined(out);
                }
                i = read_here_doc_opener(chars, i, &mut pending, &mut out);
                continue;
            }
            '\n' => {
                out.push(c);
                i += 1;
                let bodies = copy_here_doc_bodies(
                    chars,
                    i,
                    &mut pending,
                    &mut out,
                    (bash32, respell),
                    depth,
                );
                if bodies.undetermined {
                    return Lexed::undetermined(out);
                }
                unterminated.extend(bodies.unterminated);
                i = bodies.after;
                continue;
            }
            _ => out.push(c),
        }
        i += 1;
    }
    // Openers on the last line: their bodies never started.
    unterminated.extend(pending.drain(..).map(|doc| doc.delimiter));
    Lexed {
        text: out,
        open: open_quote || stack.len() > 1,
        unterminated,
        undetermined: false,
    }
}

/// What [`lex_bash32_substitution`] did with a substitution opener.
enum Sub32 {
    /// Spelt in full; lexing resumes at this index.
    Closed(usize),
    /// bash 3.2 never closes it: nothing was spelt, lex it the modern way.
    Unclosed,
    /// Not modelled.
    Undetermined,
}

/// bash 3.2 mode: the `$(` or backtick substitution at `chars[i]`, delimited
/// by [`scan_bash32`] and its content re-lexed standalone (see
/// [`bash32_lexed_reading`]), appended to `out`. Nothing is appended unless
/// the answer is [`Sub32::Closed`].
fn lex_bash32_substitution(chars: &[char], i: usize, depth: usize, out: &mut String) -> Sub32 {
    let (start, stop, opener, closer) = if chars[i] == '`' {
        (i + 1, Stop::Backtick, "`", '`')
    } else {
        (i + 2, Stop::Paren, "$(", ')')
    };
    let comments = entry_comments(chars, start, stop);
    let end = match scan_bash32(chars, start, stop, 0, comments) {
        Scan32::Closed(end) => end,
        Scan32::Unclosed => return Sub32::Unclosed,
        Scan32::Undetermined => return Sub32::Undetermined,
    };
    let content = &chars[start..end];
    if stop == Stop::Backtick && content.contains(&'\\') {
        // A backtick body is backslash-processed before it is parsed, which
        // this re-lexing does not model. Without a `<<` in the body as run
        // there is no here-document for bash 3.2 to read differently, so the
        // span is kept verbatim (it is one word to the splitters, and its
        // body is judged through `backtick_body_as_run`); with one, it is
        // not modelled.
        let run = backtick_body_as_run(content);
        if run.contains("<<") {
            return Sub32::Undetermined;
        }
        out.extend(&chars[i..=end]);
        return Sub32::Closed(end + 1);
    }
    let inner = lex(content, true, true, depth + 1);
    if inner.undetermined || inner.open {
        return Sub32::Undetermined;
    }
    let mut spelt = String::from(opener);
    spelt.push_str(&inner.text);
    for delimiter in inner.unterminated {
        if delimiter.is_empty() {
            return Sub32::Undetermined;
        }
        if !spelt.ends_with('\n') {
            spelt.push('\n');
        }
        spelt.push_str(&delimiter);
        spelt.push('\n');
    }
    spelt.push(closer);
    out.push_str(&spelt);
    Sub32::Closed(end + 1)
}

/// True when a character appended to `out` (the text spelt so far) begins a
/// word, which is where `#` starts a comment. The spelt text, not the input,
/// decides: a removed line continuation joins `a\<NL>#b` into the one word
/// `a#b`, where the `#` starts no comment.
fn starts_word(out: &str) -> bool {
    out.chars().next_back().is_none_or(|c| {
        matches!(
            c,
            ' ' | '\t' | '\n' | ';' | '&' | '|' | '(' | ')' | '<' | '>'
        )
    })
}

/// `$(` or `$((` at `chars[i]`: push the matching context(s), copy, and return
/// the index after the opener.
fn open_dollar_paren(chars: &[char], i: usize, stack: &mut Vec<Lex>, out: &mut String) -> usize {
    if chars.get(i + 2) == Some(&'(') {
        // `$((`: both parens close with `))`.
        stack.push(Lex::Arith);
        stack.push(Lex::Arith);
        out.push_str("$((");
        return i + 3;
    }
    stack.push(Lex::Paren);
    out.push_str("$(");
    i + 2
}

/// `$'…'` at `chars[i]`: copy it with each escaped quote spelt `_` (copied as
/// is when `respell` is false), and return the index after the closing `'` (or
/// the end of the input) and whether that closing `'` was found.
fn copy_ansi_c(chars: &[char], i: usize, respell: bool, out: &mut String) -> (usize, bool) {
    out.push_str("$'");
    let mut j = i + 2;
    while j < chars.len() {
        match chars[j] {
            '\'' => {
                out.push('\'');
                return (j + 1, true);
            }
            '\\' if respell && matches!(chars.get(j + 1), Some('\'' | '"' | '`')) => {
                out.push('_');
                j += 2;
            }
            '\\' => {
                out.push('\\');
                if let Some(&n) = chars.get(j + 1) {
                    out.push(n);
                }
                j += 2;
            }
            c => {
                out.push(c);
                j += 1;
            }
        }
    }
    (j, false)
}

/// A `<<`/`<<-` opener at `chars[i]`: copy it verbatim, record its delimiter
/// (quotes and backslashes removed, as the shell does), and return the index
/// after the delimiter word.
fn read_here_doc_opener(
    chars: &[char],
    i: usize,
    pending: &mut Vec<Pending>,
    out: &mut String,
) -> usize {
    let mut j = i + 2;
    let strips_tabs = chars.get(j) == Some(&'-');
    if strips_tabs {
        j += 1;
    }
    while j < chars.len() && matches!(chars[j], ' ' | '\t') {
        j += 1;
    }
    let mut delimiter = String::new();
    let mut quoted = false;
    while j < chars.len()
        && !matches!(
            chars[j],
            ' ' | '\t' | '\n' | ';' | '&' | '|' | '<' | '>' | '(' | ')'
        )
    {
        if matches!(chars[j], '\'' | '"' | '\\') {
            quoted = true;
        } else {
            delimiter.push(chars[j]);
        }
        j += 1;
    }
    out.extend(&chars[i..j]);
    pending.push(Pending {
        delimiter,
        strips_tabs,
        quoted,
    });
    j
}

/// What [`copy_here_doc_bodies`] did with the pending bodies.
struct Bodies {
    /// The index after the last body (and its delimiter line).
    after: usize,
    /// Delimiters of the bodies the input ended inside.
    unterminated: Vec<String>,
    /// An unquoted body held a shape [`spell_expanding_body`] does not model.
    undetermined: bool,
}

/// After a code newline at `chars[from - 1]`: copy every pending here-document
/// body through its delimiter line (or to the end of the input when it is
/// unterminated or its delimiter is empty).
///
/// A QUOTED-delimiter body (`<<'EOF'`) is data and is copied verbatim. An
/// UNQUOTED one (`<<EOF`) is expanded by the shell, so it is neither plain
/// data nor code: its `'` and `"` are literal characters, while its `$(…)`,
/// `$((…))` and backtick spans are live command substitutions (observed:
/// zsh runs `` `touch M` `` in `echo $(cat <<EOF` / `)'` / `` `touch M` `` /
/// `EOF` / `)`, see `tests/heredoc_paren_bash32_verifier_20261107.rs` C1/C2).
/// The naive quote trackers read such a literal `'` as opening a quoted span
/// that hid the substitution after it, so the body is re-spelt by
/// [`spell_expanding_body`]: each literal quote becomes `_` and each
/// substitution is lexed as the code it is. In such a body a backslash-newline
/// joins two lines and the delimiter is matched against the joined line
/// (observed in /bin/bash 3.2, /bin/sh and /bin/zsh: `a\` / `EOF` / `b` /
/// `EOF` prints `aEOF` and `b`); a `<<-` body with such a join, a delimiter
/// line that is itself joined, and every shape [`spell_expanding_body`] does
/// not model set [`Bodies::undetermined`].
fn copy_here_doc_bodies(
    chars: &[char],
    from: usize,
    pending: &mut Vec<Pending>,
    out: &mut String,
    (bash32, respell): (bool, bool),
    depth: usize,
) -> Bodies {
    let mut pos = from;
    let mut unterminated = Vec::new();
    let mut undetermined = false;
    for doc in pending.drain(..) {
        let body_start = pos;
        // `(start, end)` of the terminator's physical line, once found.
        let mut terminator: Option<(usize, usize)> = None;
        let mut joined = false;
        while pos < chars.len() {
            let line_start = pos;
            let mut end = line_end(chars, pos);
            // An unquoted body joins a line ending in an odd number of
            // backslashes to the next one.
            let mut logical: Vec<char> = chars[pos..end].to_vec();
            let mut physical_lines = 1;
            while !doc.quoted && end < chars.len() && ends_in_continuation(&chars[pos..end]) {
                logical.pop();
                pos = end + 1;
                end = line_end(chars, pos);
                logical.extend_from_slice(&chars[pos..end]);
                physical_lines += 1;
            }
            joined |= physical_lines > 1;
            pos = end + 1;
            let check: String = if doc.strips_tabs {
                logical.iter().skip_while(|&&c| c == '\t').collect()
            } else {
                logical.iter().collect()
            };
            if !doc.delimiter.is_empty() && check == doc.delimiter {
                if physical_lines > 1 {
                    undetermined = true;
                }
                terminator = Some((line_start, end));
                break;
            }
        }
        let body_end = terminator.map_or(chars.len(), |(start, _)| start);
        let body = &chars[body_start..body_end.max(body_start)];
        if doc.quoted || !respell {
            out.extend(body);
            undetermined |= joined && doc.strips_tabs;
        } else {
            undetermined |= joined && doc.strips_tabs;
            match spell_expanding_body(body, bash32, depth) {
                Some(spelt) => out.push_str(&spelt),
                None => {
                    undetermined = true;
                    out.extend(body);
                }
            }
        }
        match terminator {
            Some((start, end)) => {
                out.extend(&chars[start..end]);
                if end < chars.len() {
                    out.push('\n');
                }
            }
            None => unterminated.push(doc.delimiter),
        }
    }
    Bodies {
        after: pos.min(chars.len()),
        unterminated,
        undetermined,
    }
}

/// The index of the newline ending the line that starts at `chars[pos]`, or
/// `chars.len()` for the last line.
fn line_end(chars: &[char], pos: usize) -> usize {
    chars[pos.min(chars.len())..]
        .iter()
        .position(|&c| c == '\n')
        .map_or(chars.len(), |p| pos + p)
}

/// True when `line` ends in an odd number of backslashes: in an unquoted
/// here-document body the last one escapes the newline (a line continuation).
fn ends_in_continuation(line: &[char]) -> bool {
    line.iter().rev().take_while(|&&c| c == '\\').count() % 2 == 1
}

/// The body of an UNQUOTED here-document (the text between its opener line
/// and its delimiter line) re-spelt for the naive quote trackers, or `None`
/// for a shape not modelled.
///
/// The shell expands such a body the way it expands a double-quoted string
/// whose `"` is not special: `'` and `"` are literal (spelt `_`, so that no
/// tracker opens a quoted span at them); `\` escapes only `$`, `` ` ``, `\`
/// and the newline (those pairs are copied, any other `\` is literal); and
/// `$(…)` / `$((…))` / backtick spans are command (or arithmetic)
/// substitutions, each lexed by [`lex`] as the code the shell runs — there a
/// quote IS a quote. A `$(` span is delimited by the modern here-document-aware
/// scan ([`scan_balanced_strict_modern`]) within the body (the shell reads the
/// body lines before it expands them, so no substitution reaches past the
/// delimiter line); a backtick span by the next unescaped backtick, its
/// content backslash-processed ([`backtick_body_as_run`]) before it is lexed.
/// `${…}` is copied verbatim when it holds none of `'`, `"`, `` ` ``, `\`,
/// `$`, `{` or a newline.
///
/// `None` (not modelled — the caller resolves it to Ask, never to clean): a
/// substitution that does not close inside the body, whose lexed content ends
/// inside a quote or a nested context or with a here-document still open, or
/// whose lexing meets an unmodelled shape; and every other `${…}`.
fn spell_expanding_body(body: &[char], bash32: bool, depth: usize) -> Option<String> {
    let mut out = String::with_capacity(body.len());
    let lexed_code = |code: &[char]| -> Option<String> {
        let inner = lex(code, bash32, true, depth + 1);
        (!inner.undetermined && !inner.open && inner.unterminated.is_empty()).then_some(inner.text)
    };
    let mut k = 0;
    while k < body.len() {
        let c = body[k];
        let next = body.get(k + 1).copied();
        match c {
            '\\' => {
                out.push(c);
                if let Some(n @ ('$' | '`' | '\\' | '\n')) = next {
                    out.push(n);
                    k += 2;
                } else {
                    k += 1;
                }
            }
            '\'' | '"' => {
                out.push('_');
                k += 1;
            }
            '`' => {
                let mut close = k + 1;
                while close < body.len() && body[close] != '`' {
                    close += if body[close] == '\\' { 2 } else { 1 };
                }
                if close >= body.len() {
                    return None;
                }
                let run: Vec<char> = backtick_body_as_run(&body[k + 1..close]).chars().collect();
                out.push('`');
                out.push_str(&lexed_code(&run)?);
                out.push('`');
                k = close + 1;
            }
            '$' if next == Some('(') => {
                let end = scan_balanced_strict_modern(body, k + 2, Stop::Paren, depth + 1)?;
                out.push_str(&lexed_code(&body[k..=end])?);
                k = end + 1;
            }
            '$' if next == Some('{') => {
                let close = body[k + 2..].iter().position(|&ch| ch == '}')? + k + 2;
                let span = &body[k..=close];
                if span[2..span.len() - 1]
                    .iter()
                    .any(|&ch| matches!(ch, '\'' | '"' | '`' | '\\' | '$' | '{' | '\n'))
                {
                    return None;
                }
                out.extend(span);
                k = close + 1;
            }
            _ => {
                out.push(c);
                k += 1;
            }
        }
    }
    Some(out)
}
