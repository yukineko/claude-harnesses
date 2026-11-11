//! What `echo` / `printf` write to a file is the file's content, and when
//! another part of the same command runs that file, the content is judged as
//! the shell script it is (tests/script_write_then_run_harm_20261111.rs H1-H3:
//! `echo 'rm -rf ~' > x.rs && sh x.rs` was Allow). The `cat`/`tee`
//! here-string twin is [`super::here_string_file`]; this module gives the same
//! reading to the literal arguments of `echo` and `printf`.
//!
//! A writer is a pipe stage whose command (behind any exec-wrapper,
//! [`command_candidates`]) is `echo` or `printf`, writing its stdout to a file
//! through `>`, `>>`, `>|`, `&>`, `1>` (glued or not), or through a following
//! `| tee FILE…` / `| cat > FILE` stage. Writes to the same file (leading `./`
//! ignored) accumulate in command order: `>` replaces, `>>` appends, and the
//! content after every write is judged, so neither a later overwrite nor a
//! split across two `printf`s hides a script that ran in between.
//!
//! The file is judged only when some OTHER stage of the command may run it:
//!   * a stage whose command word names the file (`./x.sh`, `/p/x.sh`, a word
//!     behind an exec-wrapper);
//!   * a pipeline (one statement) that names the file anywhere and holds a
//!     shell, `.` or `source` word anywhere (`sh x.sh`, `bash -e x.sh`,
//!     `. x.sh`, `sh < x.sh`, `cat x.sh | sh`, `find . -exec sh {} \;`).
//!
//! A word names the file when its literal value has the file's basename
//! (`sh /tmp/x.sh` names `x.sh`), or, when the word holds an expansion, when
//! its text contains that basename. A written path that is not a literal
//! (`> "$F"`) is run by any pipeline holding a shell, `.` or `source` word, or
//! a command word with `$` or a backtick. These over-approximate on purpose:
//! a false match only means the written text is read as a script.
//!
//! Judged means the content goes through every shell rule
//! (`analyze_shell_payload`), Deny over Ask over Allow. Content blastguard
//! cannot reproduce is an Ask, never Allow:
//!   * an argument with `$`, a backtick, a backslash, a glob or brace
//!     character, a bare operator character, or a `~` other than a leading
//!     `~` / `~/` (kept as `~`, which the shell rules read as home, the same
//!     path the writing shell would put there);
//!   * `echo` output holding a backslash (dash's and zsh's `echo` expand
//!     escapes, bash's does under `-e` or `xpg_echo`);
//!   * a `printf` format with any `%` directive other than `%%`, any escape
//!     other than `\n`, `\t`, `\\`, a format starting with `-`, or arguments
//!     after the format (format reuse is not modelled).

use super::words::basename;
use super::{
    analyze_shell_payload, command_candidates, is_shell, normalized_command, quote_aware_words,
    split_statements_into_pipe_stages, Ctx, Decision, VerdictAcc,
};

/// A file an `echo`/`printf` stage writes: its literal path (leading `./`
/// removed), or the raw word when the path is not a literal.
#[derive(Clone, PartialEq, Eq)]
enum Target {
    Literal(String),
    Opaque(String),
}

impl Target {
    fn from_word(word: &str) -> Target {
        match word_literal(word) {
            Some(lit) if !lit.is_empty() => {
                let mut p = lit.as_str();
                while let Some(rest) = p.strip_prefix("./") {
                    p = rest;
                }
                let p = p.trim_end_matches('/');
                if basename(p).is_empty() || matches!(basename(p), "." | "..") {
                    Target::Opaque(word.to_string())
                } else {
                    Target::Literal(p.to_string())
                }
            }
            _ => Target::Opaque(word.to_string()),
        }
    }

    fn shown(&self) -> &str {
        match self {
            Target::Literal(s) | Target::Opaque(s) => s,
        }
    }
}

/// One output write: where, and whether it appends.
struct Write {
    target: Target,
    append: bool,
}

/// The text a writer puts on its stdout, or why it cannot be reproduced.
type Content = Result<String, String>;

/// One file written in the command: every content it holds after a write.
struct WrittenFile {
    target: Target,
    snapshots: Vec<Content>,
    writer: String,
}

