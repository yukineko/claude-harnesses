use super::{
    analyze_shell_payload, depth_exhausted, executed_substitution_payloads, is_shell_identifier,
    quote_aware_words, split_segments_with_separators, unquote_literal_value, Ctx, SegmentSep,
    VerdictAcc, MAX_SHELL_DEPTH,
};
use crate::model::Decision;

/// Reserved words that can stand in COMMAND POSITION in front of the simple
/// command a segment really runs, or that close a compound command with no
/// command of their own (1875f626).
///
/// [`split_segments`] cuts `for f in x; do patch .githooks/pre-commit y; done`
/// into `for f in x` / ` do patch .githooks/pre-commit y` / ` done`, and every
/// rule used to read the FIRST word of each piece as the program. So `do`,
/// `then`, `{`, `!` … were judged as unknown verbs: `patch` was never examined
/// by its own rule (a Deny behind `then rm -rf /usr` was softened to the
/// unknown-wrapper Ask), a `cd` behind `do` moved no tracked directory, and any
/// protected path anywhere in the segment raised
/// `unknown-verb-protected-path` against the keyword itself. Removing the
/// keyword before judging is what makes the real command the one judged.
///
/// Only words bash treats as reserved IN COMMAND POSITION are listed, and they
/// are only removed from the front of a segment, where they are keywords:
/// `echo done` and `grep -q then` keep their words.
pub(super) const COMPOUND_PREFIX_WORDS: &[&str] = &[
    "if", "then", "elif", "else", "fi", "while", "until", "do", "done", "esac", "{", "}", "!",
];

/// The first shell word of `s` and the text after it, splitting on whitespace
/// that is OUTSIDE quotes and not backslash-escaped. `None` for blank text.
pub(super) fn split_first_word(s: &str) -> Option<(&str, &str)> {
    let t = s.trim_start();
    if t.is_empty() {
        return None;
    }
    let (mut in_s, mut in_d, mut escaped) = (false, false, false);
    for (i, c) in t.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match c {
            '\\' if !in_s => escaped = true,
            '\'' if !in_d => in_s = !in_s,
            '"' if !in_s => in_d = !in_d,
            c if c.is_whitespace() && !in_s && !in_d => return Some((&t[..i], &t[i..])),
            _ => {}
        }
    }
    Some((t, ""))
}

/// Byte offset just past the `)` that closes a `case` PATTERN at the start of
/// `s` (`a)`, `(a)`, `*.rs)`, `"x y")`), or `None` when `s` does not start with
/// a complete pattern. Only called where the grammar says a pattern stands.
pub(super) fn case_pattern_end(s: &str) -> Option<usize> {
    let lead = s.len() - s.trim_start().len();
    let t = &s[lead..];
    let mut chars = t.char_indices().peekable();
    // The optional `(` that may open a pattern.
    if t.starts_with('(') {
        chars.next();
    }
    let (mut in_s, mut in_d, mut escaped) = (false, false, false);
    let mut depth: u32 = 0;
    let mut prev = None;
    for (i, c) in chars {
        if escaped {
            escaped = false;
            prev = Some(c);
            continue;
        }
        match c {
            '\\' if !in_s => escaped = true,
            '\'' if !in_d => in_s = !in_s,
            '"' if !in_s => in_d = !in_d,
            // `$(…)` inside a pattern: its `)` is not the pattern's.
            '(' if !in_s && !in_d && prev == Some('$') => depth += 1,
            ')' if !in_s && !in_d => {
                if depth == 0 {
                    return Some(lead + i + 1);
                }
                depth -= 1;
            }
            _ => {}
        }
        prev = Some(c);
    }
    None
}

/// A `for NAME in WORDS` / `select NAME in WORDS` header: the loop variable and
/// every value the text says it takes.
#[derive(Clone, Debug)]
pub(super) struct LoopBinding {
    name: String,
    values: Vec<String>,
}

/// The binding a `for`/`select` header establishes, or `None` for a header
/// with no variable (`for ((…))`) or one this cannot read.
///
/// `for NAME` with no `in` iterates the positional parameters, which is a
/// value the line does not state; it is bound to the expansion `"$@"` so a
/// body that uses it is judged exactly like one that wrote `"$@"` itself.
pub(super) fn loop_binding(header: &str) -> Option<LoopBinding> {
    let words = quote_aware_words(header);
    let name = words.get(1)?;
    if !is_shell_identifier(name) {
        return None;
    }
    let values = match words.get(2).map(String::as_str) {
        Some("in") => words[3..]
            .iter()
            .map(|w| unquote_literal_value(w).unwrap_or_else(|| w.clone()))
            .collect(),
        None => vec!["\"$@\"".to_string()],
        Some(_) => return None,
    };
    Some(LoopBinding {
        name: name.clone(),
        values,
    })
}

