//! Whether a shell glob on this command line may match dot-names (backlog
//! dotglob fail-open, 2026-11-20). Spec: `docs/specs/blastguard.md` S6
//! "dotglob" and `SUBSET.md` "Deny-target table".
//!
//! A shell glob component that does not start with `.` matches a name
//! starting with `.` only when the shell was told to: bash `shopt -s
//! dotglob` or a non-null `GLOBIGNORE`, zsh `GLOB_DOTS` (`setopt globdots`,
//! any case, `_` ignored, `unsetopt noglobdots`, `set -o globdots`, `set -4`
//! / `setopt -4`, `options[globdots]=on`). The dot-name rule of
//! `deny_targets::globs_may_intersect` ("`*` cannot reach `.gitconfig`") is
//! only sound when NONE of those can have happened, so the line is read once
//! and classified as a [`DotMatch`]:
//!
//! * [`DotMatch::On`] — a statement may switch dot-matching on as written:
//!   an option builtin's or a child shell's (`bash -O`, `zsh -o`) word, or
//!   an assignment name, that IS (lowercased, `_` removed, `no` prefixes
//!   dropped, the part before `=`) `dotglob`, `globdots` or `globignore`, or
//!   an `options[…globdots…]` element ([`is_switch_word`]); a `set` /
//!   `setopt` / `unsetopt` / `zsh` option cluster (`-…4…` / `+…4…`); the
//!   same inside an `eval` text or a child shell's `-c` text.
//!   Over-approximates: `shopt -u dotglob` also counts (stricter only).
//! * [`DotMatch::Maybe`] — the line runs text blastguard cannot read for it:
//!   an undetermined command word, an option builtin (`shopt`, `setopt`,
//!   `unsetopt`, `set`, `emulate`, `typeset`, `declare`, `eval`, …) with an
//!   undetermined word, `source` / `.`, a whole `options=(…)` assignment, or
//!   (when the line's analysis is `Undetermined`) any of those names in the
//!   text.
//! * [`DotMatch::Off`] — none of the above.
//!
//! The answer is held in a thread-local for the judgement of one top-level
//! line ([`with_line`]); every nested reading (`bash -c '…'`) judged inside it
//! sees the outer line's answer, whose text contains the nested one. Read
//! outside any line ([`current`]) it is `Maybe` (fail-closed).
//!
//! NOT seen (stated, not hidden): dot-matching switched on OUTSIDE the
//! command line (an rc file, an earlier Bash call in the same persistent
//! shell). The shell blastguard's verdicts describe starts without it.

use std::cell::Cell;

use super::super::{analyze, Analysis, Script, SimpleCommand, Value};

/// See the module doc.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DotMatch {
    Off,
    On,
    Maybe,
}

thread_local! {
    static LINE: Cell<Option<DotMatch>> = const { Cell::new(None) };
}

/// The [`DotMatch`] of the line being judged; `Maybe` outside [`with_line`].
pub(crate) fn current() -> DotMatch {
    LINE.with(Cell::get).unwrap_or(DotMatch::Maybe)
}

/// Run `f` with [`current`] answering for `line`. Nested calls keep the
/// outermost line's answer.
pub(crate) fn with_line<R>(line: &str, f: impl FnOnce() -> R) -> R {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            LINE.with(|c| c.set(None));
        }
    }
    if LINE.with(Cell::get).is_some() {
        return f();
    }
    let m = of_line(line);
    LINE.with(|c| c.set(Some(m)));
    let _reset = Reset;
    f()
}

/// The [`DotMatch`] of `line`.
pub(crate) fn of_line(line: &str) -> DotMatch {
    match analyze(line) {
        Analysis::Known(script) => of_script(&script),
        Analysis::Undetermined(_) => of_text(line),
    }
}

fn worst(a: DotMatch, b: DotMatch) -> DotMatch {
    match (a, b) {
        (DotMatch::On, _) | (_, DotMatch::On) => DotMatch::On,
        (DotMatch::Maybe, _) | (_, DotMatch::Maybe) => DotMatch::Maybe,
        _ => DotMatch::Off,
    }
}

fn of_script(s: &Script) -> DotMatch {
    s.commands.iter().map(of_command).fold(DotMatch::Off, worst)
}

