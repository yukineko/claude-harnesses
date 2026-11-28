//! The ONE entry for the quote removal every path judgement needs (backlog
//! 5c848034).
//!
//! A raw operand token keeps its shell quoting: `"/etc"/f`, `settings'.json'`,
//! `--work-tree="~/.claude"`. The shell removes the quotes before the command
//! sees the word, so a judgement that matches the raw token against a
//! system-directory list, a protected glob, a physical landing or a `-C`
//! directory judges a path nobody writes (`cp x "/etc"/f` was Allow while
//! `cp x /etc/f` is Deny). Removing the quotes at each caller was the recurring
//! gap: a new caller, a new spelling hole.
//!
//! The path judgements ([`super::protected`], `git_moved_dir`) therefore call
//! [`plain`] FIRST and use its value for everything after. The removal is done
//! by the shell word parser ([`first_shell_word`]), once; a word that still
//! needs real expansion (`$`, backtick) is returned untouched so the callers'
//! own unresolvable-operand handling applies.

use std::borrow::Cow;

use crate::detect::words::first_shell_word;

/// The operand with its shell quoting removed, or `None` when nothing changes
/// or the result still needs real expansion (`$`, backtick).
pub(super) fn unquoted_operand(word: &str) -> Option<String> {
    let plain = first_shell_word(word)?;
    if plain == word || plain.contains(['$', '`']) {
        return None;
    }
    Some(plain)
}

/// [`unquoted_operand`], or the word itself when there is nothing to remove.
pub(in crate::detect) fn plain(word: &str) -> Cow<'_, str> {
    match unquoted_operand(word) {
        Some(p) => Cow::Owned(p),
        None => Cow::Borrowed(word),
    }
}
