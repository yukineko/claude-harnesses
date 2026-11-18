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
    /// Every evaluated simple command whose command word is the literal
    /// `rm` (see [`rm_operand_values`]): its argv after the command word as
    /// `(byte range, raw spelling, value resolved against the command's
    /// cwd)`, one entry per evaluated instance. `None` for a command one of
    /// whose argv words is not spelled verbatim in the text at its span.
    rm_commands: Vec<Option<RmArgv>>,
}

/// One `rm` command's argv after the command word: `(byte range, raw
/// spelling, value resolved against the command's cwd)` per word.
type RmArgv = Vec<(Range<usize>, String, Value)>;

/// The command words read as `rm` itself (not a wrapper, not a function).
fn is_rm_command_word(v: &Value) -> bool {
    matches!(v, Value::Literal(w) if matches!(w.as_str(), "rm" | "/bin/rm" | "/usr/bin/rm"))
}

impl Index {
    fn build(text: &str, script: &Script) -> Index {
        let mut words = Vec::new();
        let mut rm_commands = Vec::new();
        for cmd in &script.commands {
            if cmd
                .argv
                .first()
                .is_some_and(|w| is_rm_command_word(&w.value))
            {
                let argv: Option<Vec<_>> = cmd.argv[1..]
                    .iter()
                    .map(|w| {
                        (text.get(w.span.clone()) == Some(w.raw.as_str())).then(|| {
                            (
                                w.span.clone(),
                                w.raw.clone(),
                                resolve_at(&cmd.cwd, w.value.clone()),
                            )
                        })
                    })
                    .collect();
                rm_commands.push(argv);
            }
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
            rm_commands,
        }
    }

    /// See [`rm_operand_values`].
    fn rm_operands(&self, rest: &[&str]) -> Option<Vec<RmOperand>> {
        // The AST `rm` commands spelled as `rest`: each token is the word's
        // raw spelling, or — the legacy walk rewrites a relative operand
        // after a literal `cd` to its absolute path (`detect/cwd.rs`
        // `advance_cwd_and_rewrite`) — the absolute value the AST evaluator
        // resolved a relative raw spelling to. That value is absolute only
        // when the command's cwd is a Known literal (`resolve_at`: the hook's
        // cwd leaves it relative, an undetermined cwd makes it
        // `Undetermined`), so a rewritten token matches only when both
        // readings placed the operand at the same path.
        let token_matches = |w: &(Range<usize>, String, Value), r: &str| {
            w.1 == r
                || (r.starts_with('/')
                    && !w.1.starts_with('/')
                    && matches!(&w.2, Value::Literal(v) if v == r))
        };
        let matching: Vec<&RmArgv> = self
            .rm_commands
            .iter()
            .map(|c| c.as_ref())
            .collect::<Option<Vec<_>>>()?
            .into_iter()
            .filter(|argv| {
                argv.len() == rest.len() && argv.iter().zip(rest).all(|(w, r)| token_matches(w, r))
            })
            .collect();
        if matching.is_empty() {
            return None;
        }
        let mut out: Vec<RmOperand> = Vec::new();
        for (i, spelling) in rest.iter().enumerate() {
            if spelling.starts_with('-') || spelling.is_empty() {
                continue;
            }
            // Every occurrence of the operand's spelling in the text must be
            // this operand of one of the matching `rm` commands: an
            // occurrence anywhere else (another command's argv — `xargs rm
            // -r a` next to `rm -r a` —, a quoted string, a heredoc body, a
            // comment) may be the text the caller is reading, and that one
            // was not evaluated as this `rm`'s operand. The spellings checked
            // are the token itself and every matching command's raw word (the
            // pre-rewrite spelling of a rewritten token).
            let mut spellings: Vec<&str> = vec![spelling];
            for argv in &matching {
                if !spellings.contains(&argv[i].1.as_str()) {
                    spellings.push(argv[i].1.as_str());
                }
            }
            for spelled in spellings.iter().filter(|s| !s.is_empty()) {
                for (at, _) in self.text.char_indices() {
                    if !self.text.get(at..).is_some_and(|t| t.starts_with(spelled)) {
                        continue;
                    }
                    let range = at..at + spelled.len();
                    if !matching.iter().any(|argv| argv[i].0 == range) {
                        return None;
                    }
                }
            }
            for argv in &matching {
                out.push(RmOperand {
                    spelling: (*spelling).to_string(),
                    value: argv[i].2.clone(),
                });
            }
        }
        Some(out)
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

/// One evaluated operand of an `rm` command ([`rm_operand_values`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RmOperand {
    /// The operand as spelled in the text.
    pub spelling: String,
    /// Its evaluated value: absolute after a literal `cd`, as written when
    /// no `cd` applies (relative to the hook's cwd), `Undetermined`
    /// otherwise.
    pub value: Value,
}

/// The evaluated operands of the top-level `rm` command whose argv after the
/// command word is spelled exactly `rest` (the legacy reading's tokens), for
/// the new-layer `rm` judgement (`detect/rm/new_layer.rs`). One entry per
/// operand (a token not starting with `-`) per evaluated instance of every
/// such command; values are NOT filtered — an `Undetermined` value is handed
/// back for the caller to refuse.
///
/// `None` (the caller keeps the legacy verdict) unless all of these hold:
/// the index is active, `Known`, not [`suspended`], at shell depth 0; the
/// text has at least one evaluated simple command whose command word is the
/// literal `rm` (`/bin/rm`, `/usr/bin/rm`) and whose argv after it is spelled
/// exactly `rest` — a token may also be the absolute value the evaluator
/// resolved a relative word to after a literal `cd` (the legacy walk's
/// rewrite of that operand; only a Known cwd yields an absolute value); every
/// `rm` command in the text has its argv spelled verbatim at its spans; and
/// every occurrence of every operand spelling in the text (the token and the
/// matching word's raw spelling) is that operand's word of one of those `rm`
/// commands.
pub(crate) fn rm_operand_values(rest: &[&str]) -> Option<Vec<RmOperand>> {
    if SUSPENDED.with(Cell::get) || DEPTH.with(Cell::get) != 0 {
        return None;
    }
    INDEX.with(|i| match &*i.borrow() {
        Active::Known(index) => index.rm_operands(rest),
        Active::Inactive | Active::Undetermined => None,
    })
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