/// One [`split_segments`] segment with its compound-command syntax separated
/// from the simple command it runs.
#[derive(Debug, Default)]
pub(super) struct SegmentView {
    /// Text the shell EXPANDS but does not run as a command: a `for`/`select`
    /// word list, a `case` subject, a `case` pattern. Nothing in it is a
    /// program or an operand — only the substitutions inside it execute, so
    /// that is all that is judged of it ([`analyze_expanded_data`]).
    pub(super) data: String,
    /// The simple command the segment runs, with leading reserved words and
    /// any `case` pattern removed. Empty when the segment runs none.
    pub(super) command: String,
    /// Set when the segment is a `for`/`select` header.
    pub(super) binding: Option<LoopBinding>,
    /// `(`-groups this segment opens and leaves open: the leading `(`s removed
    /// from `command`, plus any `(`/`$(`/`<(` still unmatched at its end.
    pub(super) opens: usize,
    /// Unmatched `)`s in the segment: groups that close here.
    pub(super) closes: usize,
}

/// [`SegmentView`] for every segment of `cmd`, index-aligned with
/// [`split_segments`] (built from [`split_segments_with_separators`], whose
/// `text` fields are pinned equal to it).
///
/// `case` patterns need state: a word ending in `)` is a pattern only where the
/// grammar puts one — right after `case WORD in`, or after a `;;`/`;&`/`;;&`
/// arm terminator while a `case` is open. Everywhere else it is left alone,
/// because a subshell's closing `)` (`(true && reboot)` splits into
/// ` reboot)`) has the same shape and removing it would hide the command.
/// A data here-document body never reaches this function: [`detect_bash`]
/// removes it first ([`strip_data_here_document_bodies`]). Any body still in
/// `cmd` is one that may run, and is viewed as code like any other line.
///
/// Measured on 0.2.78, before this existed: `case $x in (a) rm -rf /usr;; esac`
/// was ALLOW (the pattern `(a)` was read as the program) and
/// `if true; then git push --force origin main; fi` was ALLOW (the high-blast
/// tier read `then` as the program). Both are judged by their real rule now.
pub(super) fn compound_views(cmd: &str) -> Vec<SegmentView> {
    let segs = split_segments_with_separators(cmd);
    let mut out = Vec::with_capacity(segs.len());
    let mut case_depth: usize = 0;
    let mut expect_pattern = false;
    for (i, seg) in segs.iter().enumerate() {
        let mut rest: &str = &seg.text;
        let mut data = String::new();
        let mut binding = None;
        let mut leading_opens = 0usize;
        while let Some((word, after)) = split_first_word(rest) {
            if expect_pattern {
                if word == "esac" {
                    case_depth = case_depth.saturating_sub(1);
                    expect_pattern = false;
                    rest = after;
                    continue;
                }
                if let Some(end) = case_pattern_end(rest) {
                    data.push_str(&rest[..end]);
                    data.push(' ');
                    rest = &rest[end..];
                    expect_pattern = false;
                    continue;
                }
                if seg.sep_after == SegmentSep::Pipe {
                    // `a|b)`: the `|` split the pattern, and this piece is one
                    // alternative of it. The next piece is still the pattern.
                    data.push_str(rest);
                    rest = "";
                    break;
                }
                // Not pattern-shaped where a pattern was due: judge it as the
                // command it looks like rather than guess.
                expect_pattern = false;
            }
            // 9778927f: a subshell's opening `(` in command position, whether
            // it stands alone (`( rm …`) or is glued to the first word
            // (`(rm …`, `((rm …`). It is syntax, not the program; its matching
            // `)` is removed by `strip_unmatched_close_parens` below.
            if let Some(after_paren) = rest.trim_start().strip_prefix('(') {
                rest = after_paren;
                leading_opens += 1;
                continue;
            }
            if COMPOUND_PREFIX_WORDS.contains(&word) {
                if word == "esac" {
                    case_depth = case_depth.saturating_sub(1);
                }
                rest = after;
                continue;
            }
            if word == "for" || word == "select" {
                binding = loop_binding(rest);
                data.push_str(rest);
                rest = "";
                break;
            }
            if word == "case" {
                let mut header_end = after;
                let mut saw_in = false;
                if let Some((_subject, r)) = split_first_word(after) {
                    header_end = r;
                    if let Some(("in", r2)) = split_first_word(r) {
                        header_end = r2;
                        saw_in = true;
                    }
                }
                data.push_str(&rest[..rest.len() - header_end.len()]);
                data.push(' ');
                rest = header_end;
                if saw_in {
                    case_depth += 1;
                    expect_pattern = true;
                }
                continue;
            }
            break;
        }
        // `;;` (and `;&`, `;;&`): an EMPTY segment between a `;` and the next
        // separator. Inside an open `case` it ends an arm, and a pattern is due.
        if seg.text.is_empty() && case_depth > 0 && i > 0 {
            let prev = &segs[i - 1];
            if prev.sep_after == SegmentSep::Sequential && !prev.ends_at_newline {
                expect_pattern = true;
            }
        }
        let (command, closes, open_depth) = strip_unmatched_close_parens(rest);
        out.push(SegmentView {
            data,
            command,
            binding,
            opens: leading_opens + open_depth,
            closes,
        });
    }
    out
}

