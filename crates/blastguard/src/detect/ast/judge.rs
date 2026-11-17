//! EFFECT x TARGET-PATH judgement over an analysed [`Script`] (task S1
//! `subset-spec`, user rulings 2026-11-17).
//!
//! The specification is `SUBSET.md` next to this file: Table A (the
//! supported node / expansion kinds — everything else is
//! `Cause::Unsupported`), Table B (which effect each modelled command has on
//! which target words), Table C ([`PathClass`] x [`Effect`] -> verdict). This
//! file freezes the types and the signatures only.
//!
//! # Current state (S1)
//!
//! Nothing calls [`judge`] or [`classify`]: they are NOT wired into `detect`
//! and change no verdict. Both are stubs that never panic and answer the
//! restrictive "cannot judge" value ([`Judgement::Undetermined`] with
//! [`Cause::NotImplemented`], [`PathClass::Unresolved`]). Stage S3
//! (`path-cutover`) implements Tables A–C for the path rows and wires
//! [`judge`] in; S4 / S5 add the program-source rows; S6 removes the string
//! readings (see "Stage map" in `SUBSET.md`).
//!
//! # Contract (holds from the first caller on)
//!
//! * [`Judgement::Undetermined`] means "cannot judge" and is resolved by the
//!   caller to Ask (main session) / Deny (subagent, via
//!   [`crate::interactive::ask_available`]); it is never folded into Allow.
//!   On that path the legacy string harm detection may only RAISE the verdict
//!   (Ask -> Deny), never lower it ("Undetermined handling" in `SUBSET.md`).
//! * A target that does not resolve to a literal path is
//!   [`PathClass::Unresolved`], which Table C answers restrictively.
//! * A command with no effect model in Table B (an unknown program) has no
//!   finding: it contributes Allow. Its redirects are still judged (they are
//!   the shell's effect, not the program's). An undetermined command word is
//!   [`Judgement::Undetermined`], not an unknown program.
//! * [`PathClass`] is computed ONLY from predicates that already exist (listed
//!   on each variant); this module adds no path predicate.

use std::ops::Range;

use super::{Cause, Script, Value};
use crate::scope::SafeRoots;

/// What a modelled command (Table B) does to one target path.
#[allow(
    dead_code,
    reason = "frozen in S1; constructed by S3 (path-cutover), which wires `judge` into detect"
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Effect {
    /// The entry is removed (`rm`, `rmdir`, `unlink`, `find -delete`, a `mv`
    /// source, `git clean`). `recursive` = a whole tree may go (`rm -r`,
    /// `find -delete`, `git clean -d`, a `mv` source that may be a directory).
    Delete { recursive: bool },
    /// The content is replaced from empty: `>` / `>|` / `&>`, `tee`,
    /// `truncate`, `dd of=` (without `conv=notrunc`), the `cp` / `install` /
    /// `mv` destination, `find -fprint*` files.
    Truncate,
    /// Bytes are added at the end: `>>` / `&>>`, `tee -a`.
    Append,
    /// The content is edited in place: `sed -i`, `<>`, `dd conv=notrunc`.
    Rewrite,
    /// Mode / owner / group changes: `chmod`, `chown`, `chgrp`.
    Metadata,
    /// A link name is created or replaced: `ln` (the link, not its target).
    Link,
    /// A new directory tree is created: `cp -r` destination (the existing
    /// scratch-place rule, `detect/scratch_place.rs`).
    CreateTree,
    /// Uncommitted work-tree content may be discarded: `git reset --hard`,
    /// `git checkout -- <paths>` / `git checkout .`, `git restore` (work tree).
    GitDiscard,
    /// Every change under the target is staged: `git add -A` / `--all` / `.`.
    GitStage,
}

