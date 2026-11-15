//! Whether a row in a REPO store belongs to the store's repo (user ruling
//! 2026-11-14 「本質的というのは問題の質は通常パスでゆらぐものではない」, fix
//! d3f7a4af). In a repo store the store's repo is the identity of every row;
//! the row's `project` label only records where it was FILED, so a label naming
//! another machine's path is not evidence that the row is foreign.
//!
//! This is the one resolution shared by every site that asks "is this row the
//! same project as that one": the re-run location of `next --claim`
//! (`claim_recheck::rerun_dir`), the exact-duplicate guard of `add`
//! (`store::check_duplicate`) and the near-duplicate scan (`dedup`). Pinned
//! multi-project stores do not use it: there the label is the only separator
//! and stays the identity.

use super::canonical_project_id;
use harness_core::verdict::Determination;
use std::path::Path;

/// The answer to "does this row belong to the store's repo?". Three-valued on
/// purpose (CLAUDE.md §3): `Undetermined` is never read as either of the other
/// two; each caller resolves it to its own restrictive side and says so.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RowRepo {
    /// The store's repo: the label is absent here (filed on another machine),
    /// is not a path, or resolves to the store's repo.
    This,
    /// The label exists on this machine and resolves to a DIFFERENT repo. The
    /// string says which.
    Other(String),
    /// Existence or identity of the label (or of the store's repo) could not
    /// be determined. The string says why.
    Undetermined(String),
}

/// [`RowRepo`] for a row labelled `label` in the repo store whose root is
/// `store_root`.
///
/// The label can only DISPROVE membership, and only when it is an absolute
/// path that exists on this machine: one that resolves to a different repo is
/// [`RowRepo::Other`], one whose existence or identity cannot be determined is
/// [`RowRepo::Undetermined`]. An absent label or a non-path label is no
/// evidence against the store, so the row is [`RowRepo::This`]. When the
/// store's own repo identity cannot be determined, every row is
/// [`RowRepo::Undetermined`].
pub(crate) fn row_repo(label: &str, store_root: &Path) -> RowRepo {
    let store_id = match canonical_project_id(store_root) {
        Determination::Known(p) => p,
        Determination::Undetermined(why) => {
            return RowRepo::Undetermined(format!(
                "the project identity of this store's repo {} cannot be determined: {why}",
                store_root.display()
            ));
        }
    };
    let label = label.trim();
    let expanded = harness_core::config::expand_tilde(label);
    if !expanded.is_absolute() {
        return RowRepo::This;
    }
    match std::fs::symlink_metadata(&expanded) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => RowRepo::This,
        Err(e) => RowRepo::Undetermined(format!(
            "whether its filing location {label:?} exists on this machine could not be \
             determined, so whether it belongs to this store's repo {} cannot be \
             determined: {e}",
            store_id.display()
        )),
        Ok(_) => match canonical_project_id(&expanded) {
            Determination::Known(id) if id == store_id => RowRepo::This,
            Determination::Known(id) => RowRepo::Other(format!(
                "its filing location {label:?} exists on this machine and belongs to a \
                 DIFFERENT repo ({}) than this store's repo ({})",
                id.display(),
                store_id.display()
            )),
            Determination::Undetermined(why) => RowRepo::Undetermined(format!(
                "its filing location {label:?} exists on this machine but its project \
                 identity cannot be determined, so whether it belongs to this store's \
                 repo {} cannot be determined: {why}",
                store_id.display()
            )),
        },
    }
}
