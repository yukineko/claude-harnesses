//! Linked worktree checkout placement for [`SafeRoots`]: the checkout probe
//! and the two classifications built on it. Split out of `scope.rs` (file
//! growth cap); see the [`crate::scope`] module doc, sections
//! "Protected-named paths inside a linked worktree checkout" and "Git
//! working-tree discards inside a linked worktree checkout".

use harness_core::verdict::Determination;

use super::{Placement, SafeRoots};

impl SafeRoots {
    /// Is `operand` STRICTLY INSIDE a linked git worktree checkout that sits
    /// directly under a worktree storage root (`<root>/<checkout>/<more>`)?
    ///
    /// `Known(Inside { root: <checkout>, path })` is the only answer that may
    /// lift the protected-path rule (module doc, "Protected-named paths inside
    /// a linked worktree checkout"). Everything else — the checkout itself
    /// (`IsRoot`), a path directly under a storage root or outside every root
    /// (`Outside`), and every `Undetermined` — leaves the caller's verdict
    /// exactly as it was. All of these are required:
    ///
    ///   * the session is not refused (raw cwd / `CLAUDE_PROJECT_DIR`), and a
    ///     checkout probe exists (only with a resolver);
    ///   * the operand has no `..` component as SPELLED — the same refusal the
    ///     worktree rm `Allow` makes, because `..` is collapsed lexically
    ///     before symlinks are resolved;
    ///   * [`SafeRoots::classify_worktree`] places it `Inside` a storage root,
    ///     at least two components deep;
    ///   * no component below `<root>/<checkout>` is `.git` (a nested repo's
    ///     own hooks/config are not this checkout's tracked files);
    ///   * `<root>/<checkout>` is a real directory (not a symlink) holding a
    ///     `.git` FILE whose content starts with `gitdir:` — what `git worktree
    ///     add` writes; a main checkout has a `.git` DIRECTORY and never
    ///     qualifies;
    ///   * the operand's own spelling passes through the worktree STORAGE
    ///     ROOT with no symlink below it ([`SafeRoots::no_symlink_below`]
    ///     anchored at the storage root, not at the checkout), so a
    ///     `<checkout>/.githooks` symlinked into the main tree, a checkout
    ///     entry that is itself a symlink (`<root>/<link> -> <root>/<checkout>`),
    ///     and a spelling through a symlink outside every storage root that
    ///     lands in a checkout are never `Inside`.
    pub fn classify_worktree_checkout(
        &self,
        operand: &str,
        cwd: Option<&str>,
    ) -> Determination<Placement> {
        if let Some(why) = &self.worktree_refusal {
            return Determination::undetermined(why.clone());
        }
        let Some(probe) = self.checkout_probe else {
            return Determination::undetermined(
                "blastguard has no worktree checkout probe for this session (no resolver)",
            );
        };
        if operand.split('/').any(|c| c == "..") {
            return Determination::undetermined(
                "operand has a `..` component, so where it lands is not decided by its spelling",
            );
        }
        let lexical = match self.lexical_absolute(operand, cwd) {
            Determination::Known(l) => l,
            Determination::Undetermined(u) => return Determination::Undetermined(u),
        };
        let (root, real) = match self.classify_worktree(operand, cwd) {
            Determination::Known(Placement::Inside { root, path }) => (root, path),
            Determination::Known(other) => return Determination::known(other),
            Determination::Undetermined(u) => return Determination::Undetermined(u),
        };
        let prefix = format!("{}/", root.trim_end_matches('/'));
        let Some(rest) = real.strip_prefix(&prefix) else {
            return Determination::undetermined("placed path does not start with its root");
        };
        let comps: Vec<&str> = rest.split('/').filter(|c| !c.is_empty()).collect();
        let Some((name, below)) = comps.split_first() else {
            return Determination::undetermined("placed path has no component below its root");
        };
        let checkout = format!("{prefix}{name}");
        if below.is_empty() {
            return Determination::known(Placement::IsRoot { root: checkout });
        }
        if below.contains(&".git") {
            return Determination::undetermined(format!(
                "`{operand}` is inside git metadata below {checkout}, not a checked-out file"
            ));
        }
        match probe(&checkout) {
            Determination::Known(true) => {}
            Determination::Known(false) => {
                return Determination::known(Placement::Outside { path: real });
            }
            Determination::Undetermined(u) => return Determination::Undetermined(u),
        }
        // Anchored at the storage root: the checkout entry is a component
        // BELOW it, so an entry that is itself a symlink fails here. Anchored
        // at `checkout`, a prefix like `<root>/<link>` would resolve to the
        // checkout and be accepted as the anchor.
        match self.no_symlink_below(&lexical, &root) {
            Determination::Known(true) => Determination::known(Placement::Inside {
                root: checkout,
                path: real,
            }),
            Determination::Known(false) => Determination::undetermined(format!(
                "`{operand}` reaches {checkout} through a symlink below its storage root {root}"
            )),
            Determination::Undetermined(u) => Determination::Undetermined(u),
        }
    }

