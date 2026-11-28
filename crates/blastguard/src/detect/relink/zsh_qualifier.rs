//! Words that run code while zsh EXPANDS them (backlog 3de4729f): a glob
//! qualifier `(e:…:)` / `(+fn)`, `=( )`, the `(e)` / `(%)` parameter flags,
//! `$~` / `${~` (GLOB_SUBST), and the operand of `print -P` under
//! `promptsubst`. The relink layer reads a line with such a word as an
//! unmodelled relinker (an Ask); this module only DECIDES whether a word is
//! one, from the word's spelling.
//!
//! The qualifier list is read with zsh's grammar (zshexpn "Glob Qualifiers"),
//! one flag at a time from the left, not by looking for letters:
//!
//! * a one-character flag taking no argument (`/ . @ = p * % r w x A I E R
//!   W X s S t f ^ - M T N D n F U G`, `%b`/`%c`/`%d`) is consumed;
//! * `u` `g` `P` take a number or a delimited string, `a` `m` `c` `L` an
//!   optional unit then an optional sign and a number, `o` `O` one sort
//!   character, `l` `d` an optional sign and a number, `Y` a number, `[…]` a
//!   subscript;
//! * `e` takes the next character — ANY character, a letter included — as
//!   its delimiter and runs the text up to the closing delimiter; `+` runs
//!   the function named after it. Both are code ([`Class::Code`]). The `+`
//!   of a `m` / `L` size or time argument is a sign, not a function;
//! * a character the grammar does not know, or a list that ends inside an
//!   argument, is [`Class::Unknown`] — undetermined, and so an Ask unless the
//!   group is shaped like a pattern (below).
//!
//! Which `(…)` is a qualifier list: the LAST group of a word (`(#q…)`
//! anywhere). A group written after `! @ + ? *` or holding a `|` is also a
//! ksh/bash extglob pattern, so there an unreadable list is a pattern, never
//! a reason to Ask; a list that READS as code is code in either spelling. A
//! group holding a quote, backslash, `$` or backtick cannot be read and is
//! code. `${ … }` bodies are read (their operand is globbed when unquoted);
//! a `$( )` body is skipped — its commands are words of their own in the
//! script. Over-approximates; it only adds Asks (CLAUDE.md §3).

use super::super::ast::{Quoting, SimpleCommand, Value, Word};

/// What a qualifier list does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    /// Every flag read, none runs code.
    Plain,
    /// An `e` or `+fn` flag: runs code.
    Code,
    /// The grammar stopped understanding the list.
    Unknown,
}

/// Whether the shell word `raw` (as written) may run code while zsh
/// expands it. See the module doc.
pub(super) fn word_evaluates(raw: &str) -> bool {
    let chars: Vec<char> = raw.chars().collect();
    scan(&chars, false)
}