/// `s` with every UNMATCHED, unquoted, unescaped `)` replaced by a space.
///
/// 9778927f: [`split_segments`] has no notion of a subshell, so the `)` that
/// closes one lands glued to the last word of its final segment
/// (`(true && git reset --hard)` → ` git reset --hard)`), and every rule then
/// read `--hard)` instead of `--hard` — measured ALLOW for a command that is a
/// DENY unwrapped. A `)` with no `(` before it in the same segment closes a
/// group opened earlier (or a leading `(` [`compound_views`] already removed),
/// so it is syntax, never part of a word. A `)` that closes a `(`/`$(`/`<(`
/// opened inside the segment is matched and kept, and quoted or backslash-
/// escaped parentheses are literal text and are not counted.
///
/// A `case` pattern's `)` never reaches here: [`compound_views`] moves the
/// pattern into `data` before the command text is built.
pub(super) fn strip_unmatched_close_parens(s: &str) -> (String, usize, usize) {
    let mut closes = 0usize;
    let mut out = String::with_capacity(s.len());
    let (mut in_s, mut in_d, mut in_bt, mut escaped) = (false, false, false, false);
    let mut depth: usize = 0;
    for c in s.chars() {
        if escaped {
            escaped = false;
            out.push(c);
            continue;
        }
        match c {
            '\\' if !in_s => escaped = true,
            '\'' if !in_d && !in_bt => in_s = !in_s,
            '"' if !in_s && !in_bt => in_d = !in_d,
            '`' if !in_s => in_bt = !in_bt,
            '(' if !in_s && !in_d && !in_bt => depth += 1,
            ')' if !in_s && !in_d && !in_bt => {
                if depth == 0 {
                    out.push(' ');
                    closes += 1;
                    continue;
                }
                depth -= 1;
            }
            _ => {}
        }
        out.push(c);
    }
    (out, closes, depth)
}

/// Judge text the shell expands but does not run as a command (a loop word
/// list, a `case` subject or pattern): only its command substitutions execute,
/// and each is judged with the full analyser, exactly as [`analyze_segment`]
/// judges the substitutions of a command.
pub(super) fn analyze_expanded_data(data: &str, depth: usize, ctx: &Ctx<'_>) -> Decision {
    if depth >= MAX_SHELL_DEPTH {
        return depth_exhausted();
    }
    let mut acc = VerdictAcc::default();
    for payload in executed_substitution_payloads(data) {
        if let Some(deny) = acc.record(analyze_shell_payload(&payload, depth, ctx)) {
            return deny;
        }
    }
    acc.finish()
}

/// Most loop-variable substitutions judged for one segment.
///
/// Each variant is one more `analyze_segment` call, so this bounds the work a
/// loop can cost; the product over several nested loop variables is what can
/// grow, not a single list. The value is a chosen bound, not a measured one;
/// past it the segment is an Ask (`LoopVariants::TooMany`), never skipped.
pub(super) const MAX_LOOP_VARIANTS: usize = 256;

