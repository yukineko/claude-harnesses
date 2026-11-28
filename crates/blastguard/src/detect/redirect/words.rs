//! One place that reads redirect operators out of a word list (backlog
//! ffa05737 D1/D2, 0e79c339 D3).
//!
//! Three consumers used to disagree about what a redirect word is:
//!
//! * the text scanners ([`super::redirect::redirect_targets`]) read the word
//!   after `>` as the target, so zsh's clobber form `>! /p` (the Bash tool's
//!   shell is zsh) gave the target `!` and left `/p` unjudged;
//! * `git add -A 2>&1` counted `2>&1` as a pathspec, which turned the
//!   whole-tree sweep into "limited to a path" (Allow);
//! * `rm -rf build 2>&1` counted `2>&1` as an operand and judged it as a path
//!   (Deny).
//!
//! [`redirect_word_shape`] is the single definition of "this word is a
//! redirect operator, and does / does not own the next word". Anything that is
//! not recognisably one stays an ordinary word (the previous reading); the
//! token-level stripper additionally refuses to touch a word list whose
//! quoting it cannot follow, so a quoted `>` is never mistaken for a redirect.

use super::{redirect_word_end, skip_redirect_word_gap};

/// Shape of a redirect word: `Some(true)` when the operator stands alone and
/// the FOLLOWING word is its target (`>`, `>>`, `2>`, `&>`, `>!`, `>>!`,
/// `2>!`, `&>!`, `>|`, `>&`, `<`, `<<`), `Some(false)` when the target is
/// attached (`2>&1`, `>log`, `2>/dev/null`, `>!log`), `None` when the word
/// does not start with a redirect operator.
pub(in crate::detect) fn redirect_word_shape(tok: &str) -> Option<bool> {
    let s = tok.trim_start_matches(|c: char| c.is_ascii_digit() || c == '&');
    if !s.starts_with(['>', '<']) {
        return None;
    }
    let rest = s.trim_start_matches(['>', '<']);
    Some(matches!(rest, "" | "!" | "|" | "&"))
}

/// The words of `rest` without their redirections. When the quoting of the
/// list cannot be followed (a backslash, a backquote, `$(`, or a quote that
/// is open across words — `rest` is a whitespace split, so a quoted `>` may
/// stand as a word of its own) the list is returned unchanged: the previous
/// reading, never a word dropped from a string.
pub(in crate::detect) fn strip_redirect_words<'a>(rest: &[&'a str]) -> Vec<&'a str> {
    if rest
        .iter()
        .any(|t| t.contains(['\\', '`', '\'', '"']) || t.contains("$("))
    {
        return rest.to_vec();
    }
    let mut out = Vec::with_capacity(rest.len());
    let mut skip_next = false;
    for t in rest {
        if std::mem::take(&mut skip_next) {
            continue;
        }
        match redirect_word_shape(t) {
            Some(owns_next) => skip_next = owns_next,
            None => out.push(*t),
        }
    }
    out
}

/// zsh's clobber redirect `>!` / `>>!` / `2>!` / `&>!` (and `>!file`) reaches
/// the scanners as the target `!` (or `!file`). When the target word
/// `seg[start..end]` begins with `!`, the path zsh really opens is the rest of
/// that word, or the next word when `!` stands alone. Returned with the end of
/// the text it covers. The bash reading (a file named `!…`) stays judged by
/// the caller as before; this is the second candidate, so a protected path
/// behind `>!` is never an unjudged argument. `None` when there is no second
/// reading.
pub(in crate::detect) fn clobber_bang_target(
    seg: &str,
    bytes: &[u8],
    start: usize,
    end: usize,
) -> Option<(String, usize)> {
    let tok = seg.get(start..end)?;
    let rest = tok.strip_prefix('!')?;
    if !rest.is_empty() {
        return Some((rest.to_string(), end));
    }
    let k = skip_redirect_word_gap(bytes, end);
    let e = redirect_word_end(bytes, k);
    (k < e).then(|| (seg[k..e].to_string(), e))
}
