//! The path-resolution detectors' read of [`super::analyze`]: the statically
//! evaluated value of a word spelled in the top-level command
//! (run-20261116-020031-33040, task `migrate-path`).
//!
//! The string-reading detectors (`>` / `>>` redirects, `rm`, `sed -i` /
//! `chmod` / the other system-file mutators, the `cp` / `mv` / `install`
//! destination) name the word they cannot place by its SPELLING (`$S/x`,
//! `"$f"`, `tests/a.rs` after a `cd`). [`resolve`] answers, for such a
//! spelling, the paths the AST evaluator proved it names — and the detector
//! then judges each of those paths with the predicates it already applies to
//! a literal spelling. It never judges anything itself.
//!
//! # When a spelling resolves (all must hold; otherwise `None`, and the
//! detector keeps its old verdict)
//!
//! * [`with_index`] is active (the top-level Bash reading of
//!   `detect_bash_every_reading`) and [`super::analyze`] of that text is
//!   `Known`;
//! * the detector runs at shell depth 0 ([`enter_depth`]): a nested payload
//!   (`bash -c '…'`, `eval`, a `find -exec` tail, program text) is a
//!   different program whose variables the index does not describe;
//! * the index is not [`suspended`] (the bash 3.2 reading when its parse
//!   differs from the modern one, and the comment-hides-quotes candidate:
//!   brush-parser reads the modern grammar, so its values are not that
//!   parse's values);
//! * the spelling occurs in the analysed text at least once, and EVERY
//!   occurrence of it (every byte offset, overlapping ones included) is
//!   exactly one word of an evaluated command: an argv word, or the target
//!   word of a file redirect. An occurrence anywhere else — inside a longer
//!   word, a quoted string, a heredoc body, a comment, an assignment value —
//!   means the detector may be reading a text the evaluator did not evaluate
//!   as that word, so nothing is resolved;
//! * at least one occurrence is a word in the detector's [`Role`] (a redirect
//!   target for the `>` / `>>` rules, an argv word for the command-operand
//!   rules), and every evaluated value of every word in that role (a `for`
//!   body unrolled N times gives N values per word) is a `Literal`,
//!   non-empty, and made only of `[A-Za-z0-9/._+,:@%^-]` — the characters the
//!   detectors' unquoting reads the same as the shell, so the path handed
//!   back cannot be re-read as a different one.
//!
//! Occurrences in the OTHER role do not contribute values: a redirect rule
//! flags a redirect target, and the shell writes to no argv word; an argv
//! word that is also spelled as a redirect target elsewhere is that
//! redirect's own rule's to judge (it reads its own role). So
//! `S=/x && cat > $S/a; python3 p.py $S/a` resolves the redirect `$S/a` to
//! `/x/a` although the later argv `$S/a` (where `S` may be unset) does not
//! resolve.
//!
//! A value is resolved against the command's cwd exactly as
//! [`super::RedirTarget::Path`] is: absolute after a literal `cd`, as written
//! when no `cd` applies (the detector places it against the hook's cwd as it
//! does any relative spelling), never a relative literal after another `cd`.
//!
//! Every word in the role is handed back, whichever command it belongs to,
//! so a detector judges at least the paths its own occurrence can name — a
//! value it did not need only adds a judgement.

use std::cell::{Cell, RefCell};
use std::ops::Range;
use std::rc::Rc;

use super::eval::resolve_at;
use super::{analyze, Analysis, RedirTarget, Script, Value};

thread_local! {
    static INDEX: RefCell<Active> = const { RefCell::new(Active::Inactive) };
    static DEPTH: Cell<usize> = const { Cell::new(0) };
    static SUSPENDED: Cell<bool> = const { Cell::new(false) };
}

/// Which words of the index a detector's spelling can be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Role {
    /// The target word of a file redirect (`> w`, `>> w`, `&> w`, `< w`).
    RedirectTarget,
    /// A word of a simple command's argv.
    Argv,
}

/// The evaluated words of one analysed text.
struct Index {
    text: String,
    /// `(byte range of the word in text, role, evaluated value)`; one entry
    /// per evaluated instance (an unrolled loop repeats a range).
    words: Vec<(Range<usize>, Role, Value)>,
}

impl Index {
    fn build(text: &str, script: &Script) -> Index {
        let mut words = Vec::new();
        for cmd in &script.commands {
            for w in &cmd.argv {
                if text.get(w.span.clone()) == Some(w.raw.as_str()) {
                    let v = resolve_at(&cmd.cwd, w.value.clone());
                    words.push((w.span.clone(), Role::Argv, v));
                }
            }
            for r in &cmd.redirects {
                if let (RedirTarget::Path(v), Some(range)) = (&r.target, target_word(text, r)) {
                    words.push((range, Role::RedirectTarget, v.clone()));
                }
            }
        }
        Index {
            text: text.to_string(),
            words,
        }
    }

