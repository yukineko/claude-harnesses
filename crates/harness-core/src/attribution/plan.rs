//! Which directory a recorded Bash command's relative paths land in.
//!
//! A command is tracked ([`walk_cwd`](super::walk_cwd)) only when it is a FLAT
//! list of simple commands; a command that holds a `cd`/`pushd`/`popd` token
//! and is anything else is FENCED ([`fenced_paths`]): its directory is not
//! tracked and every candidate directory is attributed instead.

use super::*;

/// Words a directory-command scan splits on. `$` is deliberately NOT here, so
/// `cd $X` keeps its target visible as `$X`.
fn is_token_sep(c: char) -> bool {
    c.is_whitespace() || "();&|`{}<>\"'".contains(c)
}

/// Whether `text` holds a `cd`/`pushd`/`popd` token ANYWHERE, including glued
/// forms (`(cd`, `$(cd`) and quoted or heredoc text. A text-level
/// over-approximation: it only decides whether a non-flat command is fenced.
fn has_dir_token(text: &str) -> bool {
    text.split(is_token_sep).any(|t| DIR_CMDS.contains(&t))
}

/// `text` with every comment removed: a `#` that begins a word, outside
/// quotes and heredoc bodies, up to (not including) the newline. `None` when
/// the text cannot be followed (an unterminated quote or heredoc): the caller
/// must then not trust any parse of it.
fn strip_comments(text: &str) -> Option<String> {
    let b = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut pending: Vec<(String, bool)> = Vec::new();
    let mut word_start = true;
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        let ch = text[i..].chars().next()?;
        match c {
            b'\\' => {
                let n = text.get(i + 1..)?.chars().next();
                let len = 1 + n.map_or(0, char::len_utf8);
                out.push_str(&text[i..i + len]);
                i += len;
                word_start = false;
            }
            b'\'' => {
                let e = text.get(i + 1..)?.find('\'')? + i + 1;
                out.push_str(&text[i..=e]);
                i = e + 1;
                word_start = false;
            }
            b'"' => {
                let mut j = i + 1;
                loop {
                    match *b.get(j)? {
                        b'\\' => j += 2,
                        b'"' => break,
                        _ => j += 1,
                    }
                }
                out.push_str(text.get(i..=j)?);
                i = j + 1;
                word_start = false;
            }
            b'#' if word_start => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'<' if b.get(i + 1) == Some(&b'<') && b.get(i + 2) != Some(&b'<') => {
                let mut j = i + 2;
                let strip = b.get(j) == Some(&b'-');
                if strip {
                    j += 1;
                }
                while matches!(b.get(j), Some(b' ' | b'\t')) {
                    j += 1;
                }
                let mut delim = String::new();
                while let Some(&d) = b.get(j) {
                    match d {
                        b'\'' | b'"' => {
                            let e = text.get(j + 1..)?.find(d as char)? + j + 1;
                            delim.push_str(&text[j + 1..e]);
                            j = e + 1;
                        }
                        b'\\' => {
                            let n = text.get(j + 1..)?.chars().next()?;
                            delim.push(n);
                            j += 1 + n.len_utf8();
                        }
                        b' ' | b'\t' | b'\n' | b';' | b'&' | b'|' | b'(' | b')' | b'<' | b'>' => {
                            break
                        }
                        _ => {
                            let n = text[j..].chars().next()?;
                            delim.push(n);
                            j += n.len_utf8();
                        }
                    }
                }
                if delim.is_empty() {
                    return None;
                }
                pending.push((delim, strip));
                out.push_str(&text[i..j]);
                i = j;
                word_start = false;
            }
            b'\n' => {
                out.push('\n');
                i += 1;
                for (delim, strip) in pending.drain(..) {
                    let mut found = false;
                    while i <= b.len() {
                        let nl = text[i..].find('\n').map(|n| n + i);
                        let end = nl.unwrap_or(b.len());
                        let line = &text[i..end];
                        let cmp = if strip {
                            line.trim_start_matches('\t')
                        } else {
                            line
                        };
                        out.push_str(line);
                        match nl {
                            Some(n) => {
                                out.push('\n');
                                i = n + 1;
                            }
                            None => i = b.len() + 1,
                        }
                        if cmp == delim {
                            found = true;
                            break;
                        }
                        if nl.is_none() {
                            break;
                        }
                    }
                    if !found {
                        return None;
                    }
                }
                word_start = true;
            }
            b' ' | b'\t' | b';' | b'&' | b'|' | b'(' | b')' | b'<' | b'>' => {
                out.push(ch);
                i += 1;
                word_start = true;
            }
            _ => {
                out.push(ch);
                i += ch.len_utf8();
                word_start = false;
            }
        }
    }
    if !pending.is_empty() {
        return None;
    }
    Some(out)
}

