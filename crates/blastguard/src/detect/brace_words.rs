//! Brace expansion at the segment word layer (backlog 66e7a741, 46145358).
//!
//! The shell expands `{a,b}` BEFORE it execs anything, wherever the word
//! stands: `git reset {--hard,HEAD}` runs `git reset --hard HEAD` and
//! `{rm,-rf} /etc` runs `rm -rf /etc`. The detectors read the words as written,
//! so a flag or the command word hidden in a brace group was never seen
//! (`git reset {--hard,HEAD}` was Allow, `{rm,-rf} /etc` only an Ask). The
//! expansion therefore happens ONCE, here, and every detector is fed the word
//! list the shell passes — not per-detector patches.
//!
//! What is expanded: static comma groups, nested and repeated, in unquoted,
//! unescaped words ([`super::rm::expand_braces_in_word`], the same expander the
//! `rm` operand reader uses). What is left as written: assignment words
//! (`X={a,b}` is not brace-expanded by the shell), quoted text, words that
//! contain a substitution opener or closer (their bodies are analysed on their
//! own as payloads), any word that starts inside a quote opened by an earlier
//! word, literal `{x}` / `{}`, and sequence groups (`{a..c}`).
//! What cannot be established — an expansion past the bound — is
//! [`SegmentBraces::TooMany`], which the caller resolves to Ask.

use super::rm::{expand_braces_in_word, BraceWord};

/// Upper bound on the words one segment may brace-expand into in total.
const MAX_SEGMENT_EXPANSIONS: usize = 256;

pub(super) enum SegmentBraces {
    /// No word changes; judge the segment as written.
    Unchanged,
    /// The segment text with every expandable word replaced by its expansion.
    Expanded(String),
    /// The expansion exceeds the bound: what the shell runs is not established.
    TooMany,
}

/// Quote state carried across whitespace-separated tokens (a quoted string may
/// span several of them).
#[derive(Clone, Copy, PartialEq)]
enum Quote {
    None,
    Single,
    Double,
}

fn advance(mut q: Quote, tok: &str) -> Quote {
    let mut chars = tok.chars();
    while let Some(c) = chars.next() {
        q = match (q, c) {
            (Quote::Single, '\'') => Quote::None,
            (Quote::Single, _) => Quote::Single,
            (Quote::Double, '"') => Quote::None,
            (Quote::Double, '\\') => {
                chars.next();
                Quote::Double
            }
            (Quote::Double, _) => Quote::Double,
            (Quote::None, '\\') => {
                chars.next();
                Quote::None
            }
            (Quote::None, '\'') => Quote::Single,
            (Quote::None, '"') => Quote::Double,
            (Quote::None, _) => Quote::None,
        };
    }
    q
}

/// `NAME=...` / `NAME+=...`: an assignment word, which the shell does not
/// brace-expand.
fn is_assignment_word(tok: &str) -> bool {
    let Some(eq) = tok.find('=') else {
        return false;
    };
    let name = tok[..eq].strip_suffix('+').unwrap_or(&tok[..eq]);
    super::vars::is_shell_identifier(name)
}

pub(super) fn expand_segment(seg: &str) -> SegmentBraces {
    if !seg.contains('{') {
        return SegmentBraces::Unchanged;
    }
    let mut out = String::with_capacity(seg.len());
    let mut copied = 0usize;
    let mut changed = false;
    let mut total = 0usize;
    let mut quote = Quote::None;
    for tok in seg.split_whitespace() {
        let start = tok.as_ptr() as usize - seg.as_ptr() as usize;
        let at_top = quote == Quote::None;
        quote = advance(quote, tok);
        if !at_top || is_assignment_word(tok) || tok.contains(['(', ')', '`']) {
            continue;
        }
        match expand_braces_in_word(tok) {
            BraceWord::Unchanged => {}
            BraceWord::TooMany => return SegmentBraces::TooMany,
            BraceWord::Words(words) => {
                total += words.len();
                if total > MAX_SEGMENT_EXPANSIONS {
                    return SegmentBraces::TooMany;
                }
                out.push_str(&seg[copied..start]);
                out.push_str(&words.join(" "));
                copied = start + tok.len();
                changed = true;
            }
        }
    }
    if !changed {
        return SegmentBraces::Unchanged;
    }
    out.push_str(&seg[copied..]);
    SegmentBraces::Expanded(out)
}

/// The Ask for a segment whose expansion exceeds the bound.
pub(super) fn too_many_ask() -> crate::model::Decision {
    crate::model::Decision::ask(
        "this command line brace-expands into more words than blastguard will enumerate, so \
what the shell runs is not established — it refuses to guess"
            .to_string(),
    )
}

/// `seg` as the shell word list it expands to; `Err` when the expansion
/// exceeds the bound. For the line-level scans that read a segment's words
/// themselves (the high-blast tier), so they see what [`expand_segment`]'s
/// caller sees.
pub(super) fn expanded_text(seg: &str) -> Result<std::borrow::Cow<'_, str>, ()> {
    match expand_segment(seg) {
        SegmentBraces::Unchanged => Ok(std::borrow::Cow::Borrowed(seg)),
        SegmentBraces::Expanded(e) => Ok(std::borrow::Cow::Owned(e)),
        SegmentBraces::TooMany => Err(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exp(s: &str) -> Option<String> {
        match expand_segment(s) {
            SegmentBraces::Expanded(e) => Some(e),
            _ => None,
        }
    }

    #[test]
    fn command_and_flag_words_expand() {
        assert_eq!(exp("{rm,-rf} /etc").as_deref(), Some("rm -rf /etc"));
        assert_eq!(
            exp("git reset {--hard,HEAD}").as_deref(),
            Some("git reset --hard HEAD")
        );
        assert_eq!(
            exp("git push {--force,origin} main").as_deref(),
            Some("git push --force origin main")
        );
    }

    #[test]
    fn product_order_is_bash_order() {
        assert_eq!(exp("echo {a,b}{c,d}").as_deref(), Some("echo ac ad bc bd"));
        assert_eq!(exp("echo x{1,2{y,z}}").as_deref(), Some("echo x1 x2y x2z"));
    }

    #[test]
    fn quoted_assignment_and_literal_braces_stay() {
        assert_eq!(exp("echo \"{a,b}\""), None);
        assert_eq!(exp("echo \"x {a,b} y\""), None);
        assert_eq!(exp("X={a,b} true"), None);
        assert_eq!(exp("echo {} {x} {1..3}"), None);
        assert_eq!(exp("echo $(echo {a,b})"), None);
    }

    #[test]
    fn over_bound_is_too_many() {
        let w = "{a,b}".repeat(9);
        assert!(matches!(
            expand_segment(&format!("echo {w}")),
            SegmentBraces::TooMany
        ));
    }
}
