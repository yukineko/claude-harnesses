// テスト内の unwrap/expect は意図的な assert であって fail-open ではないので許可する。
// production 側は workspace の [workspace.lints.clippy] で deny のまま。
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]
//! progeffect: the EFFECTS of an embedded program, read with a real parser
//! for its language.
//!
//! User rulings 2026-11-17 (blastguard S5): "embedded Python gets a real
//! Python parser and is judged with the same effect x target-path rules as
//! shell", and "parsers per language + the effect x target rules live in ONE
//! shared lib crate used by both blastguard (embedded programs) and the
//! future script-quarantine plugin".
//!
//! # What this crate decides, and what it does not
//!
//! It turns a program text into either
//!
//! * [`Analysis::Known`] — the complete list of [`Effect`]s the program may
//!   have (every call that can run a process or touch the file system,
//!   wherever it sits in the program, reachable or not), each with literal
//!   operands; or
//! * [`Analysis::Undetermined`] — the program does something whose effect
//!   cannot be read from its text (a parse error, dynamic dispatch, an
//!   operand that is not a literal, a module outside the trusted set, …).
//!   This is "cannot judge": a consumer resolves it to its restrictive side
//!   (blastguard: Ask in the main session, Deny in a subagent). It is never
//!   folded into "no effect".
//!
//! It does NOT place a path (system / project / scratch …): that is Table C
//! of blastguard's `SUBSET.md`, which needs the session's location model.
//! Instead every effect has a shell spelling ([`Effect::shell_spelling`]) —
//! the shell command with exactly that effect on exactly those operands — so a
//! host judges a program effect with the SAME rules it judges that shell
//! command with (no second rule table that could drift).
//!
//! Text vs code is decided by the parser, never by a heuristic (user ruling
//! 2026-11-17, memory `gate-text-vs-code-via-parser`): a string literal, a
//! comment, the literal parts of an f-string are DATA; only calls are
//! effects.

mod effect;
pub mod python;

pub use effect::{sq as shell_quote, Effect, FsOp};

/// The result of analysing one program.
#[derive(Debug, Clone, PartialEq, Eq)]
#[must_use]
pub enum Analysis {
    /// Every effect the program may have. Empty = no effect.
    Known(Vec<Effect>),
    /// Cannot judge; the string says why. Never treat as "no effect".
    Undetermined(String),
}
