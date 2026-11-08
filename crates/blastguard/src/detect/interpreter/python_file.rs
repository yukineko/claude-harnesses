//! The Python judge for a QUOTED here-document body that `cat`/`tee` writes
//! to a literal `.py` file (user ruling 2026-11-08; the shape rule is
//! [`super::super::heredoc::python_file_here_document`]). Kept apart from
//! `interpreter.rs` for the file-size cap.
//!
//! Such a body is CODE (a later `python3 edit.py` runs it), so it is not data;
//! but the shell never parses it, so it is not shell lines either. Read line by
//! line as shell, a Python comment or docstring holding `` `git stash clear` ``
//! was denied as a command substitution, while
//! `subprocess.run(["git", "stash", "clear"])` followed by `python3 edit.py`
//! was allowed — the shell judge sees nothing harmful in Python syntax
//! (tests/quoted_heredoc_backtick_fp_20261108.rs).
//!
//! Here comments and string literals are inert, and the calls that hand a
//! command to the OS are judged by the shell rules applied to that command.

use super::super::{
    is_shell, normalized_command, quote_aware_words, split_segments, wrapped_command_heads,
};
use super::{code_building_marker, shell_calls, Ctx, Decision};

/// The verdict on the Python program `body`, or `None` when the body must stay
/// judged as shell lines, exactly as before the ruling:
///   * its first line is a `#!` line that does not name python (it is a script
///     for another interpreter);
///   * a line of `after` (the command lines after the closing delimiter) could
///     run the file as a shell script: a segment with a shell word anywhere in
///     it (`sh edit.py`, `| bash`, `xargs sh`), or headed by `source` / `.` /
///     `eval`, or headed by a `.py` path (`./edit.py` without a shebang is run
///     by `sh`).
///
/// Otherwise, Deny over Ask over Allow:
///   * a body that cannot be tokenised as Python (an unterminated string, an
///     unbalanced bracket) is an Ask: string boundaries are unknown, so no
///     call in it can be shown inert;
///   * every OS-facing call is judged from its literal arguments by
///     [`shell_calls::program_call_verdict`] (`os.system("…")` and
///     `subprocess.run([...])` as the command they run), on the raw body and on
///     its code view (comments and plain strings blanked, f-string `{…}` code
///     kept), so a call inside an f-string replacement field is judged too;
///   * a call whose command is computed ([`shell_calls::computed_shell_call`]:
///     `os.system(cmd)`) is an Ask;
///   * a [`super::CODE_BUILDING`] marker in the code view (`exec(`, `eval(`,
///     `__import__`, …) is an Ask: the text is not the program that runs.
///
/// Text in a comment or in a plain string literal is never judged, so a
/// docstring that names `git stash clear` is Allow. NOT covered, and not judged
/// here: what the program does through calls that are not OS commands
/// (`shutil.rmtree`, `open(p, 'w')`). A file written here and run by a LATER,
/// separate tool call is judged on that call's own text, as for data files.
pub(in crate::detect) fn python_file_body_verdict(
    body: &str,
    after: &str,
    depth: usize,
    ctx: &Ctx<'_>,
) -> Option<Decision> {
    let first = body.lines().next().unwrap_or("");
    if first.starts_with("#!") && !first.contains("python") {
        return None;
    }
    if after_may_run_as_shell(after) {
        return None;
    }
    let shape = "the here-document writes a Python file";
    let Some(code) = code_view(body) else {
        return Some(Decision::ask(format!(
            "{shape}, and its program cannot be tokenised as Python (an unterminated string or an \
unbalanced bracket) — blastguard cannot tell its code from its strings, so it cannot vouch for it"
        )));
    };
    let mut pending: Option<Decision> = None;
    for text in [body, code.as_str()] {
        match shell_calls::program_call_verdict(text, depth, ctx) {
            d @ Decision::Deny(_) => return Some(d),
            a @ Decision::Ask(_) => {
                pending.get_or_insert(a);
            }
            Decision::Allow => {}
        }
    }
    if let Some(call) = shell_calls::computed_shell_call(&code) {
        pending.get_or_insert(Decision::ask(format!(
            "{shape}, and that program runs a command it computes (`{call}` with a non-literal \
argument) — blastguard cannot see what would actually execute"
        )));
    }
    if let Some(marker) = code_building_marker(&code) {
        pending.get_or_insert(Decision::ask(format!(
            "{shape}, and that program builds or decodes the code it runs (`{marker}`) — the text \
is not the program that will execute, so blastguard cannot tell whether anything here is \
irreversible"
        )));
    }
    Some(pending.unwrap_or(Decision::Allow))
}

