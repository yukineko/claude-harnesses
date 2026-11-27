//! Static resolution of variables whose value the command TEXT alone fixes
//! (backlog 3a950c6c / 5ef2d51f, user rulings 2026-11-14: 「静的に判定できない
//! blastguard の実装がゴミなので、それをなおす」「必ず静的に判定し、判断する」).
//!
//! ONE resolver for the whole top-level Bash command, run before any rule:
//! [`resolve_static_bindings`] walks the statements of the line in order,
//! keeps an environment of the names an EARLIER statement bound to a literal,
//! and rewrites every later plain reference (`$X`, `${X}`, `"$X"`, glued
//! spellings such as `"$T"bin/tdd`, `${W}/a`, `$T/bin`) to that literal. Every
//! sink — `sed -i` / `unlink` / `mv` targets, the command word, redirect
//! targets, `rm` operands, `cd` — then judges the resolved text, so a resolved
//! value is judged exactly like its literal spelling and never reaches a
//! "cannot resolve" Ask.
//!
//! Bindings recorded (each only when the statement runs in the parent shell —
//! not a pipeline stage, not backgrounded — and unconditionally, see
//! [`assignment_execution_is_unconditional`]):
//!   * a statement that is only assignments: `X=lit`, `X="lit"`, `X='lit'`;
//!   * `export X=lit` (operands all `NAME=value` or bare names, no flags);
//!   * a value built from names already bound (`B=$A/x`) resolves too, since
//!     the value is rewritten before it is read;
//!   * an assignment that only an `&&` guard keeps from being unconditional
//!     (`cd /p && a=lit`, `a=x && b=lit`) binds ONLY along its own `&&`
//!     chain, and only when every separator of its and-or list BEFORE it is
//!     `&&` too: then a later segment reached from it through nothing but
//!     `&&` runs only if every earlier command of the list ran, the
//!     assignment included (backlog a0551e29), so `cd /p && a=lit && git add
//!     -- $a` resolves. A `||` anywhere before it binds nothing (`X || b=lit
//!     && cmd $b` is `(X || b=lit) && cmd`, which runs `cmd` without `b=lit`
//!     when `X` succeeds). A `||`, `;`, newline, `|` or `&` after it ends the
//!     binding, and the name is then unbound (it may or may not have been
//!     assigned). See `assignment_execution_is_guarded_only_by_and_or`.
//!
//! `HOME` is bound before the first statement to the hook environment's own
//! `HOME` (user ruling 2026-11-17, SUBSET.md ruling 12: `$HOME` / `${HOME}`
//! / `~` are Known), so `rm -r "$HOME/.cache/x"` and `rm -r ~/.cache/x` are
//! judged exactly as their absolute spelling; every rule below that drops or
//! rebinds a name applies to it unchanged. Unlike a line-bound name, `HOME`
//! is NOT substituted inside a longer double-quoted string (only unquoted or
//! as a whole double-quoted word), so `perl -ne "print $HOME"` and
//! `"$HOME/My Docs"` keep their spelling and their legacy verdict.
//!
//! A value is recorded only when it is a non-empty literal made of characters
//! whose meaning is the same unquoted and in a word (`[A-Za-z0-9/._+,:@%^-]`):
//! no whitespace, quote, `$`, backtick, backslash, glob, brace, `~`, `=`, `#`
//! or operator character, so substituting it can neither re-split the line nor
//! change word splitting, globbing, tilde or brace expansion.
//!
//! Everything whose value exists only at run time stays unresolved (and so
//! keeps its restrictive verdict): `$(cmd)` / backtick values and reassignment
//! from them, unassigned names, assignments guarded by `&&`/`||` (outside
//! their own `&&` chain, see above) or inside an
//! `if`/loop/`case`/subshell/brace group, a pipeline stage or `&`, arrays,
//! `+=`, any other statement mentioning the name outside a plain reference
//! (`read X`, `for X in`, `unset X`, `printf -v X`, `(X=…`, `$((X=…))`; the
//! name as a path component between two slashes — the `fd` of `/dev/fd/$fd`
//! — is literal text and does not count, so `fd=3; …; bash /dev/fd/$fd`
//! resolves to `bash /dev/fd/3` like `<&$fd` does, backlog 54231a0a; a
//! mention inside a closed `$(…)` / backtick / `<(…)` / `>(…)` body binds
//! only in that subshell and does not count for a reference OUTSIDE the
//! bodies — `X=lit; y=$(X=o; true) cmd $X` resolves the outer `$X`, backlog
//! d068ef20 — while a reference INSIDE a body is refused whenever the
//! segment mentions the name anywhere outside a plain reference), a
//! name rebound anywhere inside a loop (from the loop opener on), `eval` /
//! `source` / `.` (the environment is dropped), shell-maintained names
//! (`PWD`, `RANDOM`, `_`, …), and here-document bodies.
//!
//! The whole line is left exactly as written (no resolution at all) when it
//! defines a shell function, mentions `IFS`, uses `trap` / `alias` /
//! `readonly` / `declare` / `typeset` / `local` (namerefs, read-only names,
//! deferred code), contains a `#` comment or ANSI-C `$'…'` quoting (the quote
//! tracking here does not model them), or its statements cannot be mapped
//! back onto its text.

