//! Stage S6-3 (user ruling 2026-11-20, parity only): the generic new-layer
//! "Known Allow" of one top-level command the legacy string reading is about
//! to judge — the form of `rm/new_layer.rs::known_allow` for every Table B
//! B.1 row, which the S6 retirement steps (S6-4 onward) route their command
//! through.
//!
//! [`known_allow`] is `true` ONLY when every one of these holds; anything
//! else is `false` = "not Known", and the caller keeps its legacy verdict
//! unchanged (this function never answers Ask / Deny and never lowers a
//! verdict it did not decide):
//!
//! * the AST evaluator ties `cmd` + `rest` to evaluated top-level commands
//!   ([`super::ast::targets::tied_b1_targets`]: command word the literal
//!   `cmd`, `/bin/cmd` or `/usr/bin/cmd`; argv spelled as the legacy tokens;
//!   every occurrence of every operand spelling is that operand of such a
//!   command);
//! * every tied command has a B.1 row with targets (no row, or an
//!   `Undetermined` row, is `false`) and together they name at least one
//!   target;
//! * every target value is a `Literal`;
//! * every target's Table C class ([`super::ast::classify`], S6-2; a
//!   relative value is placed against [`crate::scope::SafeRoots::session_cwd`])
//!   is **Scratch** or **OwnWorktree** — the two Table C cells stated as
//!   Allow. Every other class, `Unresolved` included, is `false`.
//!
//! NOT wired in S6-3: no detector calls it yet, so it changes no verdict.
//! `rm` and `rsync --delete` keep `rm/new_layer.rs` (their own option gate
//! and their own Scratch / OwnWorktree predicate; see SUBSET.md "S6-3
//! status" for why the predicate is not this one yet).

use super::ast::targets::tied_b1_targets;
use super::ast::{classify, PathClass, PathCtx, Value};
use super::Ctx;

/// See the module doc.
#[allow(
    dead_code,
    reason = "S6-3: parity only (user ruling 2026-11-20); routed by the S6-4.. retirement steps"
)]
pub(super) fn known_allow(cmd: &str, rest: &[&str], ctx: &Ctx<'_>) -> bool {
    let Some(targets) = tied_b1_targets(cmd, rest) else {
        return false;
    };
    let pctx = PathCtx {
        scope: ctx.scope,
        hook_cwd: ctx.scope.session_cwd(),
        legacy: None,
    };
    !targets.is_empty()
        && targets.iter().all(|t| {
            matches!(t.target, Value::Literal(_))
                && matches!(
                    classify(t.effect, &t.target, &t.cwd, &pctx),
                    PathClass::Scratch | PathClass::OwnWorktree
                )
        })
}
