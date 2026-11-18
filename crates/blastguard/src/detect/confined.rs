//! Confined-ask helpers, split out of `detect.rs` (behavior-neutral move for
//! the file-growth cap; no logic change).

use super::Ctx;
use crate::model::Decision;

/// True for the verbs whose relative operands [`rewrite_relative_operands`]
/// has already re-expressed against the session cwd.
///
/// Kept as its own list rather than derived from [`verb_targets`] at runtime
/// (that function needs a token slice to answer), and pinned to it by
/// `verb_rewrite_list_matches_verb_targets` in the test module below — if a verb
/// is added to `verb_targets` and not here, that test fails rather than this
/// module silently resolving its operands against the wrong base.
/// The verdict for a rule whose only objection is the SHAPE of the command:
/// an Ask when every target resolved strictly inside a safe root, otherwise the
/// Deny that rule always returned.
///
/// `verb` selects the base for relative operands (see [`Ctx::base_for`]);
/// `action` is what the Ask calls the operation.
pub(super) fn shape_deny_or_confined_ask<S: AsRef<str>>(
    ctx: &Ctx<'_>,
    verb: &str,
    action: &str,
    targets: &[S],
    deny_reason: &str,
) -> Decision {
    match ctx.confined_root(verb, targets) {
        Some(root) => confined_ask(action, &root, targets),
        None => Decision::deny(deny_reason),
    }
}

impl Decision {
    /// Second chance for a rule whose operand is NOT the thing destroyed
    /// (`chmod -R`, `chown -R`): if the shape-only Deny stands, retry the
    /// placement accepting a safe root itself.
    ///
    /// Written as a post-hoc widening rather than a flag on
    /// [`shape_deny_or_confined_ask`] so the narrow rule stays the default: a
    /// verb that must NOT accept its own root (every deleting verb) cannot pick
    /// this up by forgetting to pass `false`.
    pub(super) fn or_root_itself<S: AsRef<str>>(
        self,
        ctx: &Ctx<'_>,
        verb: &str,
        action: &str,
        targets: &[S],
    ) -> Decision {
        match self {
            Decision::Deny(reason) => match ctx.confined_root_or_root_itself(verb, targets) {
                Some(root) => confined_ask(action, &root, targets),
                None => Decision::Deny(reason),
            },
            other => other,
        }
    }
}

/// The `Ask` for a destructive command whose every target resolved inside a
/// safe root.
///
/// The wording is load-bearing in two ways: it names the ROOT (so the operator
/// can see what "confined" was measured against, rather than trusting the
/// word), and it carries the fixed phrase `is confined to`, which
/// [`crate::rule_id`] keys the `scoped-destructive-confined` id on.
pub(super) fn confined_ask<S: AsRef<str>>(action: &str, root: &str, targets: &[S]) -> Decision {
    Decision::ask(format!(
        "{action} is confined to {root} — a safe root for this session (its project tree, its \
worktree, or a temp dir), and every target ({}) resolved to a real path strictly inside it, \
symlinks included. The blast radius is bounded, so this is a question rather than a refusal: \
approve it if that is the tree you meant.",
        targets
            .iter()
            .map(|t| t.as_ref())
            .collect::<Vec<&str>>()
            .join(" ")
    ))
}