use super::super::segment::{
    blank_subshell_bodies, is_separator_char, segment_spans, subshell_substitution_bodies,
};
use super::super::vars::{
    defines_shell_function, exemption_disabled, has_unresolvable_expansion, indirect_name_operand,
    is_shell_identifier, quote_aware_words, segment_mentions_name, shell_modes, tied_partner,
    unquote_literal_value,
};
use super::super::{
    assignment_execution_is_guarded_only_by_and_or, assignment_execution_is_unconditional,
    assignments_reach_parent_shell, here_document_layout, is_assignment, normalized_command,
    split_segments_with_separators, SegmentSep, SeparatedSegment,
};
use std::collections::HashMap;

/// Names the shell itself maintains or changes without the line naming them.
const SHELL_MAINTAINED: &[&str] = &[
    "_",
    "PWD",
    "OLDPWD",
    "RANDOM",
    "SRANDOM",
    "SECONDS",
    "LINENO",
    "BASHPID",
    "BASH_COMMAND",
    "BASH_SUBSHELL",
    "BASH_REMATCH",
    // zsh's `[[ x =~ re ]]` sets these.
    "MATCH",
    "MBEGIN",
    "MEND",
    "match",
    "mbegin",
    "mend",
    "EPOCHSECONDS",
    "EPOCHREALTIME",
    "REPLY",
    "OPTARG",
    "OPTIND",
    "HISTCMD",
    "PPID",
    "UID",
    "EUID",
    "MAPFILE",
    "COLUMNS",
    "LINES",
    "SHLVL",
    "PIPESTATUS",
    "FUNCNAME",
];

/// Names bound from the hook's environment rather than by the line (ruling
/// 12): see [`substitute_references`] for where they are substituted.
const ENV_SEEDED: &[&str] = &["HOME"];

/// Words whose presence anywhere makes the line ineligible (see module doc).
const BAIL_WORDS: &[&str] = &["trap", "alias", "readonly", "declare", "typeset", "local"];

/// `cmd` with every reference whose value the line's own earlier statements
/// fix replaced by that value; `cmd` unchanged when nothing resolves or the
/// line is ineligible (see the module doc).
pub(super) fn resolve_static_bindings(cmd: &str) -> String {
    resolve_static_bindings_with_home(cmd, hook_home().as_deref())
}

/// `HOME` of the hook's own environment when it may be substituted (user
/// ruling 2026-11-17, SUBSET.md ruling 12): non-empty, absolute, and made
/// only of [`is_substitutable_char`] characters, so the substituted text
/// reads as one plain word.
fn hook_home() -> Option<String> {
    std::env::var("HOME")
        .ok()
        .filter(|h| h.starts_with('/') && h.chars().all(is_substitutable_char))
}

