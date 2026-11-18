//! The new-layer (`detect::ast`, SUBSET.md Tables B/C) judgement of one `rm`
//! command, used to RETIRE the legacy `rm` string reading for the rows the new
//! layer decides (user ruling 2026-11-18, "ruling 14 replaced per rule": when
//! the new layer's verdict for an `rm` is Known, the legacy `rm` reading may
//! not raise it; when it is Undetermined, the legacy reading stands as before).
//!
//! [`known_allow`] is `true` ONLY when every one of these holds — anything
//! else is "not Known" and [`super::analyze_rm`] keeps its legacy verdict
//! unchanged (the restrictive side; this function never answers Ask / Deny
//! and never lowers a verdict it did not decide):
//!
//! * every option word is one Table B models for `rm` (`-r -R -f -i -d -v`
//!   clusters, `--recursive --force --dir --verbose`); `--` and any other
//!   option are not modelled;
//! * the AST evaluator ([`super::super::ast::targets::rm_operand_values`]) ties
//!   the legacy tokens to evaluated top-level `rm` commands (command word the
//!   literal `rm`, argv spelled exactly as the tokens, every occurrence of
//!   every operand spelling being that operand of such a command), and there
//!   is at least one operand;
//! * every evaluated operand value is a `Literal`, absolute or (no `cd`
//!   applies) relative to the session cwd, with no `..` component and not
//!   ending in a `.` component;
//! * every target's Table C class is **Scratch** or **OwnWorktree** — the
//!   two Delete cells Table C states as Allow (rulings 5, 8, 9; for Scratch
//!   confirmed by user ruling 2026-11-18 "仕様どおり Allow", so a quoted
//!   in-scratch literal gets the verdict of its unquoted spelling). Classes
//!   are tried in Table C precedence and every earlier class refuses:
//!   **System** (`/`, a system directory, a raw disk, the home itself, an
//!   irreplaceable home asset — lexical, resolved and parent-resolved
//!   spellings; no safe-root escape here), **Protected**
//!   (`exclude::is_protected_path` / `holds_protected_paths` /
//!   `touches_protected`, a protected glob, a `.git` entry — NOT lifted inside
//!   a worktree checkout here, so those stay legacy), **Workflow**. A class
//!   whose Table C cell is `legacy` (OtherWorktree, Project, Other) or
//!   `Unresolved` is not Known for this purpose.
//!
//! The path values are placed with the VALUE reading of the scope predicates
//! (`scope/reading.rs`): `"~/x"` / `./~/x` is a directory named `~` under the
//! cwd, as the shell hands it to `rm`.

use harness_core::verdict::Determination;

use super::super::ast::targets::{resolve, rm_operand_values, RmOperand, Role};
use super::super::ast::Value;
use super::{exclude, Ctx, Placement};

/// See the module doc.
pub(super) fn known_allow(rest: &[&str], ctx: &Ctx<'_>) -> bool {
    if !rest
        .iter()
        .filter(|t| t.starts_with('-'))
        .all(|t| modelled_option(t))
    {
        return false;
    }
    let Some(operands) = rm_operand_values(rest) else {
        return false;
    };
    !operands.is_empty() && operands.iter().all(|o| operand_allows(o, ctx))
}

fn modelled_option(t: &str) -> bool {
    match t.strip_prefix("--") {
        Some(long) => matches!(long, "recursive" | "force" | "dir" | "verbose"),
        None => {
            let cluster = t.strip_prefix('-').unwrap_or("");
            !cluster.is_empty()
                && cluster
                    .chars()
                    .all(|c| matches!(c, 'r' | 'R' | 'f' | 'i' | 'd' | 'v'))
        }
    }
}