/// Could a line of `after` run the written file as a shell script? See
/// [`python_file_body_verdict`]. A false hit only keeps the shell judging.
fn after_may_run_as_shell(after: &str) -> bool {
    split_segments(after).iter().any(|seg| {
        let words = quote_aware_words(seg);
        let refs: Vec<&str> = words.iter().map(String::as_str).collect();
        refs.iter().any(|w| is_shell(&normalized_command(w)))
            || wrapped_command_heads(&refs).into_iter().any(|h| {
                let head = normalized_command(refs[h]);
                matches!(head.as_str(), "source" | "." | "eval") || head.ends_with(".py")
            })
    })
}

/// `src` with `#` comments removed and the contents of plain string literals
/// replaced by spaces (newlines kept; quotes and prefixes kept), followed by
/// the code of every f-string replacement field, one per line. `None` when
/// `src` does not tokenise: a string not closed (a one-quote string at its
/// newline, any string at the end of the text), a bracket closed that was
/// never opened, or a bracket left open.
fn code_view(src: &str) -> Option<String> {
    let chars: Vec<char> = src.chars().collect();
    let mut out = String::with_capacity(src.len());
    let mut fields = String::new();
    let mut depth = 0usize;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '#' {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if c == '\'' || c == '"' {
            let prefix: String = out
                .chars()
                .rev()
                .take_while(|p| p.is_ascii_alphanumeric() || *p == '_')
                .collect();
            let fstring = prefix.len() <= 2 && prefix.chars().any(|p| p == 'f' || p == 'F');
            let triple = chars.get(i + 1) == Some(&c) && chars.get(i + 2) == Some(&c);
            let q = if triple { 3 } else { 1 };
            out.extend(std::iter::repeat_n(c, q));
            i += q;
            let mut content = String::new();
            let mut closed = false;
            while i < chars.len() {
                let d = chars[i];
                if d == '\\' {
                    let e = *chars.get(i + 1)?;
                    content.push(d);
                    content.push(e);
                    out.push(' ');
                    out.push(if e == '\n' { '\n' } else { ' ' });
                    i += 2;
                    continue;
                }
                if d == '\n' && !triple {
                    return None;
                }
                if d == c
                    && (!triple || (chars.get(i + 1) == Some(&c) && chars.get(i + 2) == Some(&c)))
                {
                    out.extend(std::iter::repeat_n(c, q));
                    i += q;
                    closed = true;
                    break;
                }
                content.push(d);
                out.push(if d == '\n' { '\n' } else { ' ' });
                i += 1;
            }
            if !closed {
                return None;
            }
            if fstring {
                for field in fstring_fields(&content) {
                    fields.push('\n');
                    fields.push_str(&field);
                }
            }
            continue;
        }
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth = depth.checked_sub(1)?,
            _ => {}
        }
        out.push(c);
        i += 1;
    }
    if depth != 0 {
        return None;
    }
    out.push_str(&fields);
    Some(out)
}

/// The replacement-field code of an f-string body: the text of each top-level
/// `{…}` (`{{` / `}}` are literal braces). A field left open runs to the end.
fn fstring_fields(content: &str) -> Vec<String> {
    let chars: Vec<char> = content.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '{' && chars.get(i + 1) == Some(&'{') {
            i += 2;
            continue;
        }
        if chars[i] != '{' {
            i += 1;
            continue;
        }
        let mut depth = 1usize;
        let mut field = String::new();
        i += 1;
        while i < chars.len() {
            match chars[i] {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                _ => {}
            }
            field.push(chars[i]);
            i += 1;
        }
        out.push(field);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{code_view, fstring_fields};

    #[test]
    fn code_view_blanks_strings_and_comments_and_keeps_fstring_fields() {
        let v = code_view("s = 'a `x` b'  # c `y`\nt = f\"{os.system(c)}\"\n").unwrap();
        assert!(!v.contains('`'), "{v}");
        assert!(v.contains("os.system(c)"), "{v}");
        assert_eq!(
            fstring_fields("a {{b}} {c:>3} {d['k']}"),
            vec!["c:>3", "d['k']"]
        );
    }

    #[test]
    fn code_view_refuses_what_does_not_tokenise() {
        assert_eq!(code_view("s = 'abc\nx = 1\n"), None);
        assert_eq!(code_view("s = \"\"\"abc\n"), None);
        assert_eq!(code_view("f(x\n"), None);
        assert_eq!(code_view("x)\n"), None);
        assert!(code_view("\"\"\"\nmulti\nline\n\"\"\"\n").is_some());
    }
}
