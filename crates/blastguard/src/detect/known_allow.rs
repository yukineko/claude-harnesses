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
//!   Allow. Every other class, `Unresolved` included, is `false`;
//! * no target names command-running configuration by the UNLIFTED
//!   predicates ([`super::ast::names_command_running_config`], stage S6-4,
//!   user ruling 2026-11-20 「Deny のまま（推奨）」): `classify` lifts Protected
//!   strictly inside a linked worktree checkout, so without this check an
//!   own-worktree `.claude/settings.json`, `.githooks/…`, `crates/*/hooks/…`
//!   or `hooks/hooks.json` would be OwnWorktree and Allow. For those the
//!   legacy verdict stays.
//!
//! The tie does not read the separate argument of a listed option
//! (`truncate -s 0`: the `0`) as an operand spelling (stage S6-4,
//! `judge::table_b::option_argument_words`).
//!
//! Callers (each a retirement step; a `false` keeps that arm's legacy
//! verdict): S6-4 — the legacy `truncate` arm in `detect.rs`
//! (`known_allow("truncate", rest, ctx)` → Allow).
//! `rm` and `rsync --delete` keep `rm/new_layer.rs` (their own option gate
//! and their own Scratch / OwnWorktree predicate; see SUBSET.md "S6-3
//! status" for why the predicate is not this one yet).

use super::ast::targets::tied_b1_targets;
use super::ast::{classify, names_command_running_config, PathClass, PathCtx, Value};
use super::Ctx;

/// See the module doc.
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
                && !names_command_running_config(&t.target, &t.cwd, &pctx)
        })
}