/// [`resolve_static_bindings`] with the hook environment's `HOME` given:
/// `HOME` starts bound to it (ruling 12: `$HOME` / `${HOME}` are Known), so
/// it resolves under exactly the rules of a name an earlier statement bound
/// — a statement that rebinds, unsets or otherwise names `HOME` drops or
/// replaces it — and a bare `~` that starts a word ([`substitute_tilde`]) is
/// that value too while `HOME` is still bound.
fn resolve_static_bindings_with_home(cmd: &str, home: Option<&str>) -> String {
    let tilde_candidate = home.is_some() && cmd.contains('~');
    if (!cmd.contains('$') && !tilde_candidate) || line_is_ineligible(cmd) {
        return cmd.to_string();
    }
    let segs = split_segments_with_separators(cmd);
    let Some(spans) = segment_spans(cmd, &segs) else {
        return cmd.to_string();
    };
    let (is_body, unclosed_opener) = here_document_layout(&segs);
    let loop_start = segs.iter().position(|s| opens_loop(&s.text));
    let exempt_ok = !exemption_disabled(cmd);
    let mut env: HashMap<String, String> = HashMap::new();
    if let Some(h) = home {
        env.insert("HOME".to_string(), h.to_string());
    }
    // `~` inside arithmetic is bitwise NOT, not a home directory.
    let tilde_ok = tilde_candidate && !cmd.contains("((") && !cmd.contains("$[");
    // Bindings of an `&&`-guarded assignment, valid only along its own `&&`
    // chain (see `assignment_execution_is_guarded_only_by_and_or`). Consulted
    // before `env`: such an assignment removed the name from `env`.
    let mut chain: HashMap<String, String> = HashMap::new();
    let mut and_prefix = true;
    let mut out = String::with_capacity(cmd.len());
    let mut cursor = 0;
    for (idx, (seg, &(start, end))) in segs.iter().zip(spans.iter()).enumerate() {
        let Some(gap) = cmd.get(cursor..start) else {
            return cmd.to_string();
        };
        // The chain continues only across a plain `&&`; `||`, `;`, a
        // newline, `|` or `&` ends what the text guarantees about it.
        let continues_chain = idx > 0
            && matches!(segs[idx - 1].sep_after, SegmentSep::Conditional)
            && gap.trim() == "&&";
        if !continues_chain {
            chain.clear();
        }
        // Every separator from the start of this segment's and-or list up to
        // it is `&&` (or it starts the list). Only then does a later `&&`
        // segment running imply this one ran: `X || b=lit && cmd` is
        // `(X || b=lit) && cmd`, which runs `cmd` without `b=lit` when `X`
        // succeeds. A list start is only trusted after a `;`/newline/`&` that
        // follows an ordinary code segment: a blank or comment-only segment
        // (the line break after a trailing `&&`/`||`) or a here-document body
        // line can sit INSIDE a continued and-or list.
        and_prefix = match idx.checked_sub(1).and_then(|p| segs.get(p)) {
            None => true,
            Some(prev) => match prev.sep_after {
                SegmentSep::Conditional => and_prefix && continues_chain,
                SegmentSep::Sequential | SegmentSep::Background | SegmentSep::EndOfLine => {
                    let t = prev.text.trim_start();
                    !t.is_empty()
                        && !t.starts_with('#')
                        && !is_body.get(idx - 1).copied().unwrap_or(true)
                }
                SegmentSep::Pipe => false,
            },
        };
        out.push_str(gap);
        cursor = end;
        // Past an unclosed here-document opener nothing is known to be code.
        if unclosed_opener.is_some_and(|o| idx > o) {
            out.push_str(cmd.get(start..).unwrap_or(""));
            return out;
        }
        if is_body.get(idx).copied().unwrap_or(true) {
            out.push_str(&seg.text);
            continue;
        }
        let in_loop = loop_start.is_some_and(|l| idx >= l);
        // What THIS shell binds: the segment with its subshell substitution
        // bodies blanked (backlog d068ef20). `y=$(L=/tmp/x; true) rm $L`
        // rebinds `L` only inside the subshell, so the outer `$L` keeps the
        // earlier binding. A reference INSIDE a body is still refused when
        // the raw text mentions the name anywhere (`$(L=/tmp/x; rm $L)`
        // reads the body's own `L`), see `body_lookup` below.
        let parent_view = blank_subshell_bodies(&seg.text);
        let bodies = subshell_substitution_bodies(&seg.text);
        let lookup = |name: &str| -> Option<String> {
            let value = chain.get(name).or_else(|| env.get(name))?;
            if segment_mentions_name(&parent_view, name, exempt_ok) {
                return None;
            }
            if in_loop
                && segs
                    .iter()
                    .skip(loop_start.unwrap_or(0))
                    .any(|s| segment_mentions_name(&s.text, name, exempt_ok))
            {
                return None;
            }
            Some(value.clone())
        };
        // Inside a body, and for `~` (substituted over the whole text): the
        // raw segment must not mention the name at all.
        let strict_lookup = |name: &str| -> Option<String> {
            if segment_mentions_name(&seg.text, name, exempt_ok) {
                return None;
            }
            lookup(name)
        };
        let body_lookup = |name: &str, at: usize| -> Option<String> {
            if bodies.iter().any(|r| r.contains(&at)) {
                strict_lookup(name)
            } else {
                lookup(name)
            }
        };
        let mut text = substitute_references(&seg.text, &body_lookup);
        if tilde_ok {
            if let Some(h) = strict_lookup("HOME") {
                text = substitute_tilde(&text, &h);
            }
        }
        update_env(
            &mut env, &mut chain, and_prefix, &segs, idx, &text, exempt_ok,
        );
        out.push_str(&text);
    }
    match cmd.get(cursor..) {
        Some(tail) if tail.chars().all(is_separator_char) => out.push_str(tail),
        _ => return cmd.to_string(),
    }
    out
}