/// The Table C Delete cell of the `rsync --delete` destination word `tok`
/// (user ruling 2026-11-18: judged like an `rm -rf` operand) is Allow: every
/// path it names — a plain literal word, or an expansion the AST evaluator
/// resolves ([`resolve`]) — is Scratch / OwnWorktree by [`value_allows`].
/// Anything else (a quote, an escape, a glob, an unresolved expansion, a
/// relative path after a `cd` the analysis could not resolve) is not Known,
/// and the caller keeps the legacy `rm` reading. A relative value is placed
/// against this segment's base (`Ctx::raw_base`: the session cwd, or the
/// directory of a resolved `cd`).
pub(super) fn rsync_dest_allows(tok: &str, ctx: &Ctx<'_>) -> bool {
    let values = if super::has_unresolvable_expansion(tok) {
        match resolve(tok, Role::Argv) {
            Some(v) => v,
            None => return false,
        }
    } else if tok.contains(['\'', '"', '\\', '*', '?', '[', ']', '{', '}', '~', '$', '`']) {
        return false;
    } else {
        vec![tok.to_string()]
    };
    let absolute = |v: &String| -> Option<String> {
        if v.starts_with('/') {
            return Some(v.clone());
        }
        let base = ctx.raw_base.as_deref().filter(|b| b.starts_with('/'))?;
        Some(format!("{}/{v}", base.trim_end_matches('/')))
    };
    !values.is_empty()
        && values
            .iter()
            .all(|v| absolute(v).is_some_and(|a| value_allows(&a, ctx)))
}

fn operand_allows(o: &RmOperand, ctx: &Ctx<'_>) -> bool {
    let Value::Literal(v) = &o.value else {
        return false;
    };
    value_allows(v, ctx)
}

fn value_allows(v: &str, ctx: &Ctx<'_>) -> bool {
    if v.is_empty() {
        return false;
    }
    let path = if v.starts_with('/') {
        v.to_string()
    } else {
        let Some(cwd) = ctx.scope.session_cwd() else {
            return false;
        };
        format!("{}/{v}", cwd.trim_end_matches('/'))
    };
    let comps: Vec<&str> = path.split('/').filter(|c| !c.is_empty()).collect();
    if comps.contains(&"..") || comps.last().is_none_or(|c| *c == ".") {
        return false;
    }
    if is_system(&path, ctx) || is_protected(&path) || exclude::is_workflow_path(&path) {
        return false;
    }
    scratch(&path, ctx) || own_worktree(&path, ctx)
}

/// Table C **Scratch**: strictly inside a temp / cache deletion root
/// (rulings 5, 8; user ruling 2026-11-18 "仕様どおり Allow").
fn scratch(path: &str, ctx: &Ctx<'_>) -> bool {
    matches!(
        ctx.scope.classify_disposable_value(path),
        Determination::Known(Placement::Inside { .. })
    )
}

/// Table C **System** (restrictive superset: no safe-root escape).
fn is_system(path: &str, ctx: &Ctx<'_>) -> bool {
    let mut candidates = vec![path.to_string()];
    if let Some(real) = ctx.scope.resolve(path) {
        candidates.push(real);
    }
    if let Some((parent, name)) = path.rsplit_once('/') {
        let parent = if parent.is_empty() { "/" } else { parent };
        if let Some(real_parent) = ctx.scope.resolve(parent) {
            candidates.push(format!("{}/{name}", real_parent.trim_end_matches('/')));
        }
    }
    let home = ctx.scope.home_real().map(|h| h.trim_end_matches('/'));
    let system = |p: &str| {
        let p = exclude::normalize(p);
        let p = p.trim_end_matches('/');
        p.is_empty()
            || crate::scope::is_inside_system_dir(p)
            || p == "/dev"
            || p.starts_with("/dev/")
            || home.is_some_and(|h| p == h)
    };
    if candidates.iter().any(|p| system(p)) {
        return true;
    }
    // An asset that cannot be placed is refused like one that is placed.
    !matches!(
        ctx.scope.irreplaceable_home_asset_value(path),
        Determination::Known(None)
    )
}

/// Table C **Protected** (not lifted inside a worktree checkout here).
fn is_protected(path: &str) -> bool {
    exclude::is_protected_path(path)
        || exclude::holds_protected_paths(path)
        || exclude::touches_protected(path)
        || super::super::protected::protected_glob_deny("rm", path).is_some()
        || path.split('/').any(|c| c == ".git")
}

/// Table C **OwnWorktree**: strictly inside the linked worktree checkout that
/// contains the session cwd (the cwd is that checkout or inside it).
fn own_worktree(path: &str, ctx: &Ctx<'_>) -> bool {
    let Determination::Known(Placement::Inside { root, .. }) =
        ctx.scope.classify_worktree_checkout_value(path)
    else {
        return false;
    };
    let Some(cwd) = ctx.scope.session_cwd() else {
        return false;
    };
    match ctx.scope.classify_worktree_checkout_value(cwd) {
        Determination::Known(Placement::Inside { root: r, .. })
        | Determination::Known(Placement::IsRoot { root: r }) => r == root,
        _ => false,
    }
}
