//! A small, conservative shell lexer for the gate's Bash path check (contract
//! C2). Not a shell parser: it only splits a command into words and says
//! which words it can turn into a literal path and which it cannot.
//!
//! * Words are split on unquoted whitespace (newline included) and on the
//!   unquoted operator characters `; & | ( ) < >`, so a redirect target such
//!   as `>file` or `2>file` becomes its own word `file`.
//! * Single quotes are literal. Double quotes keep their content, but a `$` or
//!   backtick inside them is still an expansion; inside double quotes a
//!   backslash escapes only `$`, backtick, `"`, `\` and newline. An unquoted
//!   backslash escapes the next character (backslash-newline is a line
//!   continuation and vanishes).
//! * A word is *unresolvable* when it contains an unquoted or double-quoted
//!   `$` or backtick, an unquoted `*`, `?`, `[` or `{`, ends in a dangling
//!   unquoted backslash, or starts with an unquoted `~` not followed by `/` or
//!   the end of the word (`~user`). A leading `~` / `~/` expands to `$HOME`;
//!   with `HOME` unset or empty the word is unresolvable.
//! * An unterminated quote makes the whole command unresolvable.
//! * For a word with an unquoted `=` after its first character (`--opt=value`,
//!   `NAME=value`, `of=value`), the part after the first such `=` is a further
//!   path candidate, with the same tilde rule.
//!
//! What it does not see (documented limits): aliases, functions and `PATH`
//! resolution; `cd` inside the command (every relative word resolves against
//! the payload cwd); paths a program computes at run time; here-doc bodies
//! (lexed as words like the rest of the command).

/// One lexed word: its characters with a quoted flag each, the first
/// expansion / glob character found in it (if any), and its spelling in the
/// command (for messages).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Word {
    spelled: String,
    chars: Vec<(char, bool)>,
    expansion: Option<char>,
}

/// What one word of the command can be turned into.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WordPath {
    /// A literal path text (relative or absolute), with the word it came from.
    Literal { spelled: String, path: String },
    /// The gate cannot resolve this word; `why` names the word.
    Unresolvable { why: String },
}

/// Lex `command` and turn every word into path candidates (see the module
/// doc). `home` is the value of `HOME` (`None` when unset).
pub fn word_paths(command: &str, home: Option<&str>) -> Vec<WordPath> {
    let words = match lex(command) {
        Ok(w) => w,
        Err(partial) => {
            return vec![WordPath::Unresolvable {
                why: format!(
                    "the Bash command has an unterminated quote in `{partial}`, so the fde gate \
                     cannot resolve its words"
                ),
            }]
        }
    };
    let mut out = Vec::new();
    for w in &words {
        if let Some(c) = w.expansion {
            out.push(WordPath::Unresolvable {
                why: format!(
                    "the Bash word `{}` contains `{c}` (an expansion, substitution, glob or \
                     brace, or a dangling backslash), which the fde gate cannot resolve to a path",
                    w.spelled
                ),
            });
            continue;
        }
        let mut candidates: Vec<&[(char, bool)]> = vec![&w.chars];
        if let Some(k) = w
            .chars
            .iter()
            .skip(1)
            .position(|&(c, q)| c == '=' && !q)
            .map(|k| k + 1)
        {
            candidates.push(&w.chars[k + 1..]);
        }
        for cand in candidates {
            match expand_tilde(cand, home) {
                Ok(p) if p.is_empty() => {}
                Ok(path) => out.push(WordPath::Literal {
                    spelled: w.spelled.clone(),
                    path,
                }),
                Err(why) => out.push(WordPath::Unresolvable {
                    why: format!(
                        "the Bash word `{}` {why}, so the fde gate cannot resolve it to a path",
                        w.spelled
                    ),
                }),
            }
        }
    }
    out
}

/// The literal text of `chars`, with a leading unquoted `~` / `~/` expanded
/// to `home`. `Err` for `~user` and for `~` with `home` unset or empty.
fn expand_tilde(chars: &[(char, bool)], home: Option<&str>) -> Result<String, String> {
    let text: String = chars.iter().map(|&(c, _)| c).collect();
    if chars.first() != Some(&('~', false)) {
        return Ok(text);
    }
    let end = chars
        .iter()
        .position(|&(c, q)| c == '/' && !q)
        .unwrap_or(chars.len());
    if end != 1 {
        return Err("starts with a `~user` form".to_string());
    }
    match home {
        Some(h) if !h.is_empty() => {
            let rest: String = chars[1..].iter().map(|&(c, _)| c).collect();
            Ok(format!("{h}{rest}"))
        }
        _ => Err("starts with `~` and HOME is unset or empty".to_string()),
    }
}

