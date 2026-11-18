//! Plain-`echo` output helpers, split out of `payload.rs` (behavior-neutral
//! move for the file-growth cap; no logic change).

/// What `echo [-neE…] WORD…` writes when every WORD is static and holds no
/// backslash (so bash's and zsh's echo agree: no escape is decoded): plain
/// characters, single-quoted text, or double-quoted text without `$`, a
/// backtick, `\` or `!`. `None` for anything else, including a first word `-`
/// or `--` (zsh and bash disagree on it) and any redirection.
pub(super) fn plain_echo_output(stage: &str) -> Option<String> {
    let words = super::super::quote_aware_words(stage);
    if words.first().map(String::as_str) != Some("echo") {
        return None;
    }
    let flags = echo_flag_count(&words[1..]);
    let no_newline = words[1..1 + flags].iter().any(|f| f.contains('n'));
    let rest = &words[1 + flags..];
    if rest.first().is_some_and(|w| w == "-" || w == "--") {
        return None;
    }
    let mut out = Vec::new();
    for word in rest {
        out.push(static_word(word)?);
    }
    let mut text = out.join(" ");
    if !no_newline {
        text.push('\n');
    }
    Some(text)
}

/// How many leading `words` are echo option clusters: `-` followed only by
/// `n`, `e`, `E` (`-n`, `-nE`, `-neE` …).
fn echo_flag_count<S: AsRef<str>>(words: &[S]) -> usize {
    words
        .iter()
        .take_while(|w| {
            w.as_ref()
                .strip_prefix('-')
                .is_some_and(|f| !f.is_empty() && f.chars().all(|c| matches!(c, 'n' | 'e' | 'E')))
        })
        .count()
}

/// The text of one shell word built only from plain characters, single-quoted
/// spans and double-quoted spans without `$`, a backtick, `\` or `!`; `None`
/// for anything else (expansion, escape, glob, tilde, operator, unterminated
/// quote, `#` comment).
fn static_word(word: &str) -> Option<String> {
    let mut out = String::new();
    let mut chars = word.chars();
    while let Some(c) = chars.next() {
        match c {
            '\'' | '"' => loop {
                let d = chars.next()?;
                if d == c {
                    break;
                }
                if d == '\\' || (c == '"' && matches!(d, '$' | '`' | '!')) {
                    return None;
                }
                out.push(d);
            },
            c if is_plain_word_char(c) => out.push(c),
            _ => return None,
        }
    }
    Some(out)
}

/// A character that means itself in an unquoted shell word (no quoting,
/// escape, expansion, glob, tilde, comment or operator role).
fn is_plain_word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || "._-/:=@%+,".contains(c)
}

/// `echo [-neE…] WORD…` with every WORD made only of plain characters
/// ([`is_plain_word_char`]) respelled as `echo [-neE…] 'WORD …'`: the same
/// output, in the quoted form the pipe rule reads as a literal program (an
/// unquoted `echo hi | bash` has no quoted text there and asks). The leading
/// option clusters are any `-` followed only by `n` / `e` / `E` (`-nE`, `-En`,
/// `-neE` too); a plain word holds no backslash, so `-e` decodes nothing. A
/// first word `-` / `--` (zsh and bash read it differently) or anything else
/// is returned unchanged, so it keeps whatever verdict the pipe rule gives.
pub(super) fn quote_bare_echo_words(inner: &str) -> String {
    let tokens: Vec<&str> = inner.split_whitespace().collect();
    if tokens.first() != Some(&"echo") {
        return inner.to_string();
    }
    let flags = echo_flag_count(&tokens[1..]);
    let words = &tokens[1 + flags..];
    if words.is_empty()
        || words[0] == "-"
        || words[0] == "--"
        || !words.iter().all(|w| w.chars().all(is_plain_word_char))
    {
        return inner.to_string();
    }
    format!("{} '{}'", tokens[..1 + flags].join(" "), words.join(" "))
}
