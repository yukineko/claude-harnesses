//! Shell-word reading of `rm` operands for the system-harm check
//! ([`super::rm_system_harm_deny`], backlog 3c750df2 g3).
//!
//! The segment tokens `analyze_rm` receives are whitespace splits of the raw
//! text, so they still carry their quoting: `"/etc/passwd"`, `'/bin/ls'`,
//! `/bi"n"/ls`, `$'/etc/passwd'`, `/etc/{passwd,hosts}`. Read as a path, each
//! of those is "not a literal path" and the system-directory check skipped it,
//! which made `rm "/etc/passwd"` Allow. This module turns one operand into the
//! path(s) the shell would hand `rm`:
//!
//!   * quote removal — `'…'`, `"…"`, `$'…'` (ANSI-C escapes, including `\xHH`
//!     and octal), `$"…"`, backslash escapes, and concatenated pieces;
//!   * brace expansion — comma lists (`{a,b}`, nested, several groups) are
//!     expanded; a sequence group (`{1..9}`) can only produce letters or
//!     digits, so it is replaced by the glob `*`, which the caller judges by
//!     its literal directory prefix;
//!   * `~`, `$HOME`, `${HOME}` expanded against the session's home.
//!
//! Anything else that the shell would substitute at run time — another
//! variable, `$(…)`, a backtick, `~user`, an ANSI-C escape this module does not
//! model, unbalanced quoting, a brace list that explodes past
//! [`MAX_EXPANSIONS`] — is [`OperandSpelling::Unresolvable`]. The caller does
//! not skip such an operand: what `rm` will delete is unknown, and unknown is
//! resolved to the restrictive side (Deny).

/// Upper bound on the number of words one operand may brace-expand into.
/// Beyond it the operand is [`OperandSpelling::Unresolvable`] (restrictive).
const MAX_EXPANSIONS: usize = 256;

/// What one `rm` operand spells, as far as its text determines it.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum OperandSpelling {
    /// The path(s) the shell passes to `rm`. A path may still contain glob
    /// characters (unquoted ones, quoted ones, or a sequence group); the
    /// caller judges those by their literal prefix, which is restrictive.
    /// A home-relative spelling that climbs above home (`~/../../etc/x`)
    /// carries an extra candidate rooted at `/` (`/etc/x`): the depth of the
    /// home is not something the spelling's author has to know, so the climb
    /// is judged as reaching the root.
    Paths(Vec<String>),
    /// The text does not determine what `rm` receives; the reason says why.
    Unresolvable(String),
}

/// The operand words of an `rm` argument list: `rest` re-split on whitespace
/// outside quotes, backticks and `(…)` (so `"/a b"` and `$(echo /x)` stay one
/// word each), with option words (leading `-`) dropped exactly as
/// `analyze_rm` drops them. Redirections (a word with an UNQUOTED `<`/`>`:
/// `2>/dev/null`, `>&2`, and the target word after a bare `>`/`2>`) are not
/// `rm` operands and are dropped too; their targets are judged by the
/// redirect rules. A word whose quoting is unbalanced is kept (it is then
/// [`OperandSpelling::Unresolvable`]).
pub(super) fn rm_operand_words(rest: &[&str]) -> Vec<String> {
    let mut out = Vec::new();
    let mut skip_next = false;
    for w in crate::detect::vars::quote_aware_words(&rest.join(" ")) {
        if std::mem::take(&mut skip_next) {
            continue;
        }
        let chars: Vec<char> = w.chars().collect();
        let redirect = unquoted_mask(&chars).is_ok_and(|mask| {
            chars
                .iter()
                .zip(&mask)
                .any(|(c, unquoted)| *unquoted && matches!(c, '<' | '>'))
        });
        if redirect {
            skip_next = !w.is_empty()
                && w.ends_with(['<', '>'])
                && w.chars().all(|c| matches!(c, '<' | '>' | '&' | '0'..='9'));
            continue;
        }
        if !w.starts_with('-') {
            out.push(w);
        }
    }
    out
}

