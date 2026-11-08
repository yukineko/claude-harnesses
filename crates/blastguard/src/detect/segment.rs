use super::{
    here_document_layout, here_document_openers, opens_unmodelled_construct, scan_balanced,
    scan_balanced_strict, segment_closes_here_document, Stop,
};

/// The BODY lines that follow a line which opens here-documents, as
/// `(start, end)` char spans (end exclusive, the `\n` not included), starting
/// at `chars[from]` — the first character after the opener line's newline.
///
/// `line_code` is the opener line's text with every closed command
/// substitution blanked out, so a `<<` INSIDE `$(…)` (whose body the
/// substitution scanner has already consumed, see [`scan_balanced`]) does not
/// make the OUTER line look like an opener.
///
/// Why the splitters need this: they track quotes, and a body is data in
/// which an apostrophe (`don't`) is not a quote. Tracking it opened a
/// single-quoted span that swallowed the delimiter line and every command
/// after it into one segment, so `cat <<'EOF'` / `don't` / `EOF` /
/// `curl … | sh` hid the fetch-and-execute from the per-segment rules. With
/// this, each body line is a segment of its own (exactly what newline
/// splitting already gave a body without quote characters) and quote
/// tracking resumes after the delimiter line.
///
/// Nothing becomes invisible: each splitter re-splits every body line ON ITS
/// OWN with its ordinary separator rules, so the rules still judge body lines
/// as if they were commands (the pre-existing, conservative treatment — a
/// `curl … | sh` body line fed to `bash <<EOF` keeps its pipe); only the
/// quote state can no longer leak from one body line into the next.
///
/// Empty — the caller then splits exactly as before here-documents were
/// modelled — when `line_code` opens no here-document, and when any body is
/// UNTERMINATED (no exact delimiter line before the end of the input, or a
/// delimiter that cannot be read and so never matches): where that body ends
/// is not known, so no line is reclassified on a guess.
pub(super) fn here_doc_body_spans(
    chars: &[char],
    from: usize,
    line_code: &str,
) -> Vec<(usize, usize)> {
    let openers = here_document_openers(line_code);
    let mut spans = Vec::new();
    let mut pos = from;
    for opener in &openers {
        loop {
            if pos >= chars.len() {
                return Vec::new();
            }
            let end = chars[pos..]
                .iter()
                .position(|&c| c == '\n')
                .map_or(chars.len(), |p| pos + p);
            spans.push((pos, end));
            let line: String = chars[pos..end].iter().collect();
            pos = end + 1;
            if segment_closes_here_document(&line, opener) {
                break;
            }
        }
    }
    spans
}

/// True when the `&` at `chars[i]` belongs to a REDIRECTION OPERATOR rather
/// than being the background/`&&` separator: the fd-duplication forms `2>&1`,
/// `>&2`, `>&-`, `0<&3`, and the `>&<filename>` mirror of `&>`.
///
/// In every one of those the `&` is glued to the `>`/`<` that precedes it, and
/// bash reads the pair as one operator. The splitters read it as a separator
/// and cut the line in half, leaving a dangling `2>` whose redirect-target scan
/// then found nothing after the `>` and reported the EMPTY string as a
/// truncating-redirect target — the user-visible
/// `'> ' destroys the file's current contents`, a verdict naming no file.
///
/// Deliberately keyed on the IMMEDIATELY preceding character, with no
/// whitespace skipping: a genuine background operator always has whitespace or
/// a word character before it (`cargo test > log &`), and bash itself requires
/// `>&` to be unseparated to mean a dup — `2> &1` redirects into a file called
/// `&1`. Skipping whitespace here would therefore turn real `&` separators
/// invisible, which is the fail-open direction.
///
/// Used by all three splitters so a segment INDEX taken from one stays valid in
/// the others (see [`split_segments_with_separators`]).
pub(super) fn ampersand_is_redirect_operator(chars: &[char], i: usize) -> bool {
    i > 0 && matches!(chars[i - 1], '>' | '<')
}

