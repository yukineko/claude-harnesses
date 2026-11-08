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

/// The lexer context the current character is read in.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Lex {
    /// Ordinary shell code: top level, `$(…)`, `<(…)`, `(…)`.
    Paren,
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
    let mut out = String::with_capacity(cmd.len());
    // The bottom context is the top level; it is never popped.
    let mut stack: Vec<Lex> = vec![Lex::Paren];
    let mut pending: Vec<Pending> = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let top = *stack.last().unwrap_or(&Lex::Paren);
        let next = chars.get(i + 1).copied();
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
                    i = open_dollar_paren(&chars, i, &mut stack, &mut out);
                    continue;
                }
                _ => out.push(c),
            }
            i += 1;
            continue;
        }
        // Code contexts: Paren, Arith, Backtick.
        match c {
            '\'' => {
                // A single-quoted span is copied whole; an unterminated one
                // runs to the end (the original reading sees the same).
                let end = chars[i + 1..]
                    .iter()
                    .position(|&q| q == '\'')
                    .map_or(chars.len(), |p| i + 1 + p + 1);
                out.extend(&chars[i..end]);
                i = end;
                continue;
            }
            '$' if next == Some('\'') => {
                i = copy_ansi_c(&chars, i, &mut out);
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
                i = open_dollar_paren(&chars, i, &mut stack, &mut out);
                continue;
            }
            '(' => {
                stack.push(if top == Lex::Arith {
                    Lex::Arith
                } else {
                    Lex::Paren
                });
                out.push(c);
            }
            ')' => {
                if stack.len() > 1 && matches!(top, Lex::Paren | Lex::Arith) {
                    stack.pop();
                }
                out.push(c);
            }
            '#' if top != Lex::Arith && starts_word(&chars, i) => {
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
                i = read_here_doc_opener(&chars, i, &mut pending, &mut out);
                continue;
            }
            '\n' => {
                out.push(c);
                i += 1;
                i = copy_here_doc_bodies(&chars, i, &mut pending, &mut out);
                continue;
            }
            _ => out.push(c),
        }
        i += 1;
    }
    (out != cmd).then_some(out)
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
/// the index after the closing `'` (or the end of the input).
fn copy_ansi_c(chars: &[char], i: usize, out: &mut String) -> usize {
    out.push_str("$'");
    let mut j = i + 2;
    while j < chars.len() {
        match chars[j] {
            '\'' => {
                out.push('\'');
                return j + 1;
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
    j
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
/// it is unterminated or its delimiter is empty), and return the index after.
fn copy_here_doc_bodies(
    chars: &[char],
    from: usize,
    pending: &mut Vec<Pending>,
    out: &mut String,
) -> usize {
    let mut pos = from;
    for doc in pending.drain(..) {
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
                break;
            }
        }
    }
    pos.min(chars.len())
}