/// `op` (a quote-removed operand, no glob) as an absolute, lexically
/// normalised path: made absolute against `base`, `//` and `.` collapsed,
/// `..` resolved (`/..` is `/`). Unlike `SafeRoots::lexical_absolute` it
/// accepts the characters a quoted literal may contain (space, `(`, `'`, `$`,
/// …), because quote removal has already run and no expansion is left in
/// `op`. See [`Placed`] for the other two answers.
pub(super) fn place_literal(op: &str, base: Option<&str>) -> Placed {
    if op.is_empty() {
        return Placed::NotLiteral("empty operand".to_string());
    }
    if op.chars().any(|c| c.is_control() || c == '{' || c == '}') {
        return Placed::NotLiteral("operand contains a control character or `{`/`}`".to_string());
    }
    let joined = if op.starts_with('/') {
        op.to_string()
    } else {
        match base {
            Some(b) if b.starts_with('/') => format!("{}/{op}", b.trim_end_matches('/')),
            _ => return Placed::RelativeWithoutBase,
        }
    };
    let mut parts: Vec<&str> = Vec::new();
    for comp in joined.split('/') {
        match comp {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            c => parts.push(c),
        }
    }
    Placed::Absolute(format!("/{}", parts.join("/")))
}

/// The answer of [`place_literal`].
pub(super) enum Placed {
    /// The absolute, lexically normalised path.
    Absolute(String),
    /// A relative operand and no absolute working directory to place it in.
    /// The caller does not refuse this: it is the shape of every in-repo
    /// `rm src/main.rs` judged without a known cwd, which the pre-existing
    /// rules keep Allow on purpose (`detect::tests::
    /// ordinary_single_file_rm_stays_allowed`, `tests/high_blast_tier.rs`
    /// `anti_vacuity_allow_list`), and a relative path reaches a system
    /// directory only from a cwd inside one.
    RelativeWithoutBase,
    /// The text is not a literal filename (control character, a `{}`
    /// placeholder, empty); the reason says which.
    NotLiteral(String),
}

/// Read one operand word. `home` is the session's home directory, if known.
pub(super) fn expand_operand(word: &str, home: Option<&str>) -> OperandSpelling {
    let chars: Vec<char> = word.chars().collect();
    let words = match brace_expand(chars) {
        Ok(w) => w,
        Err(why) => return OperandSpelling::Unresolvable(why),
    };
    let mut out = Vec::new();
    for w in words {
        match unquote(&w, home) {
            Ok((path, home_rest)) => {
                if let Some(rooted) = home_rest.as_deref().and_then(climb_rooted_at_slash) {
                    out.push(rooted);
                }
                out.push(path);
            }
            Err(why) => return OperandSpelling::Unresolvable(why),
        }
    }
    OperandSpelling::Paths(out)
}

/// For each char of `w`: true when it is unquoted and unescaped (so it can be
/// a brace-expansion delimiter). `Err` on unbalanced quoting.
fn unquoted_mask(w: &[char]) -> Result<Vec<bool>, String> {
    let mut mask = vec![false; w.len()];
    let mut i = 0;
    while i < w.len() {
        match w[i] {
            '\\' => i += 2,
            '\'' => i = close_quote(w, i + 1, '\'', false)? + 1,
            '"' => i = close_quote(w, i + 1, '"', true)? + 1,
            '$' if matches!(w.get(i + 1), Some('\'')) => {
                i = close_quote(w, i + 2, '\'', true)? + 1;
            }
            '$' if matches!(w.get(i + 1), Some('"')) => {
                i = close_quote(w, i + 2, '"', true)? + 1;
            }
            '$' if matches!(w.get(i + 1), Some('{')) => {
                // `${…}` is a parameter expansion, not a brace group.
                match w[i + 2..].iter().position(|&c| c == '}') {
                    Some(p) => i = i + 2 + p + 1,
                    None => return Err("unterminated `${`".to_string()),
                }
            }
            _ => {
                mask[i] = true;
                i += 1;
            }
        }
    }
    Ok(mask)
}

