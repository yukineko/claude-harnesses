//! Shapes [`super::write_prefix_cannot_relink`] admits in the prefix of a
//! truncating redirect because they cannot change which file the `>` opens
//! (backlog 1b23e133).

use super::has_glob_meta;

/// `text` with ONE trailing quoted here-document opener (`<<'TAG'`,
/// `<<-'TAG'`, `<<"TAG"`; TAG alphanumeric/`_`) and any whitespace after it
/// removed; otherwise `text` unchanged (so every other `<` still refuses).
/// The body of a quoted here-document is data the shell never expands, is
/// already removed from the line judged, and feeds stdin only.
pub(super) fn strip_quoted_heredoc_opener(text: &str) -> &str {
    let t = text.trim_end();
    let Some(pos) = t.rfind("<<") else {
        return text;
    };
    let rest = t[pos + 2..].strip_prefix('-').unwrap_or(&t[pos + 2..]);
    let quoted_tag = |q: char| {
        rest.strip_prefix(q)
            .and_then(|r| r.strip_suffix(q))
            .is_some_and(|tag| {
                !tag.is_empty() && tag.chars().all(|c| c.is_alphanumeric() || c == '_')
            })
    };
    if quoted_tag('\'') || quoted_tag('"') {
        &t[..pos]
    } else {
        text
    }
}

/// True for `mkdir [-p] DIR...` (words after the verb in `ops`): it only ADDS
/// directory entries where none exists and never renames, removes or replaces
/// one, nor creates a symlink; the redirect target is an existing file
/// observed at judge time, so its resolution cannot change. Only the plain
/// `-p` option, no `$` or glob, and at least one operand.
pub(super) fn mkdir_is_inert(ops: &[&str]) -> bool {
    let plain = |w: &&str| !w.starts_with('-') && !w.contains('$') && !has_glob_meta(w);
    ops.iter().all(|w| *w == "-p" || plain(w)) && ops.iter().any(plain)
}
