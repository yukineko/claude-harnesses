//! How an operand handed to a [`SafeRoots`] placement predicate is READ, and
//! the placement core that depends on it. Split out of `scope.rs` (file
//! growth cap).
//!
//! * [`Reading::Spelled`] — the operand is shell TEXT (what every string
//!   reading hands in): a character the shell could expand or re-read
//!   (`$`, `~`, a glob, a quote, a blank, …; [`NOT_A_LITERAL_PATH`]) means the
//!   spelling is not the path, so the operand is `Undetermined`.
//! * [`Reading::Value`] — the operand is a path VALUE the AST evaluator
//!   already proved the shell produces (`detect::ast`, a `Value::Literal`
//!   after quote removal and expansion). Its characters are the path's own
//!   bytes: `~` inside `"~/x"` or `./~/x` is a directory named `~`, not the
//!   home. Only a control character (or an empty value) is refused. Every
//!   other rule — the relative-base rule, lexical `..` collapse with the
//!   residual-`..` refusal, the resolver, longest-root match, `IsRoot` kept
//!   distinct, the no-symlink-below rule — is exactly the spelled one.

use harness_core::verdict::Determination;

use super::NOT_A_LITERAL_PATH;
use super::{normalize_abs, resolve_with, Placement, SafeRoots, IRREPLACEABLE_HOME_DIRS};

/// See the module doc.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Reading {
    Spelled,
    Value,
}

impl SafeRoots {
    pub(super) fn classify_disposable_in(
        &self,
        roots: &[String],
        kind: &str,
        operand: &str,
        cwd: Option<&str>,
        reading: Reading,
    ) -> Determination<Placement> {
        if let Some(why) = &self.worktree_refusal {
            return Determination::undetermined(why.clone());
        }
        if roots.is_empty() {
            return Determination::undetermined(format!(
                "blastguard has no {kind} deletion root for this session"
            ));
        }
        let lexical = match self.lexical_absolute_as(operand, cwd, reading) {
            Determination::Known(l) => l,
            Determination::Undetermined(u) => return Determination::Undetermined(u),
        };
        let placement = match self.classify_against_as(roots, operand, cwd, reading) {
            Determination::Known(p) => p,
            Determination::Undetermined(u) => return Determination::Undetermined(u),
        };
        let Placement::Inside { root, .. } = &placement else {
            return Determination::known(placement);
        };
        match self.no_symlink_below(&lexical, root) {
            Determination::Known(true) => Determination::known(placement),
            Determination::Known(false) => Determination::undetermined(format!(
                "`{operand}` reaches {root} through a symlink below the root, so what it deletes \
is not decided by where it is spelled"
            )),
            Determination::Undetermined(u) => Determination::Undetermined(u),
        }
    }

    /// The operand as an absolute, lexically normalised path (no symlink
    /// resolution), or why it cannot be one. The literal-path, relative-base
    /// and residual-`..` rules are exactly [`SafeRoots::classify`]'s (it uses
    /// this function).
    pub(crate) fn lexical_absolute(
        &self,
        operand: &str,
        cwd: Option<&str>,
    ) -> Determination<String> {
        self.lexical_absolute_as(operand, cwd, Reading::Spelled)
    }

    /// [`SafeRoots::lexical_absolute`] under `reading` (module doc).
    pub(super) fn lexical_absolute_as(
        &self,
        operand: &str,
        cwd: Option<&str>,
        reading: Reading,
    ) -> Determination<String> {
        if operand.is_empty() {
            return Determination::undetermined("empty operand");
        }
        let refused = |c: char| match reading {
            Reading::Spelled => NOT_A_LITERAL_PATH.contains(&c) || c.is_control(),
            Reading::Value => c.is_control(),
        };
        if operand.chars().any(refused) {
            return Determination::undetermined(match reading {
                Reading::Spelled => {
                    "operand is not a literal path (expansion, glob, quoting or a find -exec placeholder)"
                }
                Reading::Value => "path value holds a control character",
            });
        }
        let absolute = if operand.starts_with('/') {
            match normalize_abs(operand) {
                Some(a) => a,
                None => return Determination::undetermined("operand did not normalise to a path"),
            }
        } else {
            let Some(base) = cwd else {
                return Determination::undetermined(
                    "relative operand with no known working directory",
                );
            };
            if !base.starts_with('/') {
                return Determination::undetermined("working directory is not absolute");
            }
            match normalize_abs(&format!("{}/{}", base.trim_end_matches('/'), operand)) {
                Some(a) => a,
                None => return Determination::undetermined("operand did not normalise to a path"),
            }
        };
        // A relative operand that climbed above its base, or an absolute one
        // whose `..` could not be collapsed, still carries `..` here. Lexical
        // resolution has run; anything left is a path this module cannot name.
        if absolute.split('/').any(|c| c == "..") {
            return Determination::undetermined("operand still contains an unresolved `..`");
        }
        Determination::known(absolute)
    }

