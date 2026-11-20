//! The OPTION words the legacy rule arms read, with their shell quoting
//! removed (raw-token quoted-option fail-open, 2026-11-20, critical).
//!
//! The legacy arms judge whitespace-split tokens of the segment text and match
//! options by spelling: `t.starts_with("--output=")`, `*t == "-i"`,
//! `*t == "--force"`, `t.starts_with("--command=")`. The shell removes quotes
//! before the program sees its argv, so `sort '--output=/etc/passwd' x`,
//! `sed \--in-place -e d /etc/hosts`, `git push "--force" origin main` and
//! `flock '--command=rm -rf /' /tmp/l` hand the program exactly the option
//! their unquoted twins do — but the raw token `'--output=/etc/passwd'`
//! matched no arm and every one of those lines was Allow (55 measured REDs in
//! `tests/raw_token_quoted_option_20261120.rs`).
//!
//! Fixed in ONE place, not per arm: the token lists the legacy arms are handed
//! ([`super::analyze_segment`], the high-blast tier and the pipe-egress
//! stages) are passed through [`OptionWords::new`], which rewrites every token
//! that STARTS a shell word whose quote-removed text begins with `-` into the
//! spelling of its unquoted twin:
//!
//! * the option NAME — the word up to and including its first `=`, or the
//!   whole word when it has none — is dequoted by the AST parser
//!   ([`super::redirect::dequoted_redirect_word`], the same dequoting the
//!   redirect and `cd` arms use);
//! * the option VALUE after the `=` keeps its shell spelling, re-opened with
//!   the quote that was open at the `=`: `'--command=rm` becomes
//!   `--command='rm` and `"--eval=require('fs')…"` becomes
//!   `--eval="require('fs')…"` — exactly the unquoted-name spelling, whose
//!   value every arm already reads with its own quote handling (reading the
//!   value dequoted would dequote it twice). An empty quoted value is dropped
//!   (`'--in-place='` becomes `--in-place=`).
//!
//! Tokens that are not rewritten keep their exact bytes: a word whose text
//! does not begin with `-` (an operand: paths, globs and messages keep the
//! quote-sensitive readings the arms make of them), a token inside a word
//! that started earlier (`-m "use '-f' here"` — `'-f'` is message text, not
//! an option), and an unquoted option (`--output='/x'` already reads as its
//! own spelling). A rewritten token that is a contiguous part of the raw token
//! (`'-i'` → `-i`, `\--force` → `--force`) stays a slice of the segment text,
//! so the readers that locate tokens inside it ([`super::payload::faithful_tail`])
//! still find it; only a re-opened value is a new string, and for those the
//! readers fall back to their whitespace re-join (where that loses line
//! breaks they already answer Ask, `word_separators_lost`).
//!
//! UNDETERMINED (CLAUDE.md §3): a word whose literal text begins with `-` but
//! whose option name cannot be reduced to one literal — an expansion inside
//! the quoted name (`"-$x"`), an ANSI-C escape this module does not decode
//! (`$'\x2di'`; the control escapes `\n` `\t` … decode to a non-dash and are
//! read), an escaped word separator after a dash — could be any option, so
//! [`OptionWords::undetermined`] carries an Ask that the segment analysis
//! records; it is never read as "no option". A word whose first character is
//! an expansion (`"$OPT"`) is NOT in this class: its unquoted twin `$OPT`
//! reads the same, and both are left to the arms exactly as before.
//!
//! DOES NOT COVER: quoting that starts after the first character of the name
//! (`--out'put'=…`, `-'i'`) — the token is left as it is (its raw spelling
//! already starts with `-`, and the arms read the prefix they see); options
//! assembled from variables.

use std::borrow::Cow;

use super::payload::shell_word_token_span;
use super::redirect::dequoted_redirect_word;
use super::Decision;
use harness_core::verdict::Determination;

