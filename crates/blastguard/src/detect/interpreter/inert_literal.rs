//! Backlog 27795e6e: the classifier behind the string-only downgrade of
//! [`super::interpreter_program_verdict`] — does every occurrence of a
//! destructive spelling in layer-0 program text sit inside a quoted literal
//! that demonstrably cannot run code? Kept apart from `interpreter.rs` only
//! for the file-size cap; it is private to that module.

use super::generic_quote_literals;

/// Markers (matched case-insensitively as substrings) of a program that can
/// run, or reach, code it did not spell out literally. Deliberately broad: a
/// false hit only keeps the stricter Deny.
const DYNAMIC_EXECUTION_MARKERS: &[&str] = &[
    // Python / generic dynamic evaluation and indirection.
    "exec",
    "eval",
    "compile",
    "__import__",
    "getattr",
    "setattr",
    "importlib",
    "__builtins__",
    "builtins",
    "__dict__",
    "globals(",
    "locals(",
    "vars(",
    "os.system",
    "os.popen",
    "os.exec",
    "os.spawn",
    "subprocess",
    "runpy",
    "pickle",
    "marshal",
    "code.",
    "types.",
    "ctypes",
    // Non-Python interpreters: eval / require / system / send equivalents.
    "system(",
    "system ",
    "require(",
    "require ",
    "function(",
    "child_process",
    ".send",
    ".call(",
    ".apply(",
    "instance_eval",
    "class_eval",
    "`",
];

/// Does `program` contain any [`DYNAMIC_EXECUTION_MARKERS`] — could a string in
/// it plausibly be fed to dynamic execution?
///
/// A substring heuristic, not a proof: obfuscated execution can evade it. It is
/// therefore never sufficient on its own; it is one of two preconditions of the
/// inert-literal downgrade, the other being [`inert_literal_bodies`]. A hit
/// keeps the Deny.
pub(super) fn has_dynamic_code_execution(program: &str) -> bool {
    let lowered = program.to_ascii_lowercase();
    DYNAMIC_EXECUTION_MARKERS
        .iter()
        .any(|marker| lowered.contains(marker))
}

/// Characters after which a `'`/`"` may open a literal this classifier is
/// willing to call inert. Anything else that is not an identifier character
/// (`$"`, `?"`, `%"`, `\"`, `/"`, …) is a construct whose quoting this scanner
/// does not model, so it gives up.
fn inert_opener_context(prev: Option<char>) -> bool {
    match prev {
        None => true,
        Some(c) => c.is_whitespace() || matches!(c, '(' | '[' | '{' | ',' | '=' | ':' | ';' | '+'),
    }
}

fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Backlog 27795e6e. The bodies of the literals that hold every occurrence of
/// `spelling` in the layer-0 program text `program`, or `None` whenever that
/// cannot be POSITIVELY established. `Some` means all of:
///
/// * every occurrence of `spelling` (case-folded, overlapping occurrences
///   included) lies entirely inside the body of a single- or double-quoted
///   literal that is inert, or entirely inside what [`super::strip_hash_comments`]
///   reads as a `#` comment (the same rule: a `#` outside quotes at the start of
///   a line or after whitespace). Comment occurrences are left to the existing
///   comment-mention Ask; at least one occurrence must be in an inert literal;
/// * a literal is inert only if its opening quote follows the start of the text,
///   whitespace or one of `( [ { , = : ; +` — an identifier character directly
///   before it (`f"`, `r'`, `b"`, `rb"`, `qq"`, …) makes it a prefixed literal
///   that may interpolate or be something else, so an occurrence there is not
///   inert — and its body contains none of `{` `$` `@` `` ` `` `\` or a line
///   break (interpolation, escapes and multi-line quoting are not modelled);
/// * every quote in the program is terminated, no closing quote is directly
///   followed by an identifier character or another quote, and no quote is
///   escaped or opened after any other character;
/// * the program contains no backtick and no `"""`/`'''` (triple-quoted strings
///   are not modelled), and the code outside literals and comments contains no
///   `/`, `--`, `<<`, `[[`, Perl/Ruby generic quote, or line-initial `=word` —
///   regexes, other comment styles, here-documents, long strings and POD all
///   change what a `"` means, and a misread quote could make real code look
///   like a literal.
///
/// Anything else returns `None`. This is not a parser for any language; it
/// answers only "is every occurrence demonstrably inside a plain quoted
/// literal", and an answer it cannot give is `None`, which keeps the Deny.
pub(super) fn inert_literal_bodies<'a>(program: &'a str, spelling: &str) -> Option<Vec<&'a str>> {
    if spelling.is_empty()
        || program.contains('`')
        || program.contains("\"\"\"")
        || program.contains("'''")
    {
        return None;
    }
    let chars: Vec<(usize, char)> = program.char_indices().collect();
    let mut inert: Vec<(usize, usize)> = Vec::new();
    let mut prefixed: Vec<(usize, usize)> = Vec::new();
    let mut comments: Vec<(usize, usize)> = Vec::new();
    // `program` with literal bodies and comments replaced by a space: what the
    // scan reads as code.
    let mut code = String::with_capacity(program.len());
    let mut prev: Option<char> = None;
    let mut i = 0usize;
    while i < chars.len() {
        let (at, c) = chars[i];
        if c == '#' && prev.is_none_or(char::is_whitespace) {
            let mut j = i;
            while j < chars.len() && chars[j].1 != '\n' {
                j += 1;
            }
            let end = chars.get(j).map_or(program.len(), |&(b, _)| b);
            comments.push((at, end));
            code.push(' ');
            prev = Some(chars[j - 1].1);
            i = j;
            continue;
        }
        if c == '\\' {
            let next = chars.get(i + 1).map(|&(_, d)| d);
            if matches!(next, Some('"' | '\'')) {
                return None;
            }
            code.push(c);
            if let Some(d) = next {
                code.push(d);
            }
            prev = next.or(Some(c));
            i += 2;
            continue;
        }
        if c == '"' || c == '\'' {
            let eligible = if inert_opener_context(prev) {
                true
            } else if prev.is_some_and(is_ident_char) {
                false
            } else {
                return None;
            };
            let mut j = i + 1;
            let mut escaped_body = false;
            loop {
                let &(_, d) = chars.get(j)?;
                if d == '\\' {
                    escaped_body = true;
                    j += 2;
                    continue;
                }
                if d == c {
                    break;
                }
                j += 1;
            }
            let close_at = chars[j].0;
            if chars
                .get(j + 1)
                .is_some_and(|&(_, d)| is_ident_char(d) || d == '"' || d == '\'')
            {
                return None;
            }
            let range = (at + c.len_utf8(), close_at);
            let body = &program[range.0..range.1];
            if eligible && !escaped_body && !body.contains(['{', '$', '@', '`', '\\', '\n', '\r']) {
                inert.push(range);
            } else {
                prefixed.push(range);
            }
            code.push(c);
            code.push(' ');
            code.push(c);
            prev = Some(c);
            i = j + 1;
            continue;
        }
        code.push(c);
        prev = Some(c);
        i += 1;
    }
    if code.contains('/')
        || code.contains("--")
        || code.contains("<<")
        || code.contains("[[")
        || !generic_quote_literals(&code).is_empty()
        || code.lines().any(|l| {
            l.strip_prefix('=')
                .is_some_and(|r| r.starts_with(|c: char| c.is_ascii_alphabetic()))
        })
    {
        return None;
    }
    let lowered = program.to_ascii_lowercase();
    let spelling = spelling.to_ascii_lowercase();
    let within = |s: usize, e: usize, ranges: &[(usize, usize)]| {
        ranges.iter().copied().find(|&(a, b)| a <= s && e <= b)
    };
    let mut bodies: Vec<&'a str> = Vec::new();
    for s in (0..lowered.len()).filter(|&s| lowered.is_char_boundary(s)) {
        if !lowered[s..].starts_with(spelling.as_str()) {
            continue;
        }
        let e = s + spelling.len();
        if within(s, e, &comments).is_some() {
            continue;
        }
        let (a, b) = within(s, e, &inert)?;
        let body = &program[a..b];
        if !bodies.contains(&body) {
            bodies.push(body);
        }
    }
    if bodies.is_empty() {
        return None;
    }
    Some(bodies)
}

#[cfg(test)]
mod inert_literal_tests {
    use super::{has_dynamic_code_execution, inert_literal_bodies};

    const S: &str = "os.unlink(";