fn line_is_ineligible(cmd: &str) -> bool {
    if defines_shell_function(cmd) || cmd.contains("IFS") {
        return true;
    }
    // An alias / hook / sourced code / undetermined command word anywhere on
    // the line: no earlier binding is believed (backlog f3301770).
    if shell_modes(cmd).alias {
        return true;
    }
    if quote_aware_words(cmd).iter().any(|w| {
        let w = w.trim_start_matches(['(', '{']);
        BAIL_WORDS.contains(&w)
    }) {
        return true;
    }
    has_comment_or_ansi_c_quote(cmd)
}

/// True when, outside quotes, a `#` starts a word (a comment) or `$'` opens
/// ANSI-C quoting.
fn has_comment_or_ansi_c_quote(cmd: &str) -> bool {
    let chars: Vec<char> = cmd.chars().collect();
    let (mut in_s, mut in_d) = (false, false);
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\\' && !in_s {
            i += 2;
            continue;
        }
        if c == '\'' && !in_d {
            in_s = !in_s;
        } else if c == '"' && !in_s {
            in_d = !in_d;
        } else if !in_s && !in_d {
            if c == '$' && chars.get(i + 1) == Some(&'\'') {
                return true;
            }
            let at_word_start = i == 0
                || chars
                    .get(i - 1)
                    .is_some_and(|p| p.is_whitespace() || matches!(p, ';' | '&' | '|' | '(' | ')'));
            if c == '#' && at_word_start {
                return true;
            }
        }
        i += 1;
    }
    false
}

fn opens_loop(seg: &str) -> bool {
    quote_aware_words(seg).iter().any(|w| {
        matches!(
            w.trim_start_matches(['(', '{']),
            "for" | "while" | "until" | "select"
        )
    })
}

/// A literal assignment value as the shell stores it, or `None` when the text
/// does not fix one this resolver may substitute.
fn literal_value(raw: &str) -> Option<String> {
    if raw.starts_with('(') || has_unresolvable_expansion(raw) {
        return None;
    }
    let value = unquote_literal_value(raw)?;
    if value.is_empty() || !value.chars().all(is_substitutable_char) {
        return None;
    }
    Some(value)
}

fn is_substitutable_char(c: char) -> bool {
    c.is_ascii_alphanumeric()
        || matches!(c, '/' | '.' | '_' | '+' | ',' | ':' | '@' | '%' | '^' | '-')
}