    fn lookup(&self, spelling: &str, role: Role) -> Option<Vec<String>> {
        if spelling.is_empty() {
            return None;
        }
        let mut out: Vec<String> = Vec::new();
        for (at, _) in self.text.char_indices() {
            if !self.text.get(at..).is_some_and(|t| t.starts_with(spelling)) {
                continue;
            }
            let range = at..at + spelling.len();
            let mut matched = false;
            for (r, word_role, v) in &self.words {
                if *r != range {
                    continue;
                }
                matched = true;
                if *word_role != role {
                    continue;
                }
                match v {
                    Value::Literal(p) if is_plain_path(p) => {
                        if !out.contains(p) {
                            out.push(p.clone());
                        }
                    }
                    _ => return None,
                }
            }
            if !matched {
                return None;
            }
        }
        (!out.is_empty()).then_some(out)
    }
}

/// The byte range of a file redirect's target word: its span minus the
/// leading fd digits, operator and blanks.
fn target_word(text: &str, r: &super::Redirect) -> Option<Range<usize>> {
    let spelled = text.get(r.span.clone())?;
    let word = spelled.trim_start_matches(|c: char| {
        c.is_ascii_digit() || matches!(c, '<' | '>' | '&' | '|' | ' ' | '\t')
    });
    (!word.is_empty()).then(|| r.span.end - word.len()..r.span.end)
}

fn is_plain_path(p: &str) -> bool {
    !p.is_empty()
        && p.chars().all(|c| {
            c.is_ascii_alphanumeric()
                || matches!(c, '/' | '.' | '_' | '+' | ',' | ':' | '@' | '%' | '^' | '-')
        })
}

/// Restores the previous value of a thread-local when dropped (also on
/// unwind), so a panic in a detector cannot leave a stale index, depth or
/// suspension behind for a later command.
pub(crate) struct Restore<T: Copy + 'static> {
    key: &'static std::thread::LocalKey<Cell<T>>,
    prev: T,
}

impl<T: Copy + 'static> Drop for Restore<T> {
    fn drop(&mut self) {
        self.key.with(|c| c.set(self.prev));
    }
}

/// The index state [`resolve`] reads.
enum Active {
    /// Outside [`with_index`].
    Inactive,
    /// [`analyze`] of the text was `Undetermined`: no word has a proven
    /// value, so every [`resolve`] keeps the caller's old verdict.
    Undetermined,
    /// [`analyze`] of the text was `Known`.
    Known(Rc<Index>),
}

struct RestoreIndex(Active);

impl Drop for RestoreIndex {
    fn drop(&mut self) {
        let prev = std::mem::replace(&mut self.0, Active::Inactive);
        INDEX.with(|i| *i.borrow_mut() = prev);
    }
}

/// Run `f` with the index of `text` active. When [`analyze`] of `text` is
/// `Undetermined`, the state inside `f` is [`Active::Undetermined`] (every
/// [`resolve`] is `None`, so every detector keeps its old verdict).
pub(crate) fn with_index<R>(text: &str, f: impl FnOnce() -> R) -> R {
    let index = match analyze(text) {
        Analysis::Known(script) => Active::Known(Rc::new(Index::build(text, &script))),
        Analysis::Undetermined(_) => Active::Undetermined,
    };
    let prev = INDEX.with(|i| std::mem::replace(&mut *i.borrow_mut(), index));
    let _restore = RestoreIndex(prev);
    f()
}

/// Record the shell depth `detect_bash` is analysing at, until the guard is
/// dropped.
pub(crate) fn enter_depth(depth: usize) -> Restore<usize> {
    let prev = DEPTH.with(|d| d.replace(depth));
    Restore { key: &DEPTH, prev }
}

/// Run `f` with [`resolve`] answering `None`.
pub(crate) fn suspended<R>(f: impl FnOnce() -> R) -> R {
    let prev = SUSPENDED.with(|s| s.replace(true));
    let _restore = Restore {
        key: &SUSPENDED,
        prev,
    };
    f()
}

/// The paths the top-level `spelling`, read in `role`, statically names (see
/// the module doc for every condition); `None` keeps the caller's old
/// verdict.
pub(crate) fn resolve(spelling: &str, role: Role) -> Option<Vec<String>> {
    if SUSPENDED.with(Cell::get) || DEPTH.with(Cell::get) != 0 {
        return None;
    }
    INDEX.with(|i| match &*i.borrow() {
        Active::Known(index) => index.lookup(spelling, role),
        // No proof of any value: the caller keeps its old verdict (the
        // restrictive side — `None` here never clears a target).
        Active::Inactive | Active::Undetermined => None,
    })
}