/// The verdict on the files `echo`/`printf` write in `cmd` that another stage
/// of `cmd` may run; Allow when there are none. See the module documentation.
pub(super) fn echoed_script_verdict(cmd: &str, depth: usize, ctx: &Ctx<'_>) -> Decision {
    let statements = split_statements_into_pipe_stages(cmd);
    let mut files: Vec<WrittenFile> = Vec::new();
    let mut writer_stages: Vec<(usize, usize)> = Vec::new();
    for (s, stages) in statements.iter().enumerate() {
        for (i, stage) in stages.iter().enumerate() {
            let words = stage_words(stage);
            let Some(mut writer) = echo_writer(&words) else {
                continue;
            };
            writer_stages.push((s, i));
            if !writer.stdout_redirected {
                if let Some(next) = stages.get(i + 1) {
                    let next_words = stage_words(next);
                    if let Some(piped) = piped_writes(&next_words) {
                        writer_stages.push((s, i + 1));
                        writer.writes = piped;
                    }
                }
            }
            let content = writer.content();
            for w in writer.writes {
                let idx = match files.iter().position(|f| f.target == w.target) {
                    Some(idx) => idx,
                    None => {
                        files.push(WrittenFile {
                            target: w.target.clone(),
                            snapshots: Vec::new(),
                            writer: writer.name.clone(),
                        });
                        files.len() - 1
                    }
                };
                let file = &mut files[idx];
                let next = match (w.append, file.snapshots.last()) {
                    (true, Some(Ok(prev))) => content.clone().map(|c| format!("{prev}{c}")),
                    (true, Some(Err(why))) => Err(why.clone()),
                    _ => content.clone(),
                };
                file.snapshots.push(next);
            }
        }
    }
    let mut acc = VerdictAcc::default();
    for file in &files {
        let runs = statements.iter().enumerate().any(|(s, stages)| {
            let others: Vec<&str> = stages
                .iter()
                .enumerate()
                .filter(|(i, _)| !writer_stages.contains(&(s, *i)))
                .map(|(_, st)| st.as_str())
                .collect();
            statement_may_run(&others, &file.target)
        });
        if !runs {
            continue;
        }
        let shape = format!(
            "`{}` writes `{}`, and another part of this command may run that file, so the \
written text is judged as the shell script it is",
            file.writer,
            file.target.shown()
        );
        for snapshot in &file.snapshots {
            let verdict = match snapshot {
                Err(why) => Decision::ask(format!(
                    "{shape}; {why}, so blastguard cannot see the script the file will hold"
                )),
                Ok(text) if text.trim().is_empty() => Decision::Allow,
                Ok(text) => match analyze_shell_payload(text, depth, ctx) {
                    Decision::Ask(r) => Decision::ask(format!("{shape}: {r}")),
                    d => d,
                },
            };
            if let Some(deny) = acc.record(verdict) {
                return deny;
            }
        }
    }
    acc.finish()
}

/// An `echo` / `printf` stage: its name, its argument words (quotes still on),
/// the files its stdout is written to, and whether its stdout goes anywhere
/// but the pipe.
struct EchoWriter {
    name: String,
    args: Vec<String>,
    writes: Vec<Write>,
    stdout_redirected: bool,
}

fn echo_writer(words: &[String]) -> Option<EchoWriter> {
    let tokens: Vec<&str> = words.iter().map(String::as_str).collect();
    let idx = command_candidates(&tokens)
        .into_iter()
        .find(|&i| matches!(normalized_command(tokens[i]).as_str(), "echo" | "printf"))?;
    let name = normalized_command(tokens[idx]);
    let mut args = Vec::new();
    let mut writes = Vec::new();
    let mut stdout_redirected = false;
    let mut i = idx + 1;
    while i < words.len() {
        let w = words[i].as_str();
        i += 1;
        let Some(redirect) = redirect_parts(w) else {
            args.push(w.to_string());
            continue;
        };
        let target = if redirect.rest.is_empty() {
            i += 1;
            words.get(i - 1).map(String::as_str).unwrap_or("")
        } else {
            redirect.rest
        };
        if !redirect.output || !matches!(redirect.fd, "" | "1" | "&") {
            continue;
        }
        stdout_redirected = true;
        if target.starts_with('&') || target.is_empty() {
            continue;
        }
        writes.push(Write {
            target: Target::from_word(target),
            append: redirect.op == ">>",
        });
    }
    Some(EchoWriter {
        name,
        args,
        writes,
        stdout_redirected,
    })
}

