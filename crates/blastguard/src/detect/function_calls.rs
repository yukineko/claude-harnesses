//! The CALL-TIME reading of shell functions defined on the command line
//! (backlog bdce0261).
//!
//! bash does not expand a function body when the function is DEFINED; it
//! expands it each time the function is CALLED, with the variables in effect
//! at that moment. The rest of the analyser reads the line in text order, so
//! a body was judged — if at all — with the variables of its DEFINITION:
//!
//! ```text
//! L=<safe>; f() { echo x > $L; }; L=/etc/hosts; f    # writes /etc/hosts
//! f() { rm -f $L; }; L=/etc/hosts; f                 # deletes /etc/hosts
//! ```
//!
//! were both Allow, while the same write or delete written out in place is
//! refused. [`function_call_reading`] spells the line the way it runs: each
//! definition is replaced by the no-op `:` (defining a function runs nothing)
//! and each call is replaced by its body in a brace group, at the call site.
//! The caller judges that text IN ADDITION to the original, the most
//! restrictive verdict winning, so this reading can only add a verdict, never
//! remove one (the definition-time reading still stands).
//!
//! Modelled: `NAME() {`, `NAME () {`, `function NAME {`, `function NAME() {`,
//! and the `{` on the following line; bodies spanning one or many lines;
//! nested brace groups and nested definitions inside a body; a call as the
//! command word of a segment, after compound keywords (`if`, `then`, `!`, `{`,
//! …), assignment prefixes and opening `(`; calls inside a body (inlined with
//! the definitions in effect at the CALL, bounded by [`MAX_INLINE_DEPTH`]).
//!
//! Undetermined (the caller resolves it to Ask, never to clean): a CALL to a
//! function whose definition shape is not modelled (`f() ( … )`, a redirect on
//! the definition, a body that never closes), recursion deeper than
//! [`MAX_INLINE_DEPTH`], an inlined text larger than [`MAX_INLINED_LEN`], and
//! a segment layout this module cannot place back onto the text.
//!
//! NOT modelled (the call is not recognised, so this reading adds nothing for
//! it — the original reading still applies): a call inside `$(…)` / backticks,
//! through `eval` / `command` / `xargs` / `env`, and a function body read
//! from a here-document fed to a shell.

use std::collections::HashMap;

use super::compound::{split_first_word, strip_unmatched_close_parens, COMPOUND_PREFIX_WORDS};
use super::heredoc::here_document_layout;
use super::segment::{split_segments_with_separators, SeparatedSegment};
use super::shell_lex::ShellLexed;
use super::words::is_assignment;

/// How many calls deep one inlining may go (a call inside an inlined body
/// inside an inlined body …). A recursive function reaches it and is
/// Undetermined.
const MAX_INLINE_DEPTH: usize = 8;

/// The largest inlined text this reading builds; beyond it the reading is
/// Undetermined rather than truncated.
const MAX_INLINED_LEN: usize = 256 * 1024;

/// Reason of the Ask for an undetermined [`function_call_reading`].
pub(super) const FUNCTION_CALL_UNDETERMINED: &str =
    "a shell function defined on this line is called, and bash runs its body with the \
     variables in effect at the CALL — blastguard cannot determine what that body does at \
     the call (an unmodelled definition shape, recursion, or a layout it cannot read), so \
     it cannot vouch for it either way";

/// A function definition as read off the line.
#[derive(Clone)]
enum Def {
    /// A `{ … }` body: its raw text between the braces.
    Brace(String),
    /// A definition whose body or extent this module does not model.
    Unmodelled,
}

/// The line with every call of a function defined on it replaced by that
/// function's body, judged at the call site. See the module docs.
///
/// [`ShellLexed::Same`] when the line defines no function (or no rewrite
/// changes it); [`ShellLexed::Undetermined`] when a call cannot be inlined.
pub(super) fn function_call_reading(cmd: &str) -> ShellLexed {
    // Cheap gate: every modelled definition shape holds `(` or `function`.
    if !(cmd.contains('(') || cmd.contains("function")) {
        return ShellLexed::Same;
    }
    let mut defs: HashMap<String, Def> = HashMap::new();
    match inline_calls(cmd, &mut defs, 0) {
        Err(()) => ShellLexed::Undetermined,
        Ok((text, true)) if text != cmd => ShellLexed::Text(text),
        Ok(_) => ShellLexed::Same,
    }
}