    /// Is `dir` ITSELF a linked git worktree checkout directly under a worktree
    /// storage root (`<root>/<checkout>`, nothing deeper)?
    ///
    /// The git working-tree-discard `Allow` in [`crate::detect`] (user ruling
    /// 2026-11-07) needs this: `git -C <dir>` names exactly the checkout, and
    /// [`SafeRoots::classify_worktree_checkout`] answers `IsRoot` for the
    /// checkout itself BEFORE it probes anything, so it cannot tell a linked
    /// checkout from any other directory directly under a storage root.
    ///
    /// `Known(true)` ONLY when every one of these is OBSERVED:
    ///
    ///   * the session is not refused (raw cwd / `CLAUDE_PROJECT_DIR`), and a
    ///     checkout probe exists (only with a resolver);
    ///   * `dir` has no `..` component as SPELLED (`..` is collapsed lexically
    ///     before symlinks are resolved, so its landing place is not decided by
    ///     its spelling);
    ///   * [`SafeRoots::classify_worktree`] places it `Inside` a storage root
    ///     with EXACTLY one component below the root, and that component is not
    ///     `.git`;
    ///   * the same probe as [`SafeRoots::classify_worktree_checkout`]
    ///     (`is_linked_checkout`): the resolved directory is a real directory
    ///     (`lstat`, not a symlink) holding a `.git` regular FILE whose content
    ///     starts with `gitdir:` — a main checkout has a `.git` DIRECTORY and is
    ///     `Known(false)`;
    ///   * `dir`'s own spelling passes through the storage root with no
    ///     symlink below it ([`SafeRoots::no_symlink_below`], anchored at the
    ///     storage root, so a checkout entry that is itself a symlink fails).
    ///
    /// `Known(false)` is an observed "no" (outside every storage root, the
    /// storage root itself, a deeper path, a directory that is not a linked
    /// checkout). Every probe failure, refusal, symlink below the root or
    /// unplaceable spelling is `Undetermined`. Callers must read both as "keep
    /// the verdict you had".
    pub fn is_linked_worktree_checkout(&self, dir: &str, cwd: Option<&str>) -> Determination<bool> {
        if let Some(why) = &self.worktree_refusal {
            return Determination::undetermined(why.clone());
        }
        let Some(probe) = self.checkout_probe else {
            return Determination::undetermined(
                "blastguard has no worktree checkout probe for this session (no resolver)",
            );
        };
        if dir.split('/').any(|c| c == "..") {
            return Determination::undetermined(
                "directory has a `..` component, so where it lands is not decided by its spelling",
            );
        }
        let lexical = match self.lexical_absolute(dir, cwd) {
            Determination::Known(l) => l,
            Determination::Undetermined(u) => return Determination::Undetermined(u),
        };
        let (root, real) = match self.classify_worktree(dir, cwd) {
            Determination::Known(Placement::Inside { root, path }) => (root, path),
            Determination::Known(Placement::IsRoot { .. } | Placement::Outside { .. }) => {
                return Determination::known(false);
            }
            Determination::Undetermined(u) => return Determination::Undetermined(u),
        };
        let prefix = format!("{}/", root.trim_end_matches('/'));
        let Some(rest) = real.strip_prefix(&prefix) else {
            return Determination::undetermined("placed path does not start with its root");
        };
        let comps: Vec<&str> = rest.split('/').filter(|c| !c.is_empty()).collect();
        if comps.len() != 1 || comps[0] == ".git" {
            return Determination::known(false);
        }
        match probe(&real) {
            Determination::Known(true) => {}
            Determination::Known(false) => return Determination::known(false),
            Determination::Undetermined(u) => return Determination::Undetermined(u),
        }
        match self.no_symlink_below(&lexical, &root) {
            Determination::Known(true) => Determination::known(true),
            Determination::Known(false) => Determination::undetermined(format!(
                "`{dir}` reaches {real} through a symlink below its storage root {root}"
            )),
            Determination::Undetermined(u) => Determination::Undetermined(u),
        }
    }
}

/// "Is `dir` a linked git worktree checkout?" — `Known(false)` only for an
/// observed no; an IO error is `Undetermined` (never folded into "no", so the
/// caller can name it rather than report the operand as placed `Outside`).
pub(super) type CheckoutProbe = fn(&str) -> Determination<bool>;

/// The real checkout probe for [`SafeRoots::classify_worktree_checkout`] and
/// [`SafeRoots::is_linked_worktree_checkout`]:
/// `dir` is a real directory (`lstat`, not a symlink) whose `.git` is a
/// regular FILE (`lstat`) starting with `gitdir:` — the shape `git worktree
/// add` creates. A missing `dir` or `.git`, a symlink, a `.git` directory, or
/// a `.git` file that is shorter than or does not start with `gitdir:` is an
/// observed `Known(false)`; any other IO error is `Undetermined`.
pub(super) fn is_linked_checkout(dir: &str) -> Determination<bool> {
    use std::io::{ErrorKind, Read};
    match std::fs::symlink_metadata(dir) {
        Ok(m) if m.file_type().is_dir() => {}
        Ok(_) => return Determination::known(false),
        Err(e) if e.kind() == ErrorKind::NotFound => return Determination::known(false),
        Err(e) => return Determination::undetermined(format!("cannot lstat {dir}: {e}")),
    }
    let dotgit = format!("{}/.git", dir.trim_end_matches('/'));
    match std::fs::symlink_metadata(&dotgit) {
        Ok(m) if m.file_type().is_file() => {}
        Ok(_) => return Determination::known(false),
        Err(e) if e.kind() == ErrorKind::NotFound => return Determination::known(false),
        Err(e) => return Determination::undetermined(format!("cannot lstat {dotgit}: {e}")),
    }
    let mut head = [0u8; 7];
    match std::fs::File::open(&dotgit).and_then(|mut f| f.read_exact(&mut head)) {
        Ok(()) => Determination::known(&head == b"gitdir:"),
        Err(e) if e.kind() == ErrorKind::UnexpectedEof => Determination::known(false),
        Err(e) => Determination::undetermined(format!("cannot read {dotgit}: {e}")),
    }
}