/// Record what segment `idx` (already rewritten to `text`) binds: in `env`
/// when the assignment runs unconditionally, in `chain` (valid only along
/// the `&&` chain it stands in, backlog a0551e29) when only an `&&` guard
/// stops it from being unconditional and `and_prefix` says every separator of
/// its and-or list before it is `&&`. Anything that drops a name from `env`
/// drops it from `chain` too.
fn update_env(
    env: &mut HashMap<String, String>,
    chain: &mut HashMap<String, String>,
    and_prefix: bool,
    segs: &[SeparatedSegment],
    idx: usize,
    text: &str,
    exempt_ok: bool,
) {
    let Some(orig) = segs.get(idx).map(|s| s.text.as_str()) else {
        env.clear();
        chain.clear();
        return;
    };
    let words = quote_aware_words(text);
    let Some(first) = words.first() else {
        return;
    };
    let head = normalized_command(first);
    let wrapped_dot = matches!(head.as_str(), "command" | "builtin")
        && words.get(1).map(|w| w.as_str()) == Some(".");
    if head == "."
        || wrapped_dot
        || words
            .iter()
            .any(|w| matches!(unquote_literal_value(w).as_deref(), Some("eval" | "source")))
    {
        env.clear();
        chain.clear();
        return;
    }
    let assignments: Option<&[String]> = if words.iter().all(|w| is_assignment(w)) {
        Some(&words[..])
    } else if head == "export"
        && first == "export"
        && words
            .iter()
            .skip(1)
            .all(|w| is_assignment(w) || is_shell_identifier(w))
    {
        words.get(1..)
    } else {
        None
    };
    let reaches_parent = assignments_reach_parent_shell(segs, idx);
    let runs_here = reaches_parent && assignment_execution_is_unconditional(segs, idx);
    let runs_in_chain =
        reaches_parent && and_prefix && assignment_execution_is_guarded_only_by_and_or(segs, idx);
    match assignments {
        Some(list) if runs_here || runs_in_chain => {
            for w in list {
                let Some(eq) = w.find('=') else {
                    continue;
                };
                let (name, raw) = (&w[..eq], &w[eq + 1..]);
                // Whichever map the name is bound in, the other one's binding
                // is stale now.
                chain.remove(name);
                env.remove(name);
                // zsh ties `path`/`PATH` (and the other pairs): assigning one
                // assigns the other (backlog f3301770).
                if let Some(p) = tied_partner(name) {
                    chain.remove(p);
                    env.remove(p);
                }
                let Some(v) = literal_value(raw).filter(|_| !SHELL_MAINTAINED.contains(&name))
                else {
                    continue;
                };
                if runs_here {
                    env.insert(name.to_string(), v);
                } else {
                    chain.insert(name.to_string(), v);
                }
            }
        }
        _ => {
            // A name bound inside a subshell substitution body (`y=$(L=x; …)
            // cmd`) is not rebound here (backlog d068ef20).
            let parent_view = blank_subshell_bodies(orig);
            env.retain(|name, _| !segment_mentions_name(&parent_view, name, exempt_ok));
            chain.retain(|name, _| !segment_mentions_name(&parent_view, name, exempt_ok));
            // A binder whose NAME operand is an expansion (`read "$Q"`) may
            // bind any variable (backlog f3301770).
            if indirect_name_operand(&quote_aware_words(orig)) {
                env.clear();
                chain.clear();
            }
        }
    }
}

/// Replace each plain reference (`$NAME`, `${NAME}`) outside single quotes for
/// which `lookup` answers a value.
///
/// A double-quoted span that holds at least one reference and that, once every
/// reference in it is replaced, is made only of substitutable characters
/// (`"$T"`, `"$P/fstab"`) loses its quotes: nothing left in it is a character
/// the quotes would protect, so the unquoted text is the same word, and the
/// rules then see exactly the literal spelling (`> "$P/fstab"` with `P=/etc`
/// is judged, and reported, as `> /etc/fstab`). A span that keeps anything
/// else — an unresolved reference, a glob character, a space — keeps its
/// quotes.
fn substitute_references(text: &str, lookup: &dyn Fn(&str, usize) -> Option<String>) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let (mut in_s, mut in_d) = (false, false);
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\\' && !in_s {
            out.push(c);
            if let Some(n) = chars.get(i + 1) {
                out.push(*n);
            }
            i += 2;
            continue;
        }
        if c == '\'' && !in_d {
            in_s = !in_s;
            out.push(c);
            i += 1;
            continue;
        }
        if c == '"' && !in_s {
            if !in_d {
                if let Some((unquoted, close)) = resolved_double_quoted_span(&chars, i, lookup) {
                    out.push_str(&unquoted);
                    i = close + 1;
                    continue;
                }
            }
            in_d = !in_d;
            out.push(c);
            i += 1;
            continue;
        }
        if c == '$' && !in_s {
            if let Some((name, len)) = reference_at(&chars, i) {
                // An environment-seeded name is substituted only where the
                // result is one plain word (unquoted, or a whole
                // double-quoted span `resolved_double_quoted_span` resolved
                // above) — never inside a longer double-quoted string such
                // as interpreter program text (`perl -ne "print $HOME"`):
                // there the substitution would turn a run-time expansion
                // into program text the readings then clear, lowering the
                // legacy Ask (raise-only until S6, ruling 14; corpus control
                // `control_1732_perl_double_quoted_home`).
                let seeded_in_quotes = in_d && ENV_SEEDED.contains(&name.as_str());
                if let Some(v) = lookup(&name, i).filter(|_| !seeded_in_quotes) {
                    out.push_str(&v);
                    i += len;
                    continue;
                }
            }
        }
        out.push(c);
        i += 1;
    }
    out
}