/// The files a `tee` or `cat` stage reading a writer's pipe writes it to.
/// `None` when the stage is neither.
fn piped_writes(words: &[String]) -> Option<Vec<Write>> {
    let tokens: Vec<&str> = words.iter().map(String::as_str).collect();
    let idx = command_candidates(&tokens)
        .into_iter()
        .find(|&i| matches!(normalized_command(tokens[i]).as_str(), "tee" | "cat"))?;
    let tee = normalized_command(tokens[idx]) == "tee";
    let append_all = tee
        && words[idx + 1..]
            .iter()
            .any(|w| w == "-a" || w == "--append" || (w.starts_with('-') && w.contains('a')));
    let mut writes = Vec::new();
    let mut i = idx + 1;
    while i < words.len() {
        let w = words[i].as_str();
        i += 1;
        if let Some(redirect) = redirect_parts(w) {
            let target = if redirect.rest.is_empty() {
                i += 1;
                words.get(i - 1).map(String::as_str).unwrap_or("")
            } else {
                redirect.rest
            };
            if redirect.output
                && matches!(redirect.fd, "" | "1" | "&")
                && !target.starts_with('&')
                && !target.is_empty()
            {
                writes.push(Write {
                    target: Target::from_word(target),
                    append: redirect.op == ">>",
                });
            }
            continue;
        }
        if tee && !w.starts_with('-') {
            writes.push(Write {
                target: Target::from_word(w),
                append: append_all,
            });
        }
    }
    Some(writes)
}

/// A redirect word split into its fd prefix (`""`, digits or `&`), its
/// operator and the glued target (empty when the target is the next word).
struct Redirect<'a> {
    fd: &'a str,
    op: &'a str,
    rest: &'a str,
    output: bool,
}

fn redirect_parts(word: &str) -> Option<Redirect<'_>> {
    if word.starts_with(['\'', '"']) {
        return None;
    }
    let fd_len = if word.starts_with('&') {
        1
    } else {
        word.chars().take_while(char::is_ascii_digit).count()
    };
    let (fd, tail) = word.split_at(fd_len);
    for (op, output) in [
        (">>", true),
        (">|", true),
        (">", true),
        ("<<<", false),
        ("<<-", false),
        ("<<", false),
        ("<>", false),
        ("<", false),
    ] {
        if let Some(rest) = tail.strip_prefix(op) {
            if fd == "&" && !output {
                return None;
            }
            return Some(Redirect {
                fd,
                op,
                rest,
                output,
            });
        }
    }
    None
}

impl EchoWriter {
    /// The text this writer puts on its stdout.
    fn content(&self) -> Content {
        if self.name == "printf" {
            printf_content(&self.args)
        } else {
            echo_content(&self.args)
        }
    }
}

fn literal_args(args: &[String]) -> Result<Vec<String>, String> {
    args.iter()
        .map(|a| {
            word_literal(a).ok_or_else(|| {
                format!("its argument `{a}` is expanded or unquoted by the writing shell")
            })
        })
        .collect()
}

fn echo_content(args: &[String]) -> Content {
    let lits = literal_args(args)?;
    let mut newline = true;
    let mut start = 0;
    for lit in &lits {
        let flag = lit
            .strip_prefix('-')
            .is_some_and(|f| !f.is_empty() && f.chars().all(|c| matches!(c, 'n' | 'e' | 'E')));
        if !flag {
            break;
        }
        if lit.contains('n') {
            newline = false;
        }
        start += 1;
    }
    let mut text = lits[start..].join(" ");
    if text.contains('\\') {
        return Err(format!(
            "its text (`{text}`) holds a backslash, which `echo` may expand as an escape"
        ));
    }
    if newline {
        text.push('\n');
    }
    Ok(text)
}

fn printf_content(args: &[String]) -> Content {
    let lits = literal_args(args)?;
    let Some(format) = lits.first() else {
        return Ok(String::new());
    };
    if format.starts_with('-') || lits.len() > 1 {
        return Err(format!(
            "its arguments (`{}`) need printf's option or format-reuse rules, which are not \
modelled",
            lits.join(" ")
        ));
    }
    let mut out = String::new();
    let mut chars = format.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('\\') => out.push('\\'),
                _ => {
                    return Err(format!(
                        "its format (`{format}`) holds an escape that is not modelled"
                    ))
                }
            },
            '%' => match chars.next() {
                Some('%') => out.push('%'),
                _ => {
                    return Err(format!(
                        "its format (`{format}`) holds a conversion that is not modelled"
                    ))
                }
            },
            x => out.push(x),
        }
    }
    Ok(out)
}