/// When `chars[i]` OPENS a command substitution — a backtick or `$(` that is
/// not backslash-escaped — and that substitution is closed later in `chars`,
/// the index of its closing `` ` ``/`)`. `None` otherwise.
///
/// Used by [`split_segments`] and [`split_segments_with_separators`] (outside
/// single quotes only; the caller checks that) to keep a substitution's body
/// in ONE segment. A `;`/`&&`/`||`/`|`/`&`/newline inside the body separates
/// commands of the BODY, not of the outer line, and bash parses it that way.
/// Cutting there left two fragments, neither of them a real command: the
/// tail of `` B=`ls | sort` `` became the segment ``sort` ``, whose command
/// word carries the closing backtick and was asked about as an "expansion",
/// and the tail of `echo $(curl … | sh)` became `sh)`, which is not read as
/// `sh`, so that fetch-and-execute reached Allow. Kept whole, the segment's
/// body is judged as the complete command line it is by
/// [`executed_substitution_payloads`].
///
/// An UNCLOSED substitution yields `None`, and the caller then splits exactly
/// as before this existed, so every fragment of a line this cannot parse is
/// still judged on its own; nothing becomes invisible.
///
/// Uses the EXTRACTION scan [`scan_balanced`] (here-document-aware, falling
/// back to the quote/paren-only scan), because keeping the text together is
/// what lets it be judged; see [`closed_substitution_end_strict`] for the
/// variant that must not guess.
pub(super) fn closed_substitution_end(chars: &[char], i: usize) -> Option<usize> {
    substitution_end_with(chars, i, scan_balanced)
}

/// [`closed_substitution_end`] through [`scan_balanced_strict`]: `Some` only
/// when the substitution provably closes there, with every here-document body
/// inside it terminated and plain data. For a caller that ALLOWS on the
/// strength of the substitution being one closed word (the whole-tree-staging
/// argv reader, `git_stage`); `None` is its Ask side.
pub(super) fn closed_substitution_end_strict(chars: &[char], i: usize) -> Option<usize> {
    substitution_end_with(chars, i, scan_balanced_strict)
}

/// When `chars[i]` OPENS an unquoted process substitution — a `<(` or `>(`
/// whose `<`/`>` is not backslash-escaped — that is closed later in `chars`,
/// the index of its closing `)`. `None` otherwise. The caller checks that
/// `chars[i]` is outside single AND double quotes (inside `"…"` the `<(` is
/// literal text, and treating it as a span there would swallow the quote's
/// end and the separators after it).
///
/// Used by [`split_segments`] and [`split_segments_with_separators`] exactly
/// like [`closed_substitution_end`], and for the same reason: a `;`/newline
/// inside the body separates the BODY's commands. Cutting there left the
/// fragment `cat <(rm -rf /etc`, whose process substitution no longer closes
/// inside the segment, so [`executed_substitution_payloads`] stopped at it and
/// the body was judged nowhere (`cat <(rm -rf /etc;)` and the bash 3.2 shape
/// `echo $(cat <<'EOF'` / `x)<(rm -rf /etc` / `EOF` / `)` were Allow).
/// Uses the extraction scan [`scan_balanced`] (bash 3.2's end under
/// [`super::substitution::under_bash32_reading`]).
pub(super) fn closed_process_substitution_end(chars: &[char], i: usize) -> Option<usize> {
    let escaped = chars[..i].iter().rev().take_while(|&&c| c == '\\').count() % 2 == 1;
    if escaped || !matches!(chars[i], '<' | '>') || chars.get(i + 1) != Some(&'(') {
        return None;
    }
    scan_balanced(chars, i + 2, Stop::Paren, 0)
}