/// For the double-quoted span opening at `chars[open]`: its text with every
/// reference resolved and the quotes dropped, plus the index of the closing
/// quote — or `None` when the span holds no reference, holds a backslash,
/// backtick or single quote, does not close, or still holds anything outside
/// the substitutable set after resolution.
fn resolved_double_quoted_span(
    chars: &[char],
    open: usize,
    lookup: &dyn Fn(&str, usize) -> Option<String>,
) -> Option<(String, usize)> {
    let close = (open + 1..chars.len()).find(|&k| chars.get(k) == Some(&'"'))?;
    let content = chars.get(open + 1..close)?;
    if !content.contains(&'$') || content.iter().any(|c| matches!(c, '\\' | '`' | '\'')) {
        return None;
    }
    let mut out = String::new();
    let mut k = 0;
    while k < content.len() {
        if let Some((name, len)) = reference_at(content, k) {
            out.push_str(&lookup(&name, open + 1 + k)?);
            k += len;
            continue;
        }
        let c = *content.get(k)?;
        if !is_substitutable_char(c) {
            return None;
        }
        out.push(c);
        k += 1;
    }
    if out.is_empty() {
        return None;
    }
    Some((out, close))
}

/// Replace each bare `~` that starts a word outside quotes and is followed by
/// `/`, a blank, a separator or the end (`~`, `~/x`; not `~user`, `~+`, `~-`,
/// not `a=~/x` or `x:~/y`, whose expansion depends on how bash classifies
/// the word — those keep their spelling and so their restrictive verdict)
/// by `home`: the expansion bash performs (ruling 12).
fn substitute_tilde(text: &str, home: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let (mut in_s, mut in_d) = (false, false);
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\\' && !in_s {
            out.push(c);
            if let Some(n) = chars.get(i + 1) {
                out.push(*n);
            }
            i += 2;
            continue;
        }
        if c == '\'' && !in_d {
            in_s = !in_s;
        } else if c == '"' && !in_s {
            in_d = !in_d;
        } else if c == '~' && !in_s && !in_d {
            let at_word_start = i == 0
                || chars.get(i - 1).is_some_and(|p| {
                    p.is_whitespace() || matches!(p, ';' | '&' | '|' | '(' | ')' | '`')
                });
            let ends_prefix = chars.get(i + 1).is_none_or(|n| {
                *n == '/' || n.is_whitespace() || matches!(n, ';' | '&' | '|' | ')' | '`')
            });
            if at_word_start && ends_prefix {
                out.push_str(home);
                i += 1;
                continue;
            }
        }
        out.push(c);
        i += 1;
    }
    out
}

/// The name and char length of a plain reference starting at `chars[at]`
/// (`$NAME` with the longest identifier, or `${NAME}` with nothing between the
/// name and `}`), if one starts there.
fn reference_at(chars: &[char], at: usize) -> Option<(String, usize)> {
    if chars.get(at) != Some(&'$') {
        return None;
    }
    let braced = chars.get(at + 1) == Some(&'{');
    let start = at + if braced { 2 } else { 1 };
    let mut end = start;
    while chars
        .get(end)
        .is_some_and(|c| c.is_ascii_alphanumeric() || *c == '_')
    {
        end += 1;
    }
    let name: String = chars.get(start..end)?.iter().collect();
    if !is_shell_identifier(&name) {
        return None;
    }
    if braced {
        if chars.get(end) != Some(&'}') {
            return None;
        }
        return Some((name, end + 1 - at));
    }
    Some((name, end - at))
}