    fn inert(program: &str) -> bool {
        inert_literal_bodies(program, S).is_some()
    }

    #[test]
    fn backlog_program_is_inert_and_yields_its_body() {
        let p = "x = \"docs: os.unlink( removes a file\"\nprint(x)\n";
        assert_eq!(
            inert_literal_bodies(p, S),
            Some(vec!["docs: os.unlink( removes a file"])
        );
        assert!(!has_dynamic_code_execution(p));
    }

    #[test]
    fn single_quoted_and_case_folded_is_inert() {
        assert!(inert("print('see OS.UNLINK( docs')"));
    }

    #[test]
    fn comment_occurrence_is_left_to_the_comment_rule() {
        assert!(inert("# os.unlink( here\nx = 'os.unlink( there'\n"));
        // Comment-only: nothing in a literal, so nothing is proven.
        assert!(!inert("# os.unlink( here\nprint(1)\n"));
    }

    #[test]
    fn occurrence_outside_any_literal_is_not_inert() {
        assert!(!inert("import os\nos.unlink('/tmp/x')\n"));
        assert!(!inert("x = 'os.unlink( docs'\nos.unlink('/tmp/x')\n"));
    }

    #[test]
    fn prefixed_literals_are_code() {
        for p in [
            "x = f\"docs os.unlink( x\"",
            "x = F'docs os.unlink( x'",
            "x = r\"docs os.unlink( x\"",
            "x = b\"docs os.unlink( x\"",
            "x = rb'docs os.unlink( x'",
            "x = fr\"docs os.unlink( x\"",
            "x = qq\"docs os.unlink( x\"",
        ] {
            assert!(!inert(p), "{p:?}");
        }
    }

    #[test]
    fn interpolation_characters_in_the_body_are_not_inert() {
        for p in [
            "x = \"{os.unlink('/tmp/x')}\"",
            "x = \"#{os.unlink( x}\"",
            "x = \"$(true) os.unlink( x\"",
            "x = \"@{[ 1 ]} os.unlink( x\"",
            "x = \"a \\\" os.unlink( x\"",
        ] {
            assert!(!inert(p), "{p:?}");
        }
    }

    #[test]
    fn backtick_anywhere_is_not_inert() {
        assert!(!inert("x = `echo os.unlink( x`"));
        assert!(!inert("x = 'os.unlink( x'\ny = `true`"));
    }

    #[test]
    fn unterminated_quote_is_not_inert() {
        assert!(!inert("x = \"docs: os.unlink( removes a file\nprint(x)\n"));
        assert!(!inert("x = 'os.unlink( x'\ny = \"open"));
    }

    #[test]
    fn triple_quoted_strings_are_not_modelled() {
        assert!(!inert("x = \"\"\"os.unlink( x\"\"\""));
        assert!(!inert("x = '''os.unlink( x'''"));
    }

    #[test]
    fn multi_line_body_is_not_inert() {
        assert!(!inert("x = 'a\nos.unlink( x'"));
    }

    #[test]
    fn unmodelled_quoting_contexts_are_not_inert() {
        for p in [
            // A regex / other comment style / here-doc can change what a quote means.
            "y = 6 / 2\nx = 'os.unlink( x'",
            "y = 1 -- 2\nx = 'os.unlink( x'",
            "x = <<EOS\n'os.unlink( x'\nEOS",
            "x = [[ 'os.unlink( x' ]]",
            "=pod\nx = 'os.unlink( x'",
            "x = q( 'os.unlink( x' )",
            // Quote opened after an unmodelled character, or escaped outside.
            "c = ?\"; x = 'os.unlink( x'",
            "print $\"; x = 'os.unlink( x'",
            "x = \\\"; y = 'os.unlink( x'",
            // Closing quote glued to an identifier or another quote.
            "x = 'os.unlink( x'y",
            "x = 'os.unlink( x''z'",
        ] {
            assert!(!inert(p), "{p:?}");
        }
    }

    #[test]
    fn dynamic_execution_markers_are_detected() {
        assert!(has_dynamic_code_execution("exec(\"x\")"));
        assert!(has_dynamic_code_execution("getattr(os, 'remove')"));
        assert!(has_dynamic_code_execution("x = `ls`"));
        assert!(!has_dynamic_code_execution("print('hi')"));
    }
}