/// Byte ranges of `segs` in `text`. Every segment text is a substring of
/// `text`, in order (the splitter copies characters, it never rewrites them);
/// `Err` when that does not hold, so the caller cannot rewrite a wrong range.
fn locate(text: &str, segs: &[SeparatedSegment]) -> Result<Vec<(usize, usize)>, ()> {
    let mut out = Vec::with_capacity(segs.len());
    let mut cursor = 0usize;
    for seg in segs {
        if seg.text.is_empty() {
            out.push((cursor, cursor));
            continue;
        }
        let pos = text.get(cursor..).ok_or(())?.find(&seg.text).ok_or(())?;
        let start = cursor + pos;
        let end = start + seg.text.len();
        out.push((start, end));
        cursor = end;
    }
    Ok(out)
}

/// A character bash accepts in a function name for the shapes modelled here.
/// `:` is deliberately absent (the fork bomb `:(){ … }` has its own rule).
fn name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.'
}

/// What a segment that OPENS a function definition says.
enum Head {
    /// `{` follows on the same segment; the body starts at this byte offset
    /// within the segment text.
    Brace { name: String, body_off: usize },
    /// The header ends the segment; the `{` must open the next segment.
    BraceNext { name: String },
    /// A header followed by a body shape this module does not model.
    Unmodelled { name: String },
}

/// Read a function-definition header at the start of `seg`, or `None` when the
/// segment does not open a definition.
fn def_head(seg: &str) -> Option<Head> {
    let lead = seg.len() - seg.trim_start().len();
    let t = &seg[lead..];
    let (name, after, keyword) = if let Some(rest) = t.strip_prefix("function") {
        if !rest.starts_with(|c: char| c.is_whitespace()) {
            return None;
        }
        let r = rest.trim_start();
        let n: String = r.chars().take_while(|c| name_char(*c)).collect();
        if n.is_empty() {
            return None;
        }
        let after = &r[n.len()..];
        (n, after, true)
    } else {
        let n: String = t.chars().take_while(|c| name_char(*c)).collect();
        if n.is_empty() {
            return None;
        }
        (n.clone(), &t[n.len()..], false)
    };
    // `()`, optional after `function NAME`, required otherwise.
    let after_ws = after.trim_start();
    let rest = if let Some(r) = after_ws.strip_prefix('(') {
        r.trim_start().strip_prefix(')')?
    } else if keyword && (after.is_empty() || after.starts_with(|c: char| c.is_whitespace())) {
        after
    } else {
        return None;
    };
    let rest_trim = rest.trim_start();
    if rest_trim.is_empty() {
        return Some(Head::BraceNext { name });
    }
    if let Some(r) = rest_trim.strip_prefix('{') {
        if r.is_empty() || r.starts_with(|c: char| c.is_whitespace()) {
            let body_off = seg.len() - r.len();
            return Some(Head::Brace { name, body_off });
        }
    }
    Some(Head::Unmodelled { name })
}

/// The `{` that opens a segment (after leading whitespace), as the byte offset
/// just past it, or `None`.
fn leading_open_brace(seg: &str) -> Option<usize> {
    let lead = seg.len() - seg.trim_start().len();
    let r = seg[lead..].strip_prefix('{')?;
    (r.is_empty() || r.starts_with(|c: char| c.is_whitespace())).then(|| seg.len() - r.len())
}

/// The `}` that opens a segment: `Some(rest after it)`.
fn leading_close_brace(seg: &str) -> Option<&str> {
    let r = seg.trim_start().strip_prefix('}')?;
    (r.is_empty() || r.starts_with(|c: char| c.is_whitespace() || c == ')')).then_some(r)
}

/// How many brace groups a body piece opens: leading `{` words (after compound
/// keywords) and nested `NAME() {` headers.
fn opens(piece: &str) -> usize {
    if let Some(Head::Brace { .. }) = def_head(piece) {
        return 1;
    }
    let mut n = 0;
    let mut rest = piece;
    while let Some((w, tail)) = split_first_word(rest) {
        if w == "{" {
            n += 1;
        } else if !COMPOUND_PREFIX_WORDS.contains(&w) || w == "}" {
            break;
        }
        rest = tail;
    }
    n
}