/// The tokens of one segment with every quoted option word rewritten to its
/// unquoted spelling (see the module doc), plus the Ask for an option word
/// that could not be dequoted.
pub(super) struct OptionWords<'a> {
    words: Vec<Cow<'a, str>>,
    undetermined: Option<Decision>,
}

impl<'a> OptionWords<'a> {
    /// Rewrite the quoted option words among `tokens` (whitespace-split
    /// fragments of one segment). Token count and order never change, so an
    /// index into the result is an index into `tokens`.
    pub(super) fn new(tokens: &[&'a str]) -> Self {
        let mut words: Vec<Cow<'a, str>> = tokens.iter().map(|t| Cow::Borrowed(*t)).collect();
        let mut undetermined = None;
        let mut at = 0;
        while at < tokens.len() {
            match option_word(tokens[at]) {
                OptionWord::Same => {}
                OptionWord::Rewritten(w) => words[at] = w,
                OptionWord::Undetermined(why) => {
                    undetermined.get_or_insert_with(|| Decision::ask(why));
                }
            }
            // The next token that STARTS a word; a word whose quote never
            // closes swallows every later token (none of them starts a word).
            match shell_word_token_span(tokens, at) {
                Some(span) => at += span,
                None => break,
            }
        }
        Self {
            words,
            undetermined,
        }
    }

    /// The rewritten tokens, as the `&[&str]` the arms take.
    pub(super) fn tokens(&self) -> Vec<&str> {
        self.words.iter().map(AsRef::as_ref).collect()
    }

    /// The Ask for a word that begins with `-` and whose option name could not
    /// be dequoted; `None` when every option word was read.
    pub(super) fn undetermined(&self) -> Option<Decision> {
        self.undetermined.clone()
    }
}

enum OptionWord<'a> {
    /// Not a quoted option word: the token keeps its bytes.
    Same,
    /// The unquoted-name spelling of the option word.
    Rewritten(Cow<'a, str>),
    /// Begins with `-`, but the name could not be dequoted (reason).
    Undetermined(String),
}

/// An open quote of the lexer: its opener as spelt and its closing char.
#[derive(Clone, Copy)]
enum Open {
    Single,
    Double,
    AnsiC,
}

impl Open {
    fn opener(self) -> &'static str {
        match self {
            Open::Single => "'",
            Open::Double => "\"",
            Open::AnsiC => "$'",
        }
    }
    fn closer(self) -> char {
        match self {
            Open::Double => '"',
            Open::Single | Open::AnsiC => '\'',
        }
    }
}

/// Where the option name of `tok` ends and what the lexer saw on the way.
struct NameScan {
    /// Byte index just past the name (past its first `=`, else `tok.len()`).
    cut: usize,
    /// The quote still open at `cut`.
    open: Option<Open>,
    /// Whether any quote or backslash was removed inside the name.
    quoted: bool,
    /// Whether the name holds a `$'…'` run (not handed to the AST dequoter).
    ansi_c: bool,
    /// The name with its quoting removed, as this lexer reads it.
    name: String,
    /// Why the name cannot be reduced to one literal, when it cannot.
    undetermined: Option<&'static str>,
}

impl NameScan {
    /// Push one literal character of the name; `true` when it ends the name.
    fn lit(&mut self, c: char) -> bool {
        self.name.push(c);
        c == '='
    }
}