    /// Does deleting `operand` (one directory entry, not a tree) destroy an
    /// irreplaceable `$HOME`-level asset (backlog 6067a527)? `Some(path)` names
    /// the landing path when it is:
    ///
    ///  * inside a credential store at the top of `$HOME`
    ///    ([`IRREPLACEABLE_HOME_DIRS`]: `~/.ssh/id_rsa`, `~/.gnupg/…`), or
    ///  * a home-level dotfile directly at `$HOME/.<name>` (`~/.zshrc`,
    ///    `~/.gitconfig`) — the "home-level config" deletion class.
    ///
    /// `~`, `~/…`, `$HOME/…` and `${HOME}/…` are expanded against the
    /// canonical home; a relative operand is placed against `cwd`. The landing
    /// path is compared to the home both as spelled and with its PARENT
    /// resolved (`rm` removes the final entry itself, so a final symlink is not
    /// followed), so `/var/…` spelling a `/private/var/…` home still matches.
    ///
    /// `None` means "not recognised as a home asset", which keeps the caller's
    /// existing verdict; it is not a determination that the path is safe.
    pub(crate) fn irreplaceable_home_asset(
        &self,
        operand: &str,
        cwd: Option<&str>,
    ) -> Option<String> {
        let home = self.home_real()?.trim_end_matches('/');
        let expanded = ["${HOME}", "$HOME", "~"]
            .iter()
            .find_map(|p| {
                operand.strip_prefix(p).and_then(|rest| {
                    (rest.is_empty() || rest.starts_with('/')).then(|| format!("{home}{rest}"))
                })
            })
            .unwrap_or_else(|| operand.to_string());
        self.home_asset_at(&expanded, cwd, Reading::Spelled)
    }

    /// [`SafeRoots::irreplaceable_home_asset`] of an absolute path VALUE
    /// ([`Reading::Value`]: no `~` / `$HOME` expansion — a value's `~` is a
    /// character). `Undetermined` when the value cannot be placed, so the
    /// caller can refuse instead of reading `None` as "not an asset".
    pub(crate) fn irreplaceable_home_asset_value(
        &self,
        path: &str,
    ) -> Determination<Option<String>> {
        if let Determination::Undetermined(u) = self.lexical_absolute_as(path, None, Reading::Value)
        {
            return Determination::Undetermined(u);
        }
        Determination::known(self.home_asset_at(path, None, Reading::Value))
    }

    fn home_asset_at(&self, expanded: &str, cwd: Option<&str>, reading: Reading) -> Option<String> {
        let home = self.home_real()?.trim_end_matches('/');
        let Determination::Known(lexical) = self.lexical_absolute_as(expanded, cwd, reading) else {
            return None;
        };
        let mut candidates = vec![lexical.clone()];
        if let Some((parent, name)) = lexical.rsplit_once('/') {
            let parent = if parent.is_empty() { "/" } else { parent };
            if let Some(real_parent) = self.resolve(parent) {
                candidates.push(format!("{}/{name}", real_parent.trim_end_matches('/')));
            }
        }
        candidates.into_iter().find(|path| {
            let Some(rel) = path.strip_prefix(&format!("{home}/")) else {
                return false;
            };
            let first = rel.split('/').next().unwrap_or("");
            IRREPLACEABLE_HOME_DIRS.contains(&first)
                || (!rel.contains('/') && first.starts_with('.'))
        })
    }

    /// [`SafeRoots::classify_worktree`] under `reading` (module doc).
    pub(super) fn classify_worktree_as(
        &self,
        operand: &str,
        cwd: Option<&str>,
        reading: Reading,
    ) -> Determination<Placement> {
        if let Some(why) = &self.worktree_refusal {
            return Determination::undetermined(why.clone());
        }
        if self.worktree_roots.is_empty() {
            return Determination::undetermined(
                "blastguard has no worktree storage root for this session (no HOME / cwd / project dir)",
            );
        }
        self.classify_against_as(&self.worktree_roots, operand, cwd, reading)
    }

    pub(super) fn classify_against(
        &self,
        roots: &[String],
        operand: &str,
        cwd: Option<&str>,
    ) -> Determination<Placement> {
        self.classify_against_as(roots, operand, cwd, Reading::Spelled)
    }

    pub(super) fn classify_against_as(
        &self,
        roots: &[String],
        operand: &str,
        cwd: Option<&str>,
        reading: Reading,
    ) -> Determination<Placement> {
        let absolute = match self.lexical_absolute_as(operand, cwd, reading) {
            Determination::Known(a) => a,
            Determination::Undetermined(u) => return Determination::Undetermined(u),
        };
        let Some(real) = resolve_with(&absolute, self.resolver) else {
            return Determination::undetermined(
                "real path could not be resolved (no resolver, or the filesystem said no)",
            );
        };
        for root in roots {
            if &real == root {
                return Determination::known(Placement::IsRoot { root: root.clone() });
            }
        }
        // Longest matching root wins, so a nested root (`/tmp` and a project
        // that happens to live under it) reports the more specific one.
        let mut best: Option<&String> = None;
        for root in roots {
            let prefix = format!("{}/", root.trim_end_matches('/'));
            if real.starts_with(&prefix) && best.is_none_or(|b| root.len() > b.len()) {
                best = Some(root);
            }
        }
        match best {
            Some(root) => Determination::known(Placement::Inside {
                root: root.clone(),
                path: real,
            }),
            None => Determination::known(Placement::Outside { path: real }),
        }
    }
}