/// Index of the quote character closing a quote opened just before `from`.
fn close_quote(w: &[char], from: usize, q: char, backslash_escapes: bool) -> Result<usize, String> {
    let mut j = from;
    while j < w.len() {
        if backslash_escapes && w[j] == '\\' {
            j += 2;
            continue;
        }
        if w[j] == q {
            return Ok(j);
        }
        j += 1;
    }
    Err("unbalanced quoting".to_string())
}

/// Bash brace expansion of one word (quote-aware). Comma groups expand;
/// sequence groups become `*`; any other `{…}` stays literal.
fn brace_expand(w: Vec<char>) -> Result<Vec<Vec<char>>, String> {
    let mut done: Vec<Vec<char>> = Vec::new();
    let mut todo: Vec<(Vec<char>, usize)> = vec![(w, 0)];
    while let Some((w, start)) = todo.pop() {
        let mask = unquoted_mask(&w)?;
        let mut expanded = false;
        let mut i = start;
        while i < w.len() {
            if !(mask[i] && w[i] == '{') {
                i += 1;
                continue;
            }
            let mut depth = 0usize;
            let mut commas = Vec::new();
            let mut close = None;
            for j in i..w.len() {
                if !mask[j] {
                    continue;
                }
                match w[j] {
                    '{' => depth += 1,
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            close = Some(j);
                            break;
                        }
                    }
                    ',' if depth == 1 => commas.push(j),
                    _ => {}
                }
            }
            let Some(close) = close else {
                break; // an unmatched `{` is literal
            };
            let prefix = &w[..i];
            let suffix = &w[close + 1..];
            if !commas.is_empty() {
                let mut bounds = vec![i];
                bounds.extend(&commas);
                bounds.push(close);
                for pair in bounds.windows(2) {
                    let mut n = prefix.to_vec();
                    n.extend_from_slice(&w[pair[0] + 1..pair[1]]);
                    n.extend_from_slice(suffix);
                    todo.push((n, 0));
                }
                expanded = true;
                break;
            }
            let body: String = w[i + 1..close].iter().collect();
            if is_sequence(&body) {
                let mut n = prefix.to_vec();
                n.push('*');
                let resume = n.len();
                n.extend_from_slice(suffix);
                todo.push((n, resume));
                expanded = true;
                break;
            }
            i += 1; // a literal `{…}` (`{}`, `{x}`): look for a later group
        }
        if !expanded {
            done.push(w);
        }
        if done.len() + todo.len() > MAX_EXPANSIONS {
            return Err(format!("brace expansion exceeds {MAX_EXPANSIONS} words"));
        }
    }
    Ok(done)
}

/// `{x..y}` / `{x..y..n}` with alphanumeric (or signed numeric) endpoints.
fn is_sequence(body: &str) -> bool {
    let parts: Vec<&str> = body.split("..").collect();
    (parts.len() == 2 || parts.len() == 3)
        && parts.iter().all(|p| {
            let p = p.strip_prefix('-').unwrap_or(p);
            !p.is_empty() && p.chars().all(|c| c.is_ascii_alphanumeric())
        })
}