/// Lex the name part of one token (a word start) with the shell's quoting
/// rules: `'…'`, `"…"` (with `\"`, `\\`, `\$`, `` \` `` escapes), `$'…'`
/// (only the escapes `\\`, `\'`, `\"`, `\?` decoded), `$"…"` (read as `"…"`)
/// and a bare backslash escape.
fn scan_name(tok: &str) -> NameScan {
    let mut s = NameScan {
        cut: tok.len(),
        open: None,
        quoted: false,
        ansi_c: false,
        name: String::new(),
        undetermined: None,
    };
    let mut it = tok.char_indices().peekable();
    let end = |j: usize, ch: char| j + ch.len_utf8();
    while let Some((i, c)) = it.next() {
        let name_ends = match (s.open, c) {
            (None, '\\') => {
                s.quoted = true;
                match it.next() {
                    Some((j, d)) => s.lit(d).then(|| end(j, d)),
                    None => {
                        // An escaped word separator: the word continues into
                        // the next token, so this token is not the whole name.
                        // A lone `\` (the `\;` of `find -exec`, cut at the
                        // `;`) starts no option.
                        if !s.name.is_empty() {
                            s.undetermined = Some("an escaped blank splits the word");
                        }
                        None
                    }
                }
            }
            (None, '\'') => {
                s.quoted = true;
                s.open = Some(Open::Single);
                None
            }
            (None, '"') => {
                s.quoted = true;
                s.open = Some(Open::Double);
                None
            }
            (None, '$') if matches!(it.peek(), Some((_, '\'' | '"'))) => {
                s.quoted = true;
                let (_, q) = it.next().unwrap_or((i, '\''));
                s.open = Some(if q == '"' {
                    Open::Double
                } else {
                    s.ansi_c = true;
                    Open::AnsiC
                });
                None
            }
            (None | Some(Open::Double), '$' | '`') => {
                s.undetermined = Some("it holds a shell expansion");
                s.lit(c).then(|| end(i, c))
            }
            (Some(q), c) if c == q.closer() => {
                s.open = None;
                None
            }
            (Some(Open::Double), '\\') => match it.next() {
                Some((j, d @ ('"' | '\\' | '$' | '`'))) => s.lit(d).then(|| end(j, d)),
                Some((j, d)) => {
                    s.lit('\\');
                    s.lit(d).then(|| end(j, d))
                }
                None => {
                    s.lit('\\');
                    None
                }
            },
            (Some(Open::AnsiC), '\\') => match it.next() {
                Some((j, d @ ('\\' | '\'' | '"' | '?'))) => s.lit(d).then(|| end(j, d)),
                // Control-character escapes: never a dash or an `=`.
                Some((_, d @ ('n' | 't' | 'r' | 'a' | 'b' | 'e' | 'E' | 'f' | 'v'))) => {
                    s.lit(match d {
                        'n' => '\n',
                        't' => '\t',
                        'r' => '\r',
                        _ => '\u{7}',
                    });
                    None
                }
                // `\xHH`, `\nnn`, `\uHHHH`, `\cX`: could spell a dash.
                _ => {
                    s.undetermined = Some("it holds an ANSI-C escape blastguard does not decode");
                    None
                }
            },
            (_, c) => s.lit(c).then(|| end(i, c)),
        };
        if let Some(cut) = name_ends {
            s.cut = cut;
            return s;
        }
        // What follows an undecodable character is not the name's text.
        if s.undetermined.is_some() {
            return s;
        }
    }
    s
}