fn substitution_end_with(
    chars: &[char],
    i: usize,
    scan: fn(&[char], usize, Stop, usize) -> Option<usize>,
) -> Option<usize> {
    let escaped = chars[..i].iter().rev().take_while(|&&c| c == '\\').count() % 2 == 1;
    if escaped {
        return None;
    }
    match chars[i] {
        '`' => scan(chars, i + 1, Stop::Backtick, 0),
        '$' if chars.get(i + 1) == Some(&'(') => scan(chars, i + 2, Stop::Paren, 0),
        _ => None,
    }
}

/// Quote-aware split of a command line into individual simple-command segments
/// on `;`, newline, `&&`, `||`, `|`, `&`.
///
/// An `&` that is part of a redirection operator (`2>&1`, `>&2`) does NOT end a
/// segment — see [`ampersand_is_redirect_operator`].
pub(super) fn split_segments(cmd: &str) -> Vec<String> {
    // Iterate over `char`s, not raw bytes: casting a UTF-8 continuation byte
    // `as char` yields a bogus Latin-1 scalar (e.g. 0xA0 → U+00A0), which both
    // corrupts non-ASCII segments and can misfire operator/quote checks. All the
    // operators we split on are ASCII, so char iteration preserves ASCII behavior
    // exactly while keeping multi-byte text intact.
    let chars: Vec<char> = cmd.chars().collect();
    let mut segs = Vec::new();
    let mut cur = String::new();
    // The current line's code with closed substitutions blanked; read at each
    // newline for here-document openers (see `here_doc_body_spans`).
    let mut line_code = String::new();
    let (mut in_s, mut in_d) = (false, false);
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\'' && !in_d {
            in_s = !in_s;
            cur.push(c);
            line_code.push(c);
            i += 1;
            continue;
        }
        if c == '"' && !in_s {
            in_d = !in_d;
            cur.push(c);
            line_code.push(c);
            i += 1;
            continue;
        }
        if !in_s {
            let end = closed_substitution_end(&chars, i)
                .or_else(|| (!in_d).then(|| closed_process_substitution_end(&chars, i))?);
            if let Some(end) = end {
                cur.extend(&chars[i..=end]);
                line_code.push('_');
                i = end + 1;
                continue;
            }
        }
        if !in_s && !in_d && !(c == '&' && ampersand_is_redirect_operator(&chars, i)) {
            // Two-char operators.
            if (c == '&' && chars.get(i + 1) == Some(&'&'))
                || (c == '|' && chars.get(i + 1) == Some(&'|'))
            {
                segs.push(std::mem::take(&mut cur));
                line_code.push_str("&&");
                i += 2;
                continue;
            }
            if c == '\n' {
                segs.push(std::mem::take(&mut cur));
                i += 1;
                for (s, e) in here_doc_body_spans(&chars, i, &std::mem::take(&mut line_code)) {
                    // Each body line is split on its own (no newline inside,
                    // so no further body reading), as code.
                    let line: String = chars[s..e].iter().collect();
                    let mut parts = split_segments(&line);
                    if e >= chars.len() {
                        if let Some(last) = parts.pop() {
                            cur = last;
                        }
                    }
                    segs.extend(parts);
                    i = e + 1;
                }
                continue;
            }
            if c == ';' || c == '|' || c == '&' {
                segs.push(std::mem::take(&mut cur));
                line_code.push(c);
                i += 1;
                continue;
            }
        }
        cur.push(c);
        line_code.push(c);
        i += 1;
    }
    segs.push(cur);
    segs
}