/// `text` with every reference to the variable `name` (`$name`, `${name}`, and
/// the operator forms `${name%…}`, `${name:-…}` …) replaced by `value`, or
/// `None` when `text` does not reference it. A backslash-escaped `$` is
/// literal and left alone. A reference inside single quotes IS replaced — that
/// over-approximates (bash would not expand it), which can only add verdicts.
///
/// An operator form is replaced by the WHOLE value (`${f%.sh}` by all of
/// `.githooks/pre-commit.sh`), again an over-approximation: the path the
/// operator would derive lies inside the one substituted.
pub(super) fn substitute_var_refs(text: &str, name: &str, value: &str) -> Option<String> {
    let is_ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut hit = false;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\\' {
            out.push(c);
            if let Some(&n) = chars.get(i + 1) {
                out.push(n);
            }
            i += 2;
            continue;
        }
        if c == '$' {
            if chars.get(i + 1) == Some(&'{') {
                if let Some(close) = chars[i + 2..].iter().position(|&d| d == '}') {
                    let inner: String = chars[i + 2..i + 2 + close].iter().collect();
                    let names_it = inner
                        .strip_prefix(name)
                        .is_some_and(|r| !r.starts_with(is_ident));
                    if names_it {
                        out.push_str(value);
                        hit = true;
                        i += close + 3;
                        continue;
                    }
                }
            } else {
                let end = chars[i + 1..]
                    .iter()
                    .position(|&d| !is_ident(d))
                    .map_or(chars.len(), |p| i + 1 + p);
                let ident: String = chars[i + 1..end].iter().collect();
                if ident == name {
                    out.push_str(value);
                    hit = true;
                    i = end;
                    continue;
                }
            }
        }
        out.push(c);
        i += 1;
    }
    hit.then_some(out)
}

/// What the loop variables bound earlier on the line make of one segment.
pub(super) enum LoopVariants {
    /// The segment references no bound loop variable.
    Unbound,
    /// One text per combination of values, each to be judged like the segment.
    Variants(Vec<String>),
    /// More combinations than [`MAX_LOOP_VARIANTS`]: not judged, and the caller
    /// must not read that as clean.
    TooMany { name: String, count: usize },
}

/// Every text `command` becomes once the `for`/`select` variables bound
/// earlier on the line are replaced by the values the loop header lists.
///
/// Why this exists: a loop header's word list is DATA — `for f in
/// .githooks/pre-commit` modifies nothing — so it is no longer judged as the
/// operands of an unknown verb `for`. But the list is data only until the body
/// USES it: `for f in .githooks/pre-commit; do patch "$f" y; done` patches the
/// gate, and judging ` do patch "$f" y` as written would see only the
/// placeholder `"$f"`. Substituting the listed values back in is what lets the
/// body's own rules judge the path it really touches, so the header is not a
/// way to launder a path past them.
///
/// Scope is over-approximated in the restrictive direction: a variable stays
/// bound for the rest of the line (bash keeps a loop variable's last value
/// after `done`), and two loops over the same name union their values. A
/// reassignment of the name is not modelled, so it can only add a variant.
pub(super) fn loop_variants(command: &str, bindings: &[LoopBinding]) -> LoopVariants {
    // Union values per name, latest binding first, so a list that itself
    // names an earlier loop variable (`for g in $f`) is substituted before
    // that variable's own values are.
    let mut names: Vec<(&str, Vec<&str>)> = Vec::new();
    for b in bindings.iter().rev() {
        match names.iter_mut().find(|(n, _)| *n == b.name) {
            Some((_, vals)) => {
                for v in &b.values {
                    if !vals.contains(&v.as_str()) {
                        vals.push(v);
                    }
                }
            }
            None => {
                let mut vals: Vec<&str> = Vec::new();
                for v in &b.values {
                    if !vals.contains(&v.as_str()) {
                        vals.push(v);
                    }
                }
                names.push((&b.name, vals));
            }
        }
    }
    let mut variants = vec![command.to_string()];
    let mut bound = false;
    for (name, values) in names {
        if !variants
            .iter()
            .any(|v| substitute_var_refs(v, name, "").is_some())
        {
            continue;
        }
        bound = true;
        let count = variants.len() * values.len();
        if count > MAX_LOOP_VARIANTS {
            return LoopVariants::TooMany {
                name: name.to_string(),
                count,
            };
        }
        let mut next = Vec::with_capacity(count);
        for v in &variants {
            for value in &values {
                next.push(substitute_var_refs(v, name, value).unwrap_or_else(|| v.clone()));
            }
        }
        variants = next;
    }
    if bound {
        LoopVariants::Variants(variants)
    } else {
        LoopVariants::Unbound
    }
}