fn option_word(tok: &str) -> OptionWord<'_> {
    if !tok.contains(['\\', '\'', '"']) {
        return OptionWord::Same;
    }
    // Quoting that starts after the first character of the name is out of
    // scope (module doc): the raw token already begins with the dash.
    if tok.starts_with('-') {
        return OptionWord::Same;
    }
    let scan = scan_name(tok);
    // "Could be an option": the dash is literally there, or the very first
    // character could not be decoded at all (`$'\x2d…'`).
    let could_be_option = scan.name.starts_with('-') || scan.name.is_empty();
    if !scan.quoted || !could_be_option {
        return OptionWord::Same;
    }
    let undetermined = |why: &str| {
        OptionWord::Undetermined(format!(
            "the quoted option word `{tok}` could not be dequoted to one literal option ({why}) — \
blastguard cannot tell which option it passes, so it refuses to guess"
        ))
    };
    if let Some(why) = scan.undetermined {
        return undetermined(why);
    }
    if scan.name.is_empty() {
        // Nothing but quotes (`''`, `"`): no option, an operand.
        return OptionWord::Same;
    }
    // The AST parser's dequoting of the (closed) name must agree with this
    // lexer's; `$'…'` is decoded by the lexer alone.
    if !scan.ansi_c {
        let raw_name = &tok[..scan.cut];
        let closed_name = match scan.open {
            Some(q) => format!("{raw_name}{}", q.closer()),
            None => raw_name.to_string(),
        };
        match dequoted_redirect_word(&closed_name) {
            Determination::Known(name) if name == scan.name => {}
            Determination::Known(_) => {
                return undetermined("the shell parser reads its quoting differently")
            }
            Determination::Undetermined(_) => {
                return undetermined("the shell parser could not reduce it to one word")
            }
        }
    }
    let name = scan.name.as_str();
    let suffix = &tok[scan.cut..];
    let value = match scan.open {
        // The quote opened before `=` is still open: re-open it for the value,
        // unless the value is just its closing quote (an empty value).
        Some(q) if suffix.len() == 1 && suffix.starts_with(q.closer()) => String::new(),
        Some(q) => format!("{}{suffix}", q.opener()),
        None => suffix.to_string(),
    };
    let rewritten = format!("{name}{value}");
    if rewritten == tok {
        return OptionWord::Same;
    }
    // Keep it a slice of the segment text when it is one (module doc).
    match tok.find(rewritten.as_str()) {
        Some(p) => OptionWord::Rewritten(Cow::Borrowed(&tok[p..p + rewritten.len()])),
        None => OptionWord::Rewritten(Cow::Owned(rewritten)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rw(tok: &str) -> String {
        match option_word(tok) {
            OptionWord::Same => format!("SAME {tok}"),
            OptionWord::Rewritten(w) => w.into_owned(),
            OptionWord::Undetermined(_) => "UNDETERMINED".into(),
        }
    }

    #[test]
    fn quoted_option_names_are_dequoted_and_values_keep_their_quote() {
        assert_eq!(rw("'-i'"), "-i");
        assert_eq!(rw("\"--force\""), "--force");
        assert_eq!(rw("\\--force"), "--force");
        assert_eq!(rw("'--in-place='"), "--in-place=");
        assert_eq!(rw("'--command=rm"), "--command='rm");
        assert_eq!(rw("'--output=/etc/passwd'"), "--output='/etc/passwd'");
        assert_eq!(rw("\"--eval=require('fs')\""), "--eval=\"require('fs')\"");
        assert_eq!(rw("$'-i'"), "-i");
    }

    #[test]
    fn operands_unquoted_options_and_expansions_first_are_left_alone() {
        assert_eq!(rw("'a b'"), "SAME 'a b'");
        assert_eq!(rw("--output='/x'"), "SAME --output='/x'");
        assert_eq!(rw("\"$OPT\""), "SAME \"$OPT\"");
        assert_eq!(rw("-x"), "SAME -x");
        assert_eq!(rw("'.claude/*'"), "SAME '.claude/*'");
        // `find … -exec … \;` reaches a segment as a lone `\`.
        assert_eq!(rw("\\"), "SAME \\");
        assert_eq!(rw("$'\\n'"), "SAME $'\\n'");
    }

    #[test]
    fn an_option_name_that_cannot_be_dequoted_is_undetermined() {
        assert_eq!(rw("\"-$x\""), "UNDETERMINED");
        assert_eq!(rw("$'\\x2di'"), "UNDETERMINED");
    }

    #[test]
    fn a_token_inside_an_earlier_word_is_not_an_option_word() {
        let toks = ["-m", "\"use", "'-f'", "here\""];
        let w = OptionWords::new(&toks);
        assert_eq!(w.tokens(), toks.to_vec());
        assert!(w.undetermined().is_none());
    }
}