/// [`split_segments`]'s twin used by [`scan_body_for_fetch_or_decode`], with
/// an ADDITIONAL `(...)`-depth guard the plain version does not track: a
/// `;`/`&&`/`||`/`|`/`&` found INSIDE an unquoted `(...)` span does not end a
/// segment there.
///
/// Needed because [`scan_body_for_fetch_or_decode`] can be called on a body
/// whose ENTIRE text is a single, unquoted `$(...)` / process-substitution
/// span with a `|` inside it and nothing else surrounding it to protect that
/// `|` via quote-tracking (a here-string operand fed straight from
/// [`analyze_here_string_egress`], e.g. `$(curl … | base64 -d)` with the
/// enclosing quote the operand was extracted from already stripped off).
/// [`split_segments`] tracks only quotes, so it would split that text at the
/// INNER `|`, right in the middle of the `$(...)`, into two fragments neither
/// of which reassembles into a fetch/decode command word or a matched
/// `$(...)` payload — a verified bypass in this file's own recursion, not a
/// bypass of the walk itself: `stage_is_fetch_or_decode`/
/// `command_substitution_payloads` both need the segment to contain the
/// WHOLE `$(...)`/`<(...)` construct, and a mid-construct split hides it from
/// both. Every EXISTING caller of `scan_body_for_fetch_or_decode` — bodies
/// extracted from `<(...)` — is unaffected: those bodies came from
/// [`process_substitution_payloads`]/[`command_substitution_payloads`],
/// which only ever hand over text that is either paren-free or has its `(` /
/// `)` correctly balanced within a segment already, so paren-depth tracking
/// is a no-op there and changes nothing for any pre-existing passing case.
pub(super) fn split_segments_paren_aware(cmd: &str) -> Vec<String> {
    let chars: Vec<char> = cmd.chars().collect();
    let mut segs = Vec::new();
    let mut cur = String::new();
    let (mut in_s, mut in_d) = (false, false);
    let mut paren_depth: u32 = 0;
    // See `split_segments`: here-document bodies are read at a depth-0
    // newline. Inside an unquoted `(...)` the newline is not a separator and
    // the body is not modelled (the pre-existing scan).
    let mut line_code = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\'' && !in_d {
            in_s = !in_s;
            cur.push(c);
            line_code.push(c);
            i += 1;
            continue;
        }
        if c == '"' && !in_s {
            in_d = !in_d;
            cur.push(c);
            line_code.push(c);
            i += 1;
            continue;
        }
        // Backtick bodies too (paren depth does not see them); see
        // `closed_substitution_end`.
        if !in_s {
            if let Some(end) = closed_substitution_end(&chars, i) {
                cur.extend(&chars[i..=end]);
                line_code.push('_');
                i = end + 1;
                continue;
            }
        }
        if !in_s && !in_d {
            if c == '(' {
                paren_depth += 1;
                cur.push(c);
                line_code.push(c);
                i += 1;
                continue;
            }
            if c == ')' {
                paren_depth = paren_depth.saturating_sub(1);
                cur.push(c);
                line_code.push(c);
                i += 1;
                continue;
            }
            if paren_depth == 0 && !(c == '&' && ampersand_is_redirect_operator(&chars, i)) {
                if (c == '&' && chars.get(i + 1) == Some(&'&'))
                    || (c == '|' && chars.get(i + 1) == Some(&'|'))
                {
                    segs.push(std::mem::take(&mut cur));
                    line_code.push_str("&&");
                    i += 2;
                    continue;
                }
                if c == '\n' {
                    segs.push(std::mem::take(&mut cur));
                    i += 1;
                    for (s, e) in here_doc_body_spans(&chars, i, &std::mem::take(&mut line_code)) {
                        let line: String = chars[s..e].iter().collect();
                        let mut parts = split_segments_paren_aware(&line);
                        if e >= chars.len() {
                            if let Some(last) = parts.pop() {
                                cur = last;
                            }
                        }
                        segs.extend(parts);
                        i = e + 1;
                    }
                    continue;
                }
                if c == ';' || c == '|' || c == '&' {
                    segs.push(std::mem::take(&mut cur));
                    line_code.push(c);
                    i += 1;
                    continue;
                }
            }
        }
        cur.push(c);
        line_code.push(c);
        i += 1;
    }
    segs.push(cur);
    segs
}