/// Split `command` into words. `Err(partial)` on an unterminated quote, with
/// the text from the start of the unterminated word to the end.
fn lex(command: &str) -> Result<Vec<Word>, String> {
    struct Cur {
        start: usize,
        chars: Vec<(char, bool)>,
        expansion: Option<char>,
    }
    fn finish(words: &mut Vec<Word>, cur: Option<Cur>, command: &str, end: usize) {
        if let Some(c) = cur {
            if c.chars.is_empty() && c.expansion.is_none() {
                return;
            }
            words.push(Word {
                spelled: command[c.start..end].to_string(),
                chars: c.chars,
                expansion: c.expansion,
            });
        }
    }
    let mut words = Vec::new();
    let mut cur: Option<Cur> = None;
    let mut it = command.char_indices().peekable();
    while let Some((i, c)) = it.next() {
        if c.is_whitespace() || ";&|()<>".contains(c) {
            finish(&mut words, cur.take(), command, i);
            continue;
        }
        let w = cur.get_or_insert_with(|| Cur {
            start: i,
            chars: Vec::new(),
            expansion: None,
        });
        match c {
            '\'' => loop {
                match it.next() {
                    Some((_, '\'')) => break,
                    Some((_, ch)) => w.chars.push((ch, true)),
                    None => return Err(command[w.start..].to_string()),
                }
            },
            '"' => loop {
                match it.next() {
                    Some((_, '"')) => break,
                    Some((_, '\\')) => match it.peek() {
                        Some(&(_, n)) if "$`\"\\\n".contains(n) => {
                            it.next();
                            if n != '\n' {
                                w.chars.push((n, true));
                            }
                        }
                        _ => w.chars.push(('\\', true)),
                    },
                    Some((_, ch)) => {
                        if ch == '$' || ch == '`' {
                            w.expansion.get_or_insert(ch);
                        }
                        w.chars.push((ch, true));
                    }
                    None => return Err(command[w.start..].to_string()),
                }
            },
            '\\' => match it.next() {
                Some((_, '\n')) => {}
                Some((_, n)) => w.chars.push((n, true)),
                None => {
                    w.expansion.get_or_insert('\\');
                }
            },
            _ => {
                if "$`*?[{".contains(c) {
                    w.expansion.get_or_insert(c);
                }
                w.chars.push((c, false));
            }
        }
    }
    finish(&mut words, cur.take(), command, command.len());
    Ok(words)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lits(cmd: &str) -> Vec<String> {
        word_paths(cmd, Some("/h"))
            .into_iter()
            .map(|w| match w {
                WordPath::Literal { path, .. } => path,
                WordPath::Unresolvable { why } => format!("!{why}"),
            })
            .collect()
    }

    #[test]
    fn splits_operators_quotes_and_escapes() {
        assert_eq!(lits("echo hi>proj/x"), ["echo", "hi", "proj/x"]);
        assert_eq!(lits("echo hi 2>proj/x"), ["echo", "hi", "2", "proj/x"]);
        assert_eq!(lits("echo 'a b' \"c d\""), ["echo", "a b", "c d"]);
        assert_eq!(lits("touch pr\\oj/x"), ["touch", "proj/x"]);
        assert_eq!(lits("ls '*'"), ["ls", "*"]);
        assert_eq!(lits("ls ~/x ~"), ["ls", "/h/x", "/h"]);
        assert_eq!(lits("FOO=proj/x true"), ["FOO=proj/x", "proj/x", "true"]);
    }

    #[test]
    fn unresolvable_words() {
        for cmd in [
            "echo $HOME",
            "echo \"$HOME\"",
            "echo `pwd`",
            "ls *",
            "ls [ab]",
            "echo {a,b}",
            "ls ~root",
            "echo 'abc",
            "echo \"abc",
            "echo x\\",
        ] {
            assert!(
                lits(cmd).iter().any(|w| w.starts_with('!')),
                "{cmd:?} must have an unresolvable word"
            );
        }
        assert!(word_paths("ls ~/", None)
            .iter()
            .any(|w| matches!(w, WordPath::Unresolvable { .. })));
        assert!(word_paths("ls ~/", Some(""))
            .iter()
            .any(|w| matches!(w, WordPath::Unresolvable { .. })));
    }
}