/// True when the shell WORD `w` names a dot-matching switch: with outer
/// quotes and `;&|()` trimmed, the part before any `=`, lowercased and with
/// `_` removed (zsh option names ignore both) and any `no` prefixes
/// dropped, is `dotglob`, `globdots` or `globignore`, or it is an
/// `options[…globdots…]` element. Whole-word, so a path that merely
/// contains the letters (`/tmp/dotglob-x/f`) is not one.
fn is_switch_word(w: &str) -> bool {
    let w = w.trim_matches(|c: char| "'\";&|()".contains(c));
    let head = w.split('=').next().unwrap_or(w);
    let mut n: String = head
        .chars()
        .filter(|&c| c != '_')
        .flat_map(char::to_lowercase)
        .collect();
    if n.starts_with("options[") {
        return n.contains("globdots");
    }
    while let Some(rest) = n.strip_prefix("no") {
        n = rest.to_string();
    }
    matches!(n.as_str(), "dotglob" | "globdots" | "globignore")
}

/// Builtins that change or may change shell options.
const OPTION_BUILTINS: &[&str] = &[
    "shopt", "setopt", "unsetopt", "set", "emulate", "typeset", "declare", "export", "local",
    "readonly", "eval",
];

/// Child shells: their option words (`bash -O dotglob`, `zsh -o globdots`,
/// `zsh -4`) and their `-c` text decide how THEIR globs expand.
const SHELLS: &[&str] = &["bash", "sh", "zsh", "dash", "ksh"];

/// Words skipped before the command word.
const WRAPPERS: &[&str] = &["builtin", "command", "noglob", "nocorrect", "exec"];

fn of_command(c: &SimpleCommand) -> DotMatch {
    let mut m = DotMatch::Off;
    for (name, _) in &c.assignments {
        if is_switch_word(name) {
            return DotMatch::On;
        }
        if name.eq_ignore_ascii_case("options") {
            m = DotMatch::Maybe;
        }
    }
    let mut words = c.argv.iter();
    let name = loop {
        let Some(w) = words.next() else { return m };
        let Value::Literal(v) = &w.value else {
            return DotMatch::Maybe;
        };
        let base = v.rsplit('/').next().unwrap_or(v);
        if !WRAPPERS.contains(&base) {
            break base.to_string();
        }
    };
    let rest: Vec<_> = words.collect();
    if name == "source" || name == "." {
        return DotMatch::Maybe;
    }
    if SHELLS.contains(&name.as_str()) {
        for w in &rest {
            if let Value::Literal(v) = &w.value {
                if is_switch_word(v) || (name == "zsh" && zsh_cluster_4(&w.value)) {
                    return DotMatch::On;
                }
                if v.contains(char::is_whitespace) || v.contains(';') {
                    m = worst(m, of_line(v));
                }
            }
        }
        return m;
    }
    if !OPTION_BUILTINS.contains(&name.as_str()) {
        return m;
    }
    let mut literals = Vec::with_capacity(rest.len());
    for w in &rest {
        let Value::Literal(v) = &w.value else {
            return DotMatch::Maybe;
        };
        literals.push(v.as_str());
    }
    if literals.iter().any(|v| is_switch_word(v)) {
        return DotMatch::On;
    }
    if matches!(name.as_str(), "set" | "setopt" | "unsetopt")
        && rest.iter().any(|w| zsh_cluster_4(&w.value))
    {
        return DotMatch::On;
    }
    if name == "eval" {
        return worst(m, of_line(&literals.join(" ")));
    }
    m
}

/// True for an option cluster naming zsh's `GLOB_DOTS` letter (`-4`, `+4`,
/// `-e4`; `+4` unsets it under `set`/`setopt` and sets it under
/// `unsetopt`, so either sign counts).
fn zsh_cluster_4(v: &Value) -> bool {
    matches!(v, Value::Literal(v) if (v.starts_with('-') || v.starts_with('+')) && v.contains('4'))
}

/// Fallback for a line whose analysis is `Undetermined`: [`DotMatch::On`]
/// when a whitespace-separated word names a switch ([`is_switch_word`]), `Maybe` when it names an option builtin,
/// `source` or a `.` command word, else `Off`.
fn of_text(line: &str) -> DotMatch {
    if line.split_whitespace().any(is_switch_word) {
        return DotMatch::On;
    }
    let is_word = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '-';
    let mut names = OPTION_BUILTINS.to_vec();
    names.extend(["source", "options"]);
    let words: Vec<&str> = line.split(|c: char| !is_word(c)).collect();
    if words.iter().any(|w| names.contains(w)) {
        return DotMatch::Maybe;
    }
    let b = line.as_bytes();
    let dot_command = b.iter().enumerate().any(|(i, &c)| {
        c == b'.'
            && (i == 0 || b" \t\n;&|(".contains(&b[i - 1]))
            && b.get(i + 1).is_some_and(|n| n.is_ascii_whitespace())
    });
    if dot_command {
        DotMatch::Maybe
    } else {
        DotMatch::Off
    }
}