/// The separator that FOLLOWS a segment. [`split_segments`] discards it, and
/// for variable scope that loss is fatal: the separators do not all behave the
/// same way, so "an assignment appears earlier on the line" is not the same
/// question as "an assignment reaches this use".
///
/// Each variant below was verified against `bash -c` rather than assumed; the
/// observed output is quoted on the variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SegmentSep {
    /// `;` or a newline: what follows runs in the SAME shell and runs
    /// UNCONDITIONALLY.
    /// `bash -c 'RM=/bin/echo; $RM reached'` prints `reached`, and the newline
    /// spelling does too.
    Sequential,
    /// `&&` or `||`: what follows also runs in the same shell — `bash -c 'true
    /// && RM=/bin/echo; $RM reached'` prints `reached`, and so does `false ||
    /// RM=/bin/echo; $RM reached` — but WHETHER it runs is decided by the exit
    /// status of what precedes it, which is not in the text.
    /// `bash -c 'false && RM=/bin/echo; echo "word=[$RM]"'` prints `word=[]`:
    /// the same characters as the passing case, the same shell, and no
    /// assignment.
    ///
    /// A separate variant, not a note on [`SegmentSep::Sequential`], because
    /// the two answer different questions and the answers differ. For SCOPE
    /// they are alike ([`assignments_reach_parent_shell`] accepts both); for
    /// EXECUTION they are not ([`assignment_execution_is_unconditional`]
    /// rejects this one). Folding them together is what let a guarded
    /// assignment be reported as a literal program name.
    Conditional,
    /// `|`: every stage of a pipeline runs in its own subshell, so an
    /// assignment in a stage does not survive into the parent shell.
    /// `bash -c 'RM=/bin/echo | echo "word=[$RM]"'` prints `word=[]`.
    Pipe,
    /// `&`: the segment before it is backgrounded into its own subshell, so its
    /// assignment does not survive either.
    /// `bash -c 'RM=/bin/echo & echo "word=[$RM]"'` prints `word=[]`.
    Background,
    /// Nothing follows: this is the last segment of the line.
    EndOfLine,
}

/// One [`split_segments`] segment plus the separator that followed it.
pub(super) struct SeparatedSegment {
    pub(super) text: String,
    pub(super) sep_after: SegmentSep,
    /// The separator was a NEWLINE, not a `;`. [`SegmentSep::Sequential`]
    /// merges the two because they mean the same thing for scope and
    /// execution. They differ for a here-document: only a newline ends the
    /// opener's line, so only then is the next segment the first body line
    /// rather than more code (`cat <<'EOF'; rm x`).
    pub(super) ends_at_newline: bool,
}