/// The value the shell gives `word` without expanding anything, or `None`
/// when it would expand, glob, unescape or split it (see the module
/// documentation for the characters refused). A leading `~` or `~/` is kept as
/// `~`.
fn word_literal(word: &str) -> Option<String> {
    let mut out = String::new();
    let mut chars = word.chars().peekable();
    let mut first = true;
    while let Some(c) = chars.next() {
        match c {
            '\'' => loop {
                match chars.next()? {
                    '\'' => break,
                    x => out.push(x),
                }
            },
            '"' => loop {
                match chars.next()? {
                    '"' => break,
                    '$' | '`' | '\\' => return None,
                    x => out.push(x),
                }
            },
            '~' if first && matches!(chars.peek(), None | Some('/')) => out.push('~'),
            '$' | '`' | '\\' | '*' | '?' | '[' | ']' | '{' | '}' | '~' | '<' | '>' | '|' | ';'
            | '&' | '(' | ')' | '#' => return None,
            x => out.push(x),
        }
        first = false;
    }
    Some(out)
}

/// Shell reserved words and grouping tokens that may stand before a stage's
/// command word.
const LEADING_KEYWORDS: &[&str] = &[
    "{", "(", "!", "if", "then", "elif", "else", "do", "while", "until", "time",
];

/// True when the stages `stages` (one pipeline, writer stages removed) may run
/// `target`; see the module documentation.
fn statement_may_run(stages: &[&str], target: &Target) -> bool {
    let mut names = false;
    let mut runner_word = false;
    let mut expansion_head = false;
    for stage in stages {
        let words = stage_words(stage);
        let cleaned: Vec<&str> = words
            .iter()
            .map(|w| w.trim_matches(|c| matches!(c, '(' | ')' | '{' | '}' | ';')))
            .collect();
        let start = cleaned
            .iter()
            .position(|w| !w.is_empty() && !LEADING_KEYWORDS.contains(w))
            .unwrap_or(cleaned.len());
        let heads = command_candidates(&cleaned[start..]);
        for &h in &heads {
            let head = cleaned[start + h];
            if head.contains(['$', '`']) {
                expansion_head = true;
            }
            if let Target::Literal(path) = target {
                if word_names(head, path) {
                    return true;
                }
            }
        }
        for w in &cleaned {
            let n = normalized_command(w);
            if is_shell(&n) || matches!(n.as_str(), "." | "source") {
                runner_word = true;
            }
            if let Target::Literal(path) = target {
                names |= word_names(w, path);
            }
        }
    }
    match target {
        Target::Literal(_) => names && runner_word,
        Target::Opaque(_) => runner_word || expansion_head,
    }
}

/// True when `word` (an input redirect prefix such as `<` / `0<` allowed)
/// names the file `path`: its literal value has `path`'s basename, or it holds
/// an expansion and its text contains that basename.
fn word_names(word: &str, path: &str) -> bool {
    let base = basename(path);
    let mut w = word;
    if let Some(pos) = w.find('<') {
        if w[..pos].chars().all(|c| c.is_ascii_digit()) {
            w = w[pos..].trim_start_matches('<');
        }
    }
    match word_literal(w) {
        Some(lit) => basename(lit.trim_end_matches('/')) == base,
        None => w.contains(base) || w.contains(['*', '?', '[']),
    }
}

/// `quote_aware_words(stage)` with a redirect glued to the end of a word split
/// off as its own word: `'rm -rf ~'>x.sh` is the argument `'rm -rf ~'` and the
/// redirect `>x.sh`, `sh<x.sh` is `sh` and `<x.sh`. A word whose unquoted
/// prefix before the operator is an fd (`2>`, `&>`) stays whole.
fn stage_words(stage: &str) -> Vec<String> {
    let mut out = Vec::new();
    for word in quote_aware_words(stage) {
        let (mut in_s, mut in_d) = (false, false);
        let mut split = None;
        let mut escaped = false;
        for (pos, c) in word.char_indices() {
            if escaped {
                escaped = false;
                continue;
            }
            match c {
                '\\' if !in_s => escaped = true,
                '\'' if !in_d => in_s = !in_s,
                '"' if !in_s => in_d = !in_d,
                '<' | '>' if !in_s && !in_d => {
                    split = Some(pos);
                    break;
                }
                _ => {}
            }
        }
        match split {
            Some(pos)
                if pos > 0
                    && !word[..pos].chars().all(|c| c.is_ascii_digit())
                    && &word[..pos] != "&" =>
            {
                out.push(word[..pos].to_string());
                out.push(word[pos..].to_string());
            }
            _ => out.push(word),
        }
    }
    out
}