/// Where one target path lands. Table C is indexed by this x [`Effect`].
///
/// Precedence when several apply (first wins): `System`, `Protected`,
/// `Workflow`, `Persistence`, `Scratch`, `OwnWorktree`, `OtherWorktree`,
/// `Project`, `Other`.
/// `Unresolved` is exclusive (no predicate could be consulted).
#[allow(
    dead_code,
    reason = "frozen in S1; constructed by S3 (path-cutover) in `classify`"
)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PathClass {
    /// System harm: `harms_system` in `detect/harm_mark.rs` — `/`,
    /// [`crate::scope::is_inside_system_dir`], a raw disk device
    /// (`is_raw_disk`), the home directory itself,
    /// [`SafeRoots::irreplaceable_home_asset`] — on the lexical and the
    /// symlink-resolved spelling; plus `real_system_path_block`'s landing
    /// (`detect/protected.rs`). A system path strictly inside a safe root
    /// (`Ctx::confined_root`, a project under `/opt`) is not `System`.
    System,
    /// A gate / hook / policy path: [`crate::exclude::is_protected_path`]
    /// (and `exclude::touches_protected` where the verb is not modelled),
    /// lexical and through `protected::physical_landing`. Lifted (falls
    /// through to the next class) strictly inside a linked worktree checkout:
    /// [`SafeRoots::classify_worktree_checkout`] `Inside`.
    Protected,
    /// A GitHub Actions workflow file: [`crate::exclude::is_workflow_path`]
    /// (CLAUDE.md §7).
    Workflow,
    /// A NEW file in a per-user `Library/LaunchAgents`:
    /// `launch_agents_new_file_ask` (`detect/protected.rs`).
    Persistence,
    /// Every temp and cache deletion root (`/tmp`, `/private/tmp`, `$TMPDIR`,
    /// `$HOME/.cache`, `$HOME/Library/Caches`; the session scratchpad lives
    /// under `/private/tmp`): [`SafeRoots::classify_disposable`] `Inside`
    /// (strictly inside; `IsRoot` is not `Scratch`). User ruling 2026-11-17.
    Scratch,
    /// Inside the git checkout that contains the session cwd (user ruling
    /// 2026-11-17): the target is [`SafeRoots::classify_worktree_checkout`]
    /// `Inside { root: R }` and [`SafeRoots::session_cwd`] is `R` or inside
    /// `R` by the same predicate. Any `Undetermined` is not `OwnWorktree`.
    OwnWorktree,
    /// Strictly inside a worktree storage root (`$HOME/.condukt/worktrees`,
    /// `.harness-worktrees`; [`SafeRoots::classify_worktree`] `Inside`) but
    /// not `OwnWorktree`: today's verdict is kept (legacy, confined Ask).
    OtherWorktree,
    /// Inside a session safe root (payload cwd / `CLAUDE_PROJECT_DIR`):
    /// [`SafeRoots::classify`] `Inside` / `IsRoot`; judged by the existing
    /// recoverability rules (`crate::deletion::judge_rm`, `crate::reversible`).
    Project,
    /// Resolved and none of the above (`Placement::Outside`).
    Other,
    /// The target could not be placed: an `Undetermined` value, a relative
    /// path with no known base, a `..` component, a predicate answering
    /// `Undetermined`. Always restrictive in Table C.
    Unresolved(String),
}

/// One judged (effect, target) pair.
#[allow(
    dead_code,
    reason = "frozen in S1; constructed by S3 (path-cutover) in `judge`"
)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Finding {
    pub effect: Effect,
    /// The target as evaluated, relative paths represented as in
    /// [`super::RedirTarget::Path`].
    pub target: Value,
    pub class: PathClass,
    /// Byte span of the simple command (or redirect) in the analysed text.
    pub span: Range<usize>,
    pub reason: String,
}

/// The verdict of [`judge`]: the MAIN-session column of Table C. The
/// subagent column is the same with every Ask hardened to Deny by
/// [`crate::interactive::ask_available`]; no cell needs another rule.
#[allow(
    dead_code,
    reason = "frozen in S1; `Allow` / `Ask` / `Deny` are produced by S3 (path-cutover)"
)]
#[derive(Debug, Clone, PartialEq, Eq)]
#[must_use]
pub(crate) enum Judgement {
    /// Every finding's cell is Allow, or no command has an effect model.
    Allow,
    /// At least one Ask cell and no Deny cell.
    Ask(Vec<Finding>),
    /// At least one Deny cell. `system_harm` when one of them is
    /// [`PathClass::System`] (emitted through `Decision::deny_system_harm`).
    Deny {
        findings: Vec<Finding>,
        system_harm: bool,
    },
    /// Cannot judge: outside Table A (`Cause::Unsupported`), an undetermined
    /// command word, a modelled command with an unmodelled option, a parse
    /// stage cause, or (S1 only) `Cause::NotImplemented`. Never Allow.
    Undetermined(Cause),
}

/// What [`classify`] places paths against: the session's location model and
/// the base for a relative literal when no `cd` applies (the hook's cwd,
/// `Ctx::raw_base` at the start of a line).
#[allow(
    dead_code,
    reason = "frozen in S1; built by S3 (path-cutover) in detect"
)]
pub(crate) struct PathCtx<'a> {
    pub scope: &'a SafeRoots,
    pub hook_cwd: Option<&'a str>,
}

/// Judge `script` by Tables B and C (see `SUBSET.md`).
///
/// S1 stub: not wired into `detect`; never panics; always
/// `Undetermined(NotImplemented)` (the restrictive answer).
#[allow(
    dead_code,
    reason = "frozen in S1; implemented and wired into detect by S3 (path-cutover)"
)]
pub(crate) fn judge(script: &Script, ctx: &PathCtx) -> Judgement {
    let _ = (script, ctx);
    Judgement::Undetermined(Cause::NotImplemented)
}

/// The [`PathClass`] of `target` under `effect` (some classes depend on the
/// effect: `Persistence` is for new files only, `IsRoot` is acceptable only
/// for a search root). `cwd` is the command's [`super::SimpleCommand::cwd`].
///
/// S1 stub: always [`PathClass::Unresolved`] (the restrictive answer).
#[allow(
    dead_code,
    reason = "frozen in S1; implemented by S3 (path-cutover) from the predicates named on `PathClass`"
)]
pub(crate) fn classify(effect: Effect, target: &Value, cwd: &Value, ctx: &PathCtx) -> PathClass {
    let _ = (effect, target, cwd, ctx);
    PathClass::Unresolved("PathClass classification is implemented in stage S3".into())
}