/// [`split_segments`] with the separator KEPT.
///
/// Deliberately a second function rather than a change to [`split_segments`]:
/// that function's signature and output are depended on by many decision arms,
/// and none of them wants the separator. The scanning loop here is the same one
/// (same quote tracking, same two-char operators, same char-not-byte
/// iteration), so the `text` fields are equal, element for element, to
/// `split_segments`' output — which is what makes a segment INDEX taken from
/// one usable in the other. `separated_segmentation_agrees_with_split_segments`
/// pins that correspondence.
pub(super) fn split_segments_with_separators(cmd: &str) -> Vec<SeparatedSegment> {
    let chars: Vec<char> = cmd.chars().collect();
    let mut segs: Vec<SeparatedSegment> = Vec::new();
    let mut cur = String::new();
    // Same here-document body reading as `split_segments`.
    let mut line_code = String::new();
    let (mut in_s, mut in_d) = (false, false);
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\'' && !in_d {
            in_s = !in_s;
            cur.push(c);
            line_code.push(c);
            i += 1;
            continue;
        }
        if c == '"' && !in_s {
            in_d = !in_d;
            cur.push(c);
            line_code.push(c);
            i += 1;
            continue;
        }
        // Same substitution span as `split_segments`, so the two stay
        // element-for-element equal.
        if !in_s {
            let end = closed_substitution_end(&chars, i)
                .or_else(|| (!in_d).then(|| closed_process_substitution_end(&chars, i))?);
            if let Some(end) = end {
                cur.extend(&chars[i..=end]);
                line_code.push('_');
                i = end + 1;
                continue;
            }
        }
        if !in_s && !in_d && !(c == '&' && ampersand_is_redirect_operator(&chars, i)) {
            let two_char_conditional = (c == '&' && chars.get(i + 1) == Some(&'&'))
                || (c == '|' && chars.get(i + 1) == Some(&'|'));
            if two_char_conditional {
                segs.push(SeparatedSegment {
                    text: std::mem::take(&mut cur),
                    sep_after: SegmentSep::Conditional,
                    ends_at_newline: false,
                });
                line_code.push_str("&&");
                i += 2;
                continue;
            }
            if c == '\n' {
                segs.push(SeparatedSegment {
                    text: std::mem::take(&mut cur),
                    sep_after: SegmentSep::Sequential,
                    ends_at_newline: true,
                });
                i += 1;
                for (s, e) in here_doc_body_spans(&chars, i, &std::mem::take(&mut line_code)) {
                    let line: String = chars[s..e].iter().collect();
                    let mut parts = split_segments_with_separators(&line);
                    if e >= chars.len() {
                        if let Some(last) = parts.pop() {
                            cur = last.text;
                        }
                    } else if let Some(last) = parts.last_mut() {
                        // The line's own end is the newline that follows it.
                        last.sep_after = SegmentSep::Sequential;
                        last.ends_at_newline = true;
                    }
                    segs.extend(parts);
                    i = e + 1;
                }
                continue;
            }
            let one_char = match c {
                ';' => Some(SegmentSep::Sequential),
                '|' => Some(SegmentSep::Pipe),
                '&' => Some(SegmentSep::Background),
                _ => None,
            };
            if let Some(sep) = one_char {
                segs.push(SeparatedSegment {
                    text: std::mem::take(&mut cur),
                    sep_after: sep,
                    ends_at_newline: false,
                });
                line_code.push(c);
                i += 1;
                continue;
            }
        }
        cur.push(c);
        line_code.push(c);
        i += 1;
    }
    segs.push(SeparatedSegment {
        text: cur,
        sep_after: SegmentSep::EndOfLine,
        ends_at_newline: false,
    });
    segs
}

/// True only when segment `idx` is executed by the parent shell itself, so an
/// assignment standing alone in it is still set for every later segment.
///
/// Both subshell shapes are excluded, and both were measured (see
/// [`SegmentSep`]): a segment that is a pipeline stage — whether because a `|`
/// follows it or because a `|` precedes it — and a segment that a trailing `&`
/// backgrounds.
///
/// This is a question about SCOPE only — "if this assignment runs, does its
/// value outlive its segment" — so [`SegmentSep::Conditional`] counts as
/// reaching, exactly like [`SegmentSep::Sequential`]: `bash -c 'true &&
/// RM=/bin/echo; $RM reached'` prints `reached`. Whether the assignment runs at
/// all is the separate question [`assignment_execution_is_unconditional`]
/// answers, and a caller that wants a literal must pass BOTH.
///
/// The false side is the restrictive one here: "not the parent shell" makes the
/// caller ignore the segment's assignments, which can only preserve an Ask. An
/// out-of-range index therefore answers false rather than panicking.
pub(super) fn assignments_reach_parent_shell(segs: &[SeparatedSegment], idx: usize) -> bool {
    let Some(seg) = segs.get(idx) else {
        return false;
    };
    if matches!(seg.sep_after, SegmentSep::Pipe | SegmentSep::Background) {
        return false;
    }
    if idx > 0 && matches!(segs[idx - 1].sep_after, SegmentSep::Pipe) {
        return false;
    }
    true
}

