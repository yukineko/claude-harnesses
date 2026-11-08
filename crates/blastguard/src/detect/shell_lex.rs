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
//! [`shell_lexed_reading`] re-spells the line the way the shell tokenises it,
//! so that the naive trackers read it correctly: every `#` comment is removed
//! (its newline kept) and every backslash-escaped `'`/`"`/`` ` `` — one the
//! shell reads as a literal character, never as a quote — becomes `_`. The
//! caller ([`super::readings::detect_bash_every_reading`]) judges that text IN
//! ADDITION to the original, the most restrictive verdict winning, so this
//! reading can only add a verdict, never remove one.
//!
//! Modelled: single quotes, `$'…'` (ANSI-C, where `\'` is escaped), double
//! quotes, backticks, `$(…)`/`<(…)`/`>(…)`/`(…)` nesting (each a fresh code
//! context, as in the shell), `$((…))` arithmetic (no comments there), and
//! here-document bodies (copied verbatim: they are data, an apostrophe in them
//! is not a quote — the existing body handling of the splitters still applies).
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
    backtick_body_as_run, entry_comments, scan_bash32, Scan32, Stop, MAX_QUOTE_PAREN_DEPTH,
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
}

/// `cmd` re-spelled as the shell tokenises it (see the module doc), or `None`
/// when that spelling is identical to `cmd` (nothing to add).
pub(super) fn shell_lexed_reading(cmd: &str) -> Option<String> {
    let chars: Vec<char> = cmd.chars().collect();
    // Only the bash 3.2 branches of the lexer ever raise `undetermined`.
    let lexed = lex(&chars, false, 0);
    (lexed.text != cmd).then_some(lexed.text)
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
    let lexed = lex(&chars, true, 0);
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
    /// A shape this lexer does not model was met (bash 3.2 mode only).
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

/// The lexer behind [`shell_lexed_reading`] (`bash32 == false`) and
/// [`bash32_lexed_reading`] (`bash32 == true`).
fn lex(chars: &[char], bash32: bool, depth: usize) -> Lexed {
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
                '\\' if matches!(next, Some('"' | '`')) => {
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
                let (after, closed) = copy_ansi_c(chars, i, &mut out);
                open_quote |= !closed;
                i = after;
                continue;
            }
            '"' => {
                stack.push(Lex::Double);
                out.push(c);
            }
            '\\' if matches!(next, Some('\'' | '"' | '`')) => {
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
            '#' if top != Lex::Arith && starts_word(chars, i) => {
                // A comment runs to the newline (kept), or — inside
                // backticks, whose body the shell cuts out first — to the
                // closing backtick.
                let mut j = i;
                while j < chars.len()
                    && chars[j] != '\n'
                    && !(top == Lex::Backtick && chars[j] == '`')
                {
                    j += 1;
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
                let (after, open) = copy_here_doc_bodies(chars, i, &mut pending, &mut out);
                unterminated.extend(open);
                i = after;
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
    let inner = lex(content, true, depth + 1);
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

/// True when `chars[i]` begins a word, which is where `#` starts a comment.
fn starts_word(chars: &[char], i: usize) -> bool {
    i == 0
        || matches!(
            chars[i - 1],
            ' ' | '\t' | '\n' | ';' | '&' | '|' | '(' | ')' | '<' | '>'
        )
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

/// `$'…'` at `chars[i]`: copy it with each escaped quote spelt `_`, and return
/// the index after the closing `'` (or the end of the input) and whether that
/// closing `'` was found.
fn copy_ansi_c(chars: &[char], i: usize, out: &mut String) -> (usize, bool) {
    out.push_str("$'");
    let mut j = i + 2;
    while j < chars.len() {
        match chars[j] {
            '\'' => {
                out.push('\'');
                return (j + 1, true);
            }
            '\\' if matches!(chars.get(j + 1), Some('\'' | '"' | '`')) => {
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
    while j < chars.len()
        && !matches!(
            chars[j],
            ' ' | '\t' | '\n' | ';' | '&' | '|' | '<' | '>' | '(' | ')'
        )
    {
        if !matches!(chars[j], '\'' | '"' | '\\') {
            delimiter.push(chars[j]);
        }
        j += 1;
    }
    out.extend(&chars[i..j]);
    pending.push(Pending {
        delimiter,
        strips_tabs,
    });
    j
}

/// After a code newline at `chars[from - 1]`: copy every pending here-document
/// body verbatim through its delimiter line (or to the end of the input when
/// it is unterminated or its delimiter is empty), and return the index after
/// together with the delimiters of the bodies the input ended inside.
fn copy_here_doc_bodies(
    chars: &[char],
    from: usize,
    pending: &mut Vec<Pending>,
    out: &mut String,
) -> (usize, Vec<String>) {
    let mut pos = from;
    let mut unterminated = Vec::new();
    for doc in pending.drain(..) {
        let mut terminated = false;
        while pos < chars.len() {
            let end = chars[pos..]
                .iter()
                .position(|&c| c == '\n')
                .map_or(chars.len(), |p| pos + p);
            let line: String = chars[pos..end].iter().collect();
            out.push_str(&line);
            if end < chars.len() {
                out.push('\n');
            }
            pos = end + 1;
            let check = if doc.strips_tabs {
                line.trim_start_matches('\t')
            } else {
                line.as_str()
            };
            if !doc.delimiter.is_empty() && check == doc.delimiter {
                terminated = true;
                break;
            }
        }
        if !terminated {
            unterminated.push(doc.delimiter);
        }
    }
    (pos.min(chars.len()), unterminated)
}
