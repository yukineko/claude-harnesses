//! The program word of an inline-eval flag (backlog d6de38be). Split out of
//! `payload.rs` to keep that file under the file-growth cap.

use super::{first_shell_word, shell_word_token_span, tail_text};

/// The PROGRAM word of an inline-eval flag whose interpreter ends its option
/// list at that flag (`python3 -c PROG arg…`): the one shell word holding the
/// program, unquoted. `inline` is the in-token remainder glued to the flag
/// (`-cPROG`, `--x=PROG`), empty when the value is the next token. Every word
/// after this one is an `argv` operand, not program text (backlog d6de38be).
///
/// `None` when the word is undetermined — no word follows the flag, or its
/// quote never closes — so a caller cannot use it to narrow anything.
pub(in crate::detect) fn inline_eval_program_word(
    rest: &[&str],
    pos: usize,
    inline: &str,
    source: Option<&str>,
) -> Option<String> {
    let mut words: Vec<&str> = Vec::with_capacity(rest.len() + 1);
    if !inline.is_empty() {
        words.push(inline);
    }
    words.extend_from_slice(rest.get(pos + 1..)?);
    if words.is_empty() {
        return None;
    }
    shell_word_token_span(&words, 0)?;
    first_shell_word(&tail_text(source, &words, 0))
}