/// The command word of a segment, as `(byte offset within seg, word)`, after
/// compound keywords, assignment prefixes and opening `(`.
fn call_word(seg: &str) -> Option<(usize, &str)> {
    let mut off = 0usize;
    loop {
        let rest = &seg[off..];
        let lead = rest.len() - rest.trim_start().len();
        off += lead;
        let rest = &seg[off..];
        if let Some(r) = rest.strip_prefix('(') {
            if !r.starts_with('(') {
                off += 1;
                continue;
            }
        }
        let (w, _) = split_first_word(rest)?;
        if (COMPOUND_PREFIX_WORDS.contains(&w) && w != "}") || is_assignment(w) {
            off += w.len();
            continue;
        }
        let w = w.trim_end_matches(')');
        return (!w.is_empty()).then_some((off, w));
    }
}

/// Rewrite `text`: definitions become `:`, calls become their body at the
/// call site. Returns the text and whether anything changed.
fn inline_calls(
    text: &str,
    defs: &mut HashMap<String, Def>,
    depth: usize,
) -> Result<(String, bool), ()> {
    if depth > MAX_INLINE_DEPTH {
        return Err(());
    }
    let segs = split_segments_with_separators(text);
    let offs = locate(text, &segs)?;
    let (is_body, _) = here_document_layout(&segs);
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0usize;
    let mut changed = false;
    let mut i = 0usize;
    while i < segs.len() {
        if is_body.get(i).copied().unwrap_or(false) {
            i += 1;
            continue;
        }
        let (start, end) = offs[i];
        if let Some(head) = def_head(&segs[i].text) {
            let (name, def, last) = read_definition(text, &segs, &offs, &is_body, i, head);
            out.push_str(&text[cursor..start]);
            out.push(':');
            cursor = offs[last].1;
            defs.insert(name, def);
            changed = true;
            i = last + 1;
            continue;
        }
        if let Some((woff, word)) = call_word(&segs[i].text) {
            if let Some(def) = defs.get(word).cloned() {
                let Def::Brace(body) = def else {
                    return Err(());
                };
                let (inner, _) = inline_calls(&body, defs, depth + 1)?;
                let ws = start + woff;
                let (_, closes, _) = strip_unmatched_close_parens(&text[ws..end]);
                let mut b = inner.trim();
                if let Some(s) = b.strip_suffix(';') {
                    if !s.ends_with(';') {
                        b = s.trim_end();
                    }
                }
                let b = if b.is_empty() { ":" } else { b };
                out.push_str(&text[cursor..ws]);
                out.push_str("{ ");
                out.push_str(b);
                out.push_str("\n}");
                out.push_str(&")".repeat(closes));
                cursor = end;
                changed = true;
            }
        }
        if out.len() > MAX_INLINED_LEN {
            return Err(());
        }
        i += 1;
    }
    out.push_str(&text[cursor..]);
    if out.len() > MAX_INLINED_LEN {
        return Err(());
    }
    Ok((out, changed))
}

/// Read the definition opened at segment `i`: its name, its body (or
/// [`Def::Unmodelled`]), and the index of its last segment.
fn read_definition(
    text: &str,
    segs: &[SeparatedSegment],
    offs: &[(usize, usize)],
    is_body: &[bool],
    i: usize,
    head: Head,
) -> (String, Def, usize) {
    let (name, body_start, first_piece_idx) = match head {
        Head::Unmodelled { name } => return (name, Def::Unmodelled, i),
        Head::Brace { name, body_off } => {
            let start = offs[i].0 + body_off;
            (name, start, i)
        }
        Head::BraceNext { name } => {
            let Some(next) = segs.get(i + 1) else {
                return (name, Def::Unmodelled, i);
            };
            match leading_open_brace(&next.text) {
                Some(off) => (name, offs[i + 1].0 + off, i + 1),
                None => return (name, Def::Unmodelled, i),
            }
        }
    };
    let mut depth = 1usize;
    // The first piece is the text after the opening `{` in its segment.
    let first_piece = &text[body_start..offs[first_piece_idx].1];
    depth += opens(first_piece);
    for k in first_piece_idx + 1..segs.len() {
        if is_body.get(k).copied().unwrap_or(false) {
            continue;
        }
        let piece = &segs[k].text;
        if let Some(after) = leading_close_brace(piece) {
            depth -= 1;
            if depth == 0 {
                let after = after.trim();
                if !(after.is_empty() || after.starts_with('#')) {
                    // A redirect (or anything else) on the definition itself.
                    return (name, Def::Unmodelled, k);
                }
                let body = text[body_start..offs[k].0].to_string();
                return (name, Def::Brace(body), k);
            }
            continue;
        }
        depth += opens(piece);
    }
    // The body never closes: bash rejects the line, but the extent is unknown.
    (name, Def::Unmodelled, segs.len().saturating_sub(1))
}