/// Scan one word (or one `${ }` operand): unquoted constructs outside every
/// quote, with `quoted` set when the text sits inside double quotes.
fn scan(chars: &[char], quoted: bool) -> bool {
    let (mut in_s, mut in_d) = (false, quoted);
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        if in_s {
            in_s = c != '\'';
            i += 1;
            continue;
        }
        match c {
            '\\' => i += 1,
            '\'' if !in_d => {
                in_s = true;
                // `$'…'`: a backslash escapes inside it.
                if i > 0 && chars[i - 1] == '$' {
                    i += 1;
                    while i < chars.len() && chars[i] != '\'' {
                        i += if chars[i] == '\\' { 2 } else { 1 };
                    }
                    in_s = false;
                }
            }
            '"' => in_d = !in_d,
            '`' => {
                i += 1;
                while i < chars.len() && chars[i] != '`' {
                    i += if chars[i] == '\\' { 2 } else { 1 };
                }
            }
            '$' if next == Some('~') => return true,
            '$' if next == Some('{') && matches!(chars.get(i + 2), Some('~' | '(')) => return true,
            '$' if next == Some('{') => {
                let end = skip_group(chars, i + 1, '{', '}');
                // An unclosed `${` cannot be read.
                if chars.get(end - 1) != Some(&'}') || end <= i + 2 {
                    return true;
                }
                if scan(operand(&chars[i + 2..end - 1]), in_d) {
                    return true;
                }
                i = end;
                continue;
            }
            '$' | '<' | '>' if next == Some('(') && (c == '$' || !in_d) => {
                i = skip_group(chars, i + 1, '(', ')');
                continue;
            }
            // A word-leading `=(` is a process substitution.
            '(' if !in_d && i == 1 && chars[0] == '=' => return true,
            '(' if !in_d && i > 0 => {
                let end = skip_group(chars, i, '(', ')');
                if chars.get(end - 1) != Some(&')') || end <= i + 1 {
                    return true;
                }
                let inner = &chars[i + 1..end - 1];
                let hashq = inner.starts_with(&['#', 'q']);
                if (end == chars.len() || hashq) && group_runs_code(inner, chars[i - 1], hashq) {
                    return true;
                }
                i = end;
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    false
}

/// The operand of a `${ … }` body (the text after its operator), or the whole
/// body when it has none (`${arr[(r)x]}` holds no globbable operand).
fn operand(body: &[char]) -> &[char] {
    let at = |p: &[char]| body.windows(p.len()).position(|w| w == p);
    for op in [[':', '-'], [':', '='], [':', '+'], [':', '?']] {
        if let Some(p) = at(&op) {
            return &body[p + 2..];
        }
    }
    // `-`, `=`, `+`, `?`, `%`, `#`, `/` after the name (not the first char).
    match body.iter().skip(1).position(|c| "-=+?%#/".contains(*c)) {
        Some(p) => {
            let mut s = p + 2;
            while body.get(s) == body.get(s - 1) && s < body.len() {
                s += 1;
            }
            &body[s.min(body.len())..]
        }
        None => body,
    }
}

/// Whether the `(…)` group `inner`, written right after `prev`, runs code.
fn group_runs_code(inner: &[char], prev: char, hashq: bool) -> bool {
    let list = if hashq { &inner[2..] } else { inner };
    if list.iter().any(|c| "'\"\\$`".contains(*c)) {
        return true;
    }
    match classify(list) {
        Class::Code => true,
        Class::Plain => false,
        Class::Unknown => {
            let pattern_shaped = !hashq && ("!@+?*".contains(prev) || list.contains(&'|'));
            !pattern_shaped
        }
    }
}

/// Read a qualifier list with the grammar of the module doc.
fn classify(list: &[char]) -> Class {
    let mut i = 0;
    let at = |k: usize| list.get(k).copied();
    while i < list.len() {
        let c = list[i];
        i += 1;
        match c {
            '/'
            | '.'
            | '@'
            | '='
            | 'p'
            | '*'
            | 'r'
            | 'w'
            | 'x'
            | 'A'
            | 'I'
            | 'E'
            | 'R'
            | 'W'
            | 'X'
            | 's'
            | 'S'
            | 't'
            | 'f'
            | '^'
            | '-'
            | 'M'
            | 'T'
            | 'N'
            | 'D'
            | 'n'
            | 'F'
            | 'U'
            | 'G'
            | ','
            | '0'..='9' => {}
            '%' => {
                if matches!(at(i), Some('b' | 'c' | 'd')) {
                    i += 1;
                }
            }
            'e' => {
                return if i < list.len() {
                    Class::Code
                } else {
                    Class::Unknown
                }
            }
            '+' => {
                return if at(i).is_some_and(|n| n.is_alphanumeric() || n == '_') {
                    Class::Code
                } else {
                    Class::Unknown
                }
            }
            'o' | 'O' => match at(i) {
                Some('e') => return Class::Code,
                Some(_) => i += 1,
                None => return Class::Unknown,
            },
            'u' | 'g' | 'P' => match at(i) {
                Some(d) if d.is_ascii_digit() => {
                    while at(i).is_some_and(|n| n.is_ascii_digit()) {
                        i += 1;
                    }
                }
                Some(d) => match find_closer(list, i + 1, closer(d)) {
                    Some(k) => i = k + 1,
                    None => return Class::Unknown,
                },
                None => return Class::Unknown,
            },
            'a' | 'm' | 'c' | 'L' => {
                let units = if c == 'L' { "pPkKmMgGtT" } else { "Mwhms" };
                if at(i).is_some_and(|n| units.contains(n)) {
                    i += 1;
                }
                if matches!(at(i), Some('+' | '-')) {
                    i += 1;
                }
                while at(i).is_some_and(|n| n.is_ascii_digit()) {
                    i += 1;
                }
            }
            'l' | 'd' => {
                if matches!(at(i), Some('+' | '-')) {
                    i += 1;
                }
                while at(i).is_some_and(|n| n.is_ascii_digit()) {
                    i += 1;
                }
            }
            'Y' => {
                while at(i).is_some_and(|n| n.is_ascii_digit()) {
                    i += 1;
                }
            }
            '[' => match find_closer(list, i, ']') {
                Some(k) => i = k + 1,
                None => return Class::Unknown,
            },
            _ => return Class::Unknown,
        }
    }
    Class::Plain
}

/// The closing delimiter for an opening one (`[ { < (` pair up).
fn closer(open: char) -> char {
    match open {
        '[' => ']',
        '{' => '}',
        '<' => '>',
        '(' => ')',
        o => o,
    }
}

/// The index of the first `close` at or after `from`.
fn find_closer(list: &[char], from: usize, close: char) -> Option<usize> {
    (from..list.len()).find(|&k| list[k] == close)
}

/// Index just past the `close` matching the `open` at `at` (the end when
/// none), skipping quoted text and backslash escapes.
fn skip_group(chars: &[char], at: usize, open: char, close: char) -> usize {
    let (mut depth, mut i) = (0usize, at);
    let (mut in_s, mut in_d) = (false, false);
    while i < chars.len() {
        let c = chars[i];
        if in_s {
            in_s = c != '\'';
        } else if c == '\\' {
            i += 1;
        } else if c == '\'' && !in_d {
            in_s = true;
        } else if c == '"' {
            in_d = !in_d;
        } else if !in_d && c == open {
            depth += 1;
        } else if !in_d && c == close {
            depth -= 1;
            if depth == 0 {
                return i + 1;
            }
        }
        i += 1;
    }
    chars.len()
}

/// Precommands that run the command after them with no change to its words.
const PRECOMMANDS: &[&str] = &["builtin", "noglob", "command", "exec"];

/// `print -P …` expands prompt escapes and, under `promptsubst`, runs a
/// `$( )` / backtick / `${ }` held in its operand whatever the quoting, and
/// the value of an unquoted `$NAME`. Reached through `builtin` / `noglob` /
/// `command` / `exec` too; `-rP` style flag clusters count.
pub(super) fn prompt_expands_substitution(c: &SimpleCommand) -> bool {
    let lit = |w: &Word| match &w.value {
        Value::Literal(s) => s.clone(),
        Value::Undetermined(_) => w.raw.clone(),
    };
    let mut k = 0;
    while c
        .argv
        .get(k)
        .map(lit)
        .is_some_and(|s| PRECOMMANDS.contains(&s.as_str()))
    {
        k += 1;
    }
    if c.argv.get(k).map(lit).as_deref() != Some("print") {
        return false;
    }
    let args = &c.argv[k + 1..];
    let flags_p = args.iter().any(|w| {
        let s = lit(w);
        s.starts_with('-') && s.contains('P')
    });
    flags_p
        && args.iter().any(|w| {
            let unquoted_name = w.quoting == Quoting::Unquoted
                && w.raw
                    .split('$')
                    .skip(1)
                    .any(|r| r.starts_with(|n: char| n.is_alphabetic() || n == '_'));
            [&w.raw, &lit(w)]
                .iter()
                .any(|s| s.contains("$(") || s.contains('`') || s.contains("${"))
                || unquoted_name
        })
}