/// True only when the TEXT alone guarantees that segment `idx` runs at all.
///
/// `&&` and `||` make what follows them depend on the exit status of what
/// precedes them, and an exit status is a RUN-TIME fact that reading the line
/// cannot recover. For an assignment that means the line describes two
/// different programs — the assigned one if the guard let it run, whatever the
/// environment already held if it did not — so a resolver that ignored this
/// would put its single permissive answer on an unobservable.
///
/// WHICH POSITION IS TESTED MATTERS, and it is the assignment's, not the use's.
/// In the corpus's dominant shape
///
/// ```text
/// BIN=/path/to/tool
/// cd "$SB" && "$BIN" guard main-tree --json
/// ```
///
/// the `&&` stands between the assignment and the use. It decides whether the
/// COMMAND runs, which the caller is free not to care about: if that command
/// runs at all, its program is the one `BIN` was unconditionally given. Testing
/// the use's separator instead would refuse this shape — 70 of the 115 measured
/// interventions, i.e. every one this function was written to resolve.
///
/// DELIBERATELY COARSE, stated because it costs resolutions a sharper rule
/// would keep: this asks whether a conditional separator appears ANYWHERE
/// before `idx`, not whether the nearest and-or list still encloses it. In
/// `a && b; BIN=x` the `;` ends the and-or list, so the shell really does run
/// `BIN=x` on both branches, and this still answers false. Tracking where each
/// and-or list ends would need real grammar, not a flat separator scan, and the
/// error direction of guessing wrong there is permissive. So the coarse rule
/// stands: it forgoes a resolution, and never invents one.
///
/// `&&`/`||` are not the only way execution stops being guaranteed, so the scan
/// also refuses anything standing after a segment that OPENS a compound command
/// or a subshell ([`opens_unmodelled_construct`]). A NEWLINE-formatted compound
/// command (`if foo`, newline, `then`, newline, `BIN=x`, newline, `fi`) hands
/// [`split_segments`] a body segment that is a pure assignment list with an
/// ordinary separator on both sides and nothing at all marking it as guarded —
/// it looks exactly like a top-level `BIN=x`. (The `;`-formatted spellings were
/// already excluded upstream by the pure-assignment-list rule in
/// [`resolve_expanded_command_word`]: `then BIN=x` is not an assignment list.
/// Only the newline spellings need this scan.)
///
/// That check is coarse in the same direction as the separator one: it never
/// looks for the `fi`/`done`/`)` that closes the construct, so an assignment
/// AFTER a completed `if … fi` is refused too. Tracking where a construct ends
/// needs real grammar, and the error direction of guessing wrong there is
/// permissive.
///
/// A HERE-DOCUMENT opened earlier on the line disqualifies later segments for a
/// third reason, and a worse one than the other two: the text after
/// `cat <<EOF` is not guarded code, it is not code at all. [`split_segments`]
/// splits the body on its newlines like any other segment, so a `NAME=value`
/// TYPED INTO A BODY reads as a pure assignment list and is indistinguishable
/// from a real one. Measured: `BIN=A` / `cat <<EOF` / `BIN=B` / `EOF` /
/// `$BIN marker` prints the body line verbatim and then runs `A` — the body
/// assigned nothing. Reporting `B` there is not a stale literal but an invented
/// one, so the scan refuses anything standing after a here-document opener
/// ([`opens_here_document`]), with the same coarseness as above: it does not
/// look for the closing delimiter.
pub(super) fn assignment_execution_is_unconditional(segs: &[SeparatedSegment], idx: usize) -> bool {
    // `take` rather than a slice: an out-of-range `idx` then scans the whole
    // line instead of panicking, which is also the restrictive direction.
    // An OPEN here-document still hides whether the segments after it are code
    // (see `here_document_layout`). A CLOSED one does not: its body has been
    // identified and skipped, and the line past the delimiter is ordinary code.
    let (_, unclosed_here_doc) = here_document_layout(segs);
    if unclosed_here_doc.map(|o| o < idx).unwrap_or(false) {
        return false;
    }
    !segs.iter().take(idx).any(|s| {
        matches!(s.sep_after, SegmentSep::Conditional) || opens_unmodelled_construct(&s.text)
    })
}
