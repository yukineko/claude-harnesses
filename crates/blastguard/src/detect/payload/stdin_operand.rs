//! Where an interpreter's argv names its own stdin as the program (`-`,
//! `/dev/stdin` …), read on shell words rather than whitespace tokens
//! (backlog 32b414d2). Split out of `payload.rs` (file-growth cap).

use super::{argv_words, is_shell, normalized_command, shell_program_source, ProgramSource};

/// The stdin-marker operand of the reader at `tokens[idx]` (see
/// [`interpreter_stdin_operand`]). For a SHELL whose program is a `-c`
/// string, the words from the `-c` option on are that program and its `$0`,
/// `$1` … operands, never where the shell reads its program: `bash -c 'cat' -`
/// runs `cat`, and `-` is `$0`. Only the option words BEFORE `-c` are scanned
/// (an `--rcfile /dev/stdin` there is still a stdin program). The `-c` word is
/// found with `shell_program_source` itself — the shortest argv prefix it
/// reads as inline — so the two option walks cannot drift.
pub(super) fn stage_stdin_operand<'a>(tokens: &[&'a str], idx: usize) -> Option<&'a str> {
    let rest = &tokens[idx + 1..];
    if is_shell(&normalized_command(tokens[idx])) {
        let argv = argv_words(rest);
        if let Some(p) =
            (0..argv.len()).find(|&p| shell_program_source(&argv[..=p]) == ProgramSource::Inline)
        {
            return interpreter_stdin_operand(&argv[..p]);
        }
    }
    interpreter_stdin_operand(rest)
}

/// The operand among an interpreter's argv tokens that names its own stdin
/// (`-`, `/dev/stdin`, `/dev/fd/0`, `/proc/self/fd/0`, `/dev/tty`).
///
/// `rest` is whitespace-split, which ignores quoting: `zsh -c 'echo - foo'`
/// arrives as `-c`, `'echo`, `-`, `foo'`, and the `-` INSIDE the quoted `-c`
/// program was taken for the stdin marker (backlog 32b414d2). The tokens are
/// first regrouped into shell words (`shell_word_spans`): a token that leaves
/// a quote open, or ends in an unquoted backslash (an escaped blank), is joined
/// with the next. Only a word that is exactly one token can be a marker — a
/// marker contains no quote or blank, so a word spanning several tokens never
/// is one.
///
/// When the quoting does not close (undetermined word boundaries), every raw
/// token is scanned as before: the restrictive reading.
pub(in crate::detect) fn interpreter_stdin_operand<'a>(rest: &[&'a str]) -> Option<&'a str> {
    fn is_marker(t: &str) -> bool {
        matches!(
            t,
            "-" | "/dev/stdin" | "/dev/fd/0" | "/proc/self/fd/0" | "/dev/tty"
        )
    }
    match shell_word_spans(rest) {
        Some(spans) => spans
            .into_iter()
            .filter(|&(start, end)| end == start + 1)
            .map(|(start, _)| rest[start])
            .find(|t| is_marker(t)),
        None => rest.iter().find(|t| is_marker(t)).copied(),
    }
}

/// Regroup whitespace-split tokens into shell words: `(start, end)` token
/// ranges, a word spanning several tokens when a quote opened in one closes in
/// a later one, or a token ends in an unquoted backslash (escaping the blank
/// the split removed). `None` when a quote is still open (or a backslash
/// escapes nothing) after the last token: the word boundaries are not
/// determined.
fn shell_word_spans(tokens: &[&str]) -> Option<Vec<(usize, usize)>> {
    let mut spans = Vec::new();
    let mut quote: Option<char> = None;
    let mut start = 0;
    for (k, token) in tokens.iter().enumerate() {
        let mut escaped_end = false;
        let mut chars = token.chars();
        while let Some(c) = chars.next() {
            match (quote, c) {
                (None | Some('"'), '\\') => {
                    if chars.next().is_none() {
                        escaped_end = true;
                    }
                }
                (None, '\'' | '"') => quote = Some(c),
                (Some(q), c) if c == q => quote = None,
                _ => {}
            }
        }
        if quote.is_none() && !escaped_end {
            spans.push((start, k + 1));
            start = k + 1;
        }
    }
    (start == tokens.len()).then_some(spans)
}
