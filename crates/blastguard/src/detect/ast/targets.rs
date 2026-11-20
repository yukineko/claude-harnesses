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
use super::judge::table_b::{extract, option_argument_words, Row};
use super::judge::Effect;
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
    /// A word of a simple command's argv, its value AS WRITTEN: the evaluated
    /// value before it is placed against the command's cwd (no
    /// `resolve_at`). The value a word holds as TEXT rather than as a path
    /// opened from the cwd — a symlink's source, which the link stores
    /// verbatim and which is resolved from the link's own directory
    /// (backlog 42d5fa32: `for f in /etc; do ln -st D $f; done`). A relative
    /// value stays relative here, so a caller that needs an absolute value
    /// sees it is not one instead of a cwd-placed path.
    ArgvAsWritten,
}

/// The evaluated words of one analysed text.
struct Index {
    text: String,
    /// `(byte range of the word in text, role, evaluated value)`; one entry
    /// per evaluated instance (an unrolled loop repeats a range).
    words: Vec<(Range<usize>, Role, Value)>,
    /// Every evaluated simple command whose command word is the literal `X`,
    /// `/bin/X` or `/usr/bin/X` for a name `X` with no `/` (see
    /// [`tied_operand_values`]), one entry per evaluated instance.
    commands: Vec<IndexedCommand>,
    /// `(byte range, literal value)` of every assignment value word
    /// (`F=<word>`, prefix or assignment-only) spelled verbatim at its span
    /// whose evaluated value is a `Literal` equal to that spelling (stage
    /// S6-6): the source a legacy `$F` rewrite copies, see [`Index::tie`].
    assignment_values: Vec<(Range<usize>, String)>,
}

/// One evaluated top-level command of [`Index::commands`].
struct IndexedCommand {
    /// `X` of the command word `X` / `/bin/X` / `/usr/bin/X`.
    name: String,
    /// Its argv after the command word as `(byte range, raw spelling, value
    /// resolved against the command's cwd)`; `None` when one of those words
    /// is not spelled verbatim in the text at its span.
    argv: Option<TiedArgv>,
    /// Its Table B B.1 row (`judge::table_b::extract`): target values as
    /// written, to be placed against [`IndexedCommand::cwd`].
    row: Row,
    /// Indexes into [`IndexedCommand::argv`] of the words a listed option of
    /// the B.1 row took as its separate argument
    /// (`judge::table_b::option_argument_words`, stage S6-4): not operand
    /// spellings, so [`Index::tie`] does not require them to tie. `None` (not
    /// determined): no word is skipped.
    option_args: Option<Vec<usize>>,
    /// The command's statically tracked cwd ([`super::SimpleCommand::cwd`]).
    cwd: Value,
}

/// One command's argv after the command word: `(byte range, raw spelling,
/// value resolved against the command's cwd)` per word.
type TiedArgv = Vec<(Range<usize>, String, Value)>;

/// The name `X` of a command word spelled as the literal `X`, `/bin/X` or
/// `/usr/bin/X` (`X` non-empty, no `/`). For `rm` these are exactly the
/// three spellings the `rm` tie has always accepted. The test is on the
/// word's value only (a wrapper such as `sudo rm` is the command `sudo`).
fn command_name(v: &Value) -> Option<&str> {
    let Value::Literal(w) = v else {
        return None;
    };
    let name = w
        .strip_prefix("/usr/bin/")
        .or_else(|| w.strip_prefix("/bin/"))
        .unwrap_or(w);
    (!name.is_empty() && !name.contains('/')).then_some(name)
}

/// The commands [`Index::tie`] tied to a legacy token list, and the operands
/// it hands back.
struct Tie<'a> {
    commands: Vec<&'a IndexedCommand>,
    operands: Vec<TiedOperand>,
}