/// A command is FLAT when it is a list of simple commands joined by `&&`, `;`
/// or newline and nothing else: no `||`, `&`, pipe, subshell paren, function
/// definition, compound keyword (`if`/`then`/`else`/`elif`/`fi`/`while`/
/// `until`/`for`/`do`/`done`/`case`/`esac`/`function`/`select`, `{`, `}`,
/// `[[`), command substitution or here-string. Keywords are looked for in
/// every parsed word, not only in command position (`echo done` is not flat:
/// the restrictive side).
fn is_flat(stmts: &[Statement]) -> bool {
    const KEYWORDS: [&str; 17] = [
        "if", "then", "else", "elif", "fi", "while", "until", "for", "do", "done", "case", "esac",
        "function", "select", "{", "}", "[[",
    ];
    stmts.iter().all(|s| {
        matches!(s.post, Sep::Seq | Sep::And | Sep::End)
            && !s.opaque
            && s.stages.len() == 1
            && s.stages
                .iter()
                .flat_map(|st| st.words.iter())
                .all(|w| !KEYWORDS.contains(&w.as_str()))
    })
}

/// How one recorded Bash command's directory is tracked.
pub(super) struct Plan {
    /// The command with comments removed (or as recorded when they cannot be).
    pub(super) cmd: String,
    /// Its statements; `None` when the tokenizer cannot follow it.
    pub(super) stmts: Option<Vec<Statement>>,
    /// A `cd`/`pushd`/`popd` token appears anywhere in `cmd`.
    pub(super) has_cd: bool,
    /// `has_cd` and the command is not a flat list (or is unparsed): the
    /// directory is NOT tracked ([`fenced_paths`]).
    pub(super) fenced: bool,
}

pub(super) fn plan_call(command: &str) -> Plan {
    let stripped = strip_comments(command);
    let cmd = stripped.clone().unwrap_or_else(|| command.to_string());
    let stmts = stripped.and_then(|s| statements(&s));
    let has_cd = has_dir_token(&cmd);
    let flat = stmts.as_deref().is_some_and(is_flat);
    let fenced = has_cd && !flat;
    Plan {
        cmd,
        stmts,
        has_cd,
        fenced,
    }
}

impl Plan {
    /// The directory in effect per statement: followed ([`walk_cwd`]) for a
    /// flat command that holds a directory command, otherwise the recorded
    /// cwd throughout (no directory command to follow).
    pub(super) fn walk(&self, stmts: &[Statement], cwd: Option<&Path>) -> Result<Walk, String> {
        if self.has_cd {
            walk_cwd(&self.cmd, stmts, cwd)
        } else {
            let base: Vec<PathBuf> = cwd.map(Path::to_path_buf).into_iter().collect();
            Ok(Walk {
                bases: vec![base; stmts.len()],
                entered: Vec::new(),
            })
        }
    }
}

/// The paths of a command whose directory is NOT tracked (a directory command
/// inside a compound command, subshell, function, substitution, comment,
/// here-string or otherwise unparsed text). Fail closed by widening: every
/// literal directory any `cd`/`pushd` token in it could enter is added to the
/// candidate cwds next to the recorded cwd, and every token of the text is
/// joined with every candidate. The set is therefore a superset of where the
/// writes can land. A target that is not a literal (`cd $X`, bare `cd`),
/// `CDPATH`, or `cd -`/`popd`/`pushd +N` cannot be bounded: `Some(reason)`.
pub(super) fn fenced_paths(
    cmd: &str,
    cwd: Option<&Path>,
    has_cd: bool,
) -> (BTreeSet<PathBuf>, Option<String>) {
    let mut out = BTreeSet::new();
    let tokens: Vec<&str> = cmd.split(is_token_sep).filter(|t| !t.is_empty()).collect();
    let mut bases: Vec<PathBuf> = cwd.map(Path::to_path_buf).into_iter().collect();
    let mut entered: Vec<PathBuf> = Vec::new();
    if has_cd && cmd.contains("CDPATH") {
        return (
            out,
            Some("CDPATH in a command whose directory is not tracked".into()),
        );
    }
    for (i, t) in tokens.iter().enumerate() {
        if !DIR_CMDS.contains(t) {
            continue;
        }
        if *t == "popd" {
            continue;
        }
        let target = tokens[i + 1..]
            .iter()
            .copied()
            .find(|a| !matches!(*a, "--" | "-P" | "-L"));
        let Some(target) = target else {
            return (out, Some(format!("`{t}` without a target")));
        };
        let rotate = target == "-"
            || ((target.starts_with('+') || target.starts_with('-'))
                && target.len() > 1
                && target[1..].chars().all(|c| c.is_ascii_digit()));
        if rotate {
            continue;
        }
        if target.starts_with('-') || !plain_word(target) {
            return (out, Some(format!("`{t} {target}`: not a literal path")));
        }
        let p = Path::new(target);
        let mut cands: Vec<PathBuf> = Vec::new();
        if p.is_absolute() || bases.is_empty() {
            cands.push(p.to_path_buf());
        }
        if !p.is_absolute() {
            cands.extend(bases.iter().map(|b| b.join(p)));
        }
        for c in cands {
            if (c.is_relative() || c.is_dir()) && !bases.contains(&c) {
                entered.push(c.clone());
                bases.push(c);
            }
        }
    }
    for t in &tokens {
        insert_named(&mut out, t, &bases);
    }
    for d in entered {
        out.insert(if d.is_relative() { d } else { resolve(&d) });
    }
    (out, None)
}