/// Quote removal and `~`/`$HOME` expansion of one (brace-expanded) word.
/// Returns the path and, when the word STARTS with a home expansion, the text
/// that follows the home in the result.
fn unquote(w: &[char], home: Option<&str>) -> Result<(String, Option<String>), String> {
    let mut out = String::new();
    let mut home_end: Option<usize> = None;
    let home_or_err = || {
        home.map(|h| h.trim_end_matches('/').to_string())
            .ok_or_else(|| "home directory unknown".to_string())
    };
    let mut i = 0;
    if w.first() == Some(&'~') {
        let end = w.iter().position(|&c| c == '/').unwrap_or(w.len());
        if end != 1 {
            return Err("`~user` expansion".to_string());
        }
        out.push_str(&home_or_err()?);
        home_end = Some(out.len());
        i = 1;
    }
    let mut in_double = false;
    while i < w.len() {
        let c = w[i];
        match c {
            '`' => return Err("command substitution (backtick)".to_string()),
            '\\' if in_double => {
                match w.get(i + 1) {
                    Some(&n @ ('$' | '`' | '"' | '\\')) => out.push(n),
                    Some(&n) => {
                        out.push('\\');
                        out.push(n);
                    }
                    None => return Err("unbalanced quoting".to_string()),
                }
                i += 2;
            }
            '\\' => {
                match w.get(i + 1) {
                    Some(&n) => out.push(n),
                    None => return Err("trailing backslash".to_string()),
                }
                i += 2;
            }
            '"' => {
                in_double = !in_double;
                i += 1;
            }
            '\'' if !in_double => {
                let close = close_quote(w, i + 1, '\'', false)?;
                out.extend(&w[i + 1..close]);
                i = close + 1;
            }
            '$' if !in_double && w.get(i + 1) == Some(&'\'') => {
                let close = close_quote(w, i + 2, '\'', true)?;
                out.push_str(&ansi_c(&w[i + 2..close])?);
                i = close + 1;
            }
            '$' if !in_double && w.get(i + 1) == Some(&'"') => {
                in_double = true;
                i += 2;
            }
            '$' => {
                let rest: String = w[i + 1..].iter().collect();
                let len = if rest.starts_with("{HOME}") {
                    7
                } else if rest.starts_with("HOME")
                    && !rest[4..]
                        .chars()
                        .next()
                        .is_some_and(|n| n.is_ascii_alphanumeric() || n == '_')
                {
                    5
                } else {
                    return Err("variable or command substitution".to_string());
                };
                let at_start = out.is_empty();
                out.push_str(&home_or_err()?);
                if at_start {
                    home_end = Some(out.len());
                }
                i += len;
            }
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
    if in_double {
        return Err("unbalanced quoting".to_string());
    }
    let home_rest = home_end.map(|e| out[e..].to_string());
    Ok((out, home_rest))
}

/// The body of `$'…'` with its escapes applied. Escapes this does not model
/// (`\u`, `\U`, `\c`, non-ASCII bytes) are `Err`.
fn ansi_c(body: &[char]) -> Result<String, String> {
    let mut out = String::new();
    let mut i = 0;
    while i < body.len() {
        if body[i] != '\\' {
            out.push(body[i]);
            i += 1;
            continue;
        }
        let Some(&e) = body.get(i + 1) else {
            return Err("unterminated ANSI-C escape".to_string());
        };
        i += 2;
        let simple = match e {
            'a' => Some('\u{7}'),
            'b' => Some('\u{8}'),
            'e' | 'E' => Some('\u{1b}'),
            'f' => Some('\u{c}'),
            'n' => Some('\n'),
            'r' => Some('\r'),
            't' => Some('\t'),
            'v' => Some('\u{b}'),
            '\\' | '\'' | '"' | '?' => Some(e),
            _ => None,
        };
        if let Some(s) = simple {
            out.push(s);
            continue;
        }
        let (radix, max) = match e {
            'x' => (16, 2),
            '0'..='7' => {
                i -= 1; // the first octal digit is part of the number
                (8, 3)
            }
            _ => return Err(format!("unmodelled ANSI-C escape `\\{e}`")),
        };
        let digits: String = body[i..]
            .iter()
            .take(max)
            .take_while(|c| c.is_digit(radix))
            .collect();
        if digits.is_empty() {
            return Err(format!("empty ANSI-C `\\{e}` escape"));
        }
        i += digits.chars().count();
        let v = u32::from_str_radix(&digits, radix).map_err(|e| e.to_string())?;
        match char::from_u32(v) {
            Some(ch) if ch.is_ascii() => out.push(ch),
            _ => return Err("non-ASCII ANSI-C escape".to_string()),
        }
    }
    Ok(out)
}

/// When `rest` (the text after a leading home expansion) climbs above the
/// home with `..`, the remaining path rooted at `/`.
fn climb_rooted_at_slash(rest: &str) -> Option<String> {
    let mut stack: Vec<&str> = Vec::new();
    let mut climbed = false;
    for comp in rest.split('/') {
        match comp {
            "" | "." => {}
            ".." => {
                if stack.pop().is_none() {
                    climbed = true;
                }
            }
            c => stack.push(c),
        }
    }
    climbed.then(|| format!("/{}", stack.join("/")))
}