impl Index {
    fn build(text: &str, script: &Script) -> Index {
        let mut words = Vec::new();
        let mut commands = Vec::new();
        let mut assignment_values = Vec::new();
        for cmd in &script.commands {
            for (_, w) in &cmd.assignments {
                if let Value::Literal(v) = &w.value {
                    if text.get(w.span.clone()) == Some(w.raw.as_str()) && *v == w.raw {
                        assignment_values.push((w.span.clone(), v.clone()));
                    }
                }
            }
            if let Some(name) = cmd.argv.first().and_then(|w| command_name(&w.value)) {
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
                commands.push(IndexedCommand {
                    name: name.to_string(),
                    argv,
                    row: extract(cmd),
                    option_args: option_argument_words(cmd),
                    cwd: cmd.cwd.clone(),
                });
            }
            for w in &cmd.argv {
                if text.get(w.span.clone()) == Some(w.raw.as_str()) {
                    let v = resolve_at(&cmd.cwd, w.value.clone());
                    words.push((w.span.clone(), Role::Argv, v));
                    words.push((w.span.clone(), Role::ArgvAsWritten, w.value.clone()));
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
            commands,
            assignment_values,
        }
    }

    /// See [`tied_operand_values`].
    fn tie(&self, name: &str, rest: &[&str]) -> Option<Tie<'_>> {
        // The AST `name` commands spelled as `rest`: each token is the word's
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
        // Every `name` command must have its argv spelled verbatim (the `?`):
        // one that is not may be the command the legacy tokens read.
        let matching: Vec<(&IndexedCommand, &TiedArgv)> = self
            .commands
            .iter()
            .filter(|c| c.name == name)
            .map(|c| c.argv.as_ref().map(|argv| (c, argv)))
            .collect::<Option<Vec<_>>>()?
            .into_iter()
            .filter(|(_, argv)| {
                argv.len() == rest.len() && argv.iter().zip(rest).all(|(w, r)| token_matches(w, r))
            })
            .collect();
        if matching.is_empty() {
            return None;
        }
        let mut out: Vec<TiedOperand> = Vec::new();
        for (i, spelling) in rest.iter().enumerate() {
            if spelling.starts_with('-') || spelling.is_empty() {
                continue;
            }
            // Stage S6-4: the separate argument of a listed option (`-s 0`)
            // is not an operand spelling — `0` inside a path elsewhere in
            // the text is not a second reading of it. Skipped only when
            // EVERY matching command's B.1 parse took it as such (they all
            // spell `rest`, so they agree unless a row is undetermined).
            if matching
                .iter()
                .all(|(c, _)| c.option_args.as_ref().is_some_and(|a| a.contains(&i)))
            {
                continue;
            }
            // Every occurrence of the operand's spelling in the text must be
            // this operand of one of the matching commands: an occurrence
            // anywhere else (another command's argv — `xargs rm -r a` next
            // to `rm -r a` —, a quoted string, a heredoc body, a comment) may
            // be the text the caller is reading, and that one was not
            // evaluated as this command's operand. The spellings checked are
            // the token itself and every matching command's raw word (the
            // pre-rewrite spelling of a rewritten token).
            let mut spellings: Vec<&str> = vec![spelling];
            for (_, argv) in &matching {
                if !spellings.contains(&argv[i].1.as_str()) {
                    spellings.push(argv[i].1.as_str());
                }
            }
            //
            // Stage S6-6: one more occurrence is not a second reading — the
            // whole value word of an assignment (`F=<spelled>`, unquoted, a
            // Literal equal to its spelling) when each matching command's
            // operand evaluated to exactly that Literal. The text analysed
            // here is the line after the legacy walk substituted a same-line
            // literal assignment into `$F` / `"$F"` (`detect/vars.rs`;
            // measured: `F=/t/f; tee "$F"` reaches the index as
            // `F=/t/f; tee /t/f`), so the assigned value is spelled twice;
            // the assignment word is data, not a command operand, and no
            // other command's reading of it can be this command's argv.
            //
            // `tee` ONLY (S6-6 retires `tee` alone): for the other rows the
            // same shape keeps its pinned legacy verdict
            // (`F=<Scratch>; truncate -s 0 $F` ask/deny,
            // tests/s6_4_truncate_retire_20261120.rs `control_non_literal`)
            // until a ruling extends it.
            let assigned_here = |range: &Range<usize>, spelled: &str| {
                name == "tee"
                    && self
                        .assignment_values
                        .iter()
                        .any(|(r, v)| r == range && v == spelled)
                    && matching
                        .iter()
                        .all(|(_, argv)| matches!(&argv[i].2, Value::Literal(v) if v == spelled))
            };
            for spelled in spellings.iter().filter(|s| !s.is_empty()) {
                for (at, _) in self.text.char_indices() {
                    if !self.text.get(at..).is_some_and(|t| t.starts_with(spelled)) {
                        continue;
                    }
                    let range = at..at + spelled.len();
                    if !matching.iter().any(|(_, argv)| argv[i].0 == range)
                        && !assigned_here(&range, spelled)
                    {
                        return None;
                    }
                }
            }
            for (_, argv) in &matching {
                out.push(TiedOperand {
                    spelling: (*spelling).to_string(),
                    value: argv[i].2.clone(),
                });
            }
        }
        Some(Tie {
            commands: matching.into_iter().map(|(c, _)| c).collect(),
            operands: out,
        })
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

/// One evaluated operand of a tied command ([`tied_operand_values`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TiedOperand {
    /// The operand as spelled in the text.
    pub spelling: String,
    /// Its evaluated value: absolute after a literal `cd`, as written when
    /// no `cd` applies (relative to the hook's cwd), `Undetermined`
    /// otherwise.
    pub value: Value,
}

/// The evaluated operands of the top-level command `name` whose argv after
/// the command word is spelled exactly `rest` (the legacy reading's tokens),
/// for a new-layer judgement (`detect/rm/new_layer.rs`,
/// `detect/known_allow.rs`). One entry per operand (a token not starting
/// with `-` and not the separate argument of a listed option of the
/// command's B.1 row, `judge::table_b::option_argument_words`; `rm` has no
/// such option) per evaluated instance of every such command; values are NOT
/// filtered — an `Undetermined` value is handed back for the caller to
/// refuse.
///
/// `None` (the caller keeps the legacy verdict) unless all of these hold:
/// the index is active, `Known`, not [`suspended`], at shell depth 0; the
/// text has at least one evaluated simple command whose command word is the
/// literal `name` (`/bin/name`, `/usr/bin/name`) and whose argv after it is
/// spelled exactly `rest` — a token may also be the absolute value the
/// evaluator resolved a relative word to after a literal `cd` (the legacy
/// walk's rewrite of that operand; only a Known cwd yields an absolute
/// value); every `name` command in the text has its argv spelled verbatim at
/// its spans; and every occurrence of every operand spelling (option
/// arguments excluded, as above) in the text
/// (the token and the matching word's raw spelling) is that operand's word
/// of one of those commands — or, for `name == "tee"` only (stage S6-6), the
/// whole unquoted value word of an assignment whose Literal value is that
/// spelling, when every such operand evaluated to that same Literal.
pub(crate) fn tied_operand_values(name: &str, rest: &[&str]) -> Option<Vec<TiedOperand>> {
    with_known(|index| index.tie(name, rest).map(|t| t.operands))
}

/// One Table B B.1 target of a tied command ([`tied_b1_targets`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TiedTarget {
    pub effect: Effect,
    /// The target word's value as written (not yet placed).
    pub target: Value,
    /// The cwd the command runs in ([`super::SimpleCommand::cwd`]).
    pub cwd: Value,
}

/// Every (effect, target) pair of the Table B B.1 rows
/// (`judge::table_b::extract`) of the commands [`tied_operand_values`] ties
/// to `name` + `rest`, over every tied instance. `None` (not Known: the
/// caller keeps its legacy verdict) when nothing is tied (every condition of
/// [`tied_operand_values`]), or a tied command has no B.1 row or an
/// undetermined one. The list may be empty (a row naming no target); what
/// that means is the caller's to decide.
pub(crate) fn tied_b1_targets(name: &str, rest: &[&str]) -> Option<Vec<TiedTarget>> {
    with_known(|index| {
        let tie = index.tie(name, rest)?;
        let mut out = Vec::new();
        for c in tie.commands {
            let Row::Targets(pairs) = &c.row else {
                return None;
            };
            out.extend(pairs.iter().map(|(effect, target)| TiedTarget {
                effect: *effect,
                target: target.clone(),
                cwd: c.cwd.clone(),
            }));
        }
        Some(out)
    })
}

/// `f` of the active `Known` index, at shell depth 0 and not [`suspended`];
/// `None` otherwise (the caller keeps its old verdict).
fn with_known<R>(f: impl FnOnce(&Index) -> Option<R>) -> Option<R> {
    if SUSPENDED.with(Cell::get) || DEPTH.with(Cell::get) != 0 {
        return None;
    }
    INDEX.with(|i| match &*i.borrow() {
        Active::Known(index) => f(index),
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
