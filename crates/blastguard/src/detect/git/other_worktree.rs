//! The git working-tree-discard verdict for a linked worktree checkout that
//! does not contain the session cwd (user ruling 2026-11-20, backlog
//! 606a93ab). Split out of `detect/git.rs`; [`super::managed_worktree_git`]
//! classifies the checkout, the discard arms there consult
//! [`managed_discard`].

use super::{Decision, Determination};

/// Which linked worktree checkout, if any, a git working-tree-discard runs in
/// (see [`super::managed_worktree_git`]).
pub(super) enum ManagedWt {
    /// Not a managed linked worktree checkout (or not shown to be one): the
    /// caller keeps the exact verdict it had before the 2026-11-07 ruling.
    No,
    /// The linked checkout that contains the session cwd
    /// ([`crate::scope::SafeRoots::session_cwd`]): the session's OWN worktree.
    Own,
    /// A linked checkout that does NOT contain the session cwd (or there is no
    /// session cwd, or the checkout could not be resolved): another session's
    /// worktree. Holds the directory git discovers the checkout from.
    Other(String),
}

/// The managed-checkout verdict for one working-tree-discard form (`what`
/// names it in the Ask reason): `None` — not a managed checkout, the caller
/// keeps its own verdict; `Some(Allow)` — the session's own checkout, or
/// another session's checkout that [`linked_checkout_is_clean`] positively
/// reports clean (`include_ignored`: ignored files count as changes, for
/// `git clean -x`/`-X`); `Some(Ask)` — another session's checkout with
/// uncommitted changes, or one whose cleanliness cannot be observed (user
/// ruling 2026-11-20, backlog 606a93ab; a subagent's Ask is hardened to Deny).
pub(super) fn managed_discard(
    managed: &ManagedWt,
    what: &str,
    include_ignored: bool,
) -> Option<Decision> {
    match managed {
        ManagedWt::No => None,
        ManagedWt::Own => Some(Decision::Allow),
        ManagedWt::Other(dir) => Some(other_worktree_discard(dir, what, include_ignored)),
    }
}

/// [`managed_discard`] for [`ManagedWt::Other`]: Allow only on a positive
/// clean report, Ask for reported changes and for every probe failure.
pub(super) fn other_worktree_discard(dir: &str, what: &str, include_ignored: bool) -> Decision {
    match linked_checkout_is_clean(dir, include_ignored) {
        Determination::Known(true) => Decision::Allow,
        Determination::Known(false) => Decision::ask(format!(
            "{what} in {dir} discards working-tree changes of a linked worktree that is not \
this session's (the session cwd is not inside it), and git reports uncommitted changes there \
(possibly another session's work)"
        )),
        Determination::Undetermined(u) => Decision::ask(format!(
            "{what} in {dir} discards working-tree changes of a linked worktree that is not \
this session's (the session cwd is not inside it), and blastguard cannot tell whether it has \
uncommitted changes ({})",
            u.reason()
        )),
    }
}

/// Does the git checkout at `dir` hold nothing a working-tree discard could
/// destroy? `Known(true)` ONLY when `git status --porcelain=v1 -z
/// --untracked-files=all --ignore-submodules=none` (plus `--ignored` when
/// `include_ignored`) exits 0 with empty output AND `git ls-files -v -z` exits
/// 0 with every index entry tagged `H` (no assume-unchanged / skip-worktree
/// entry, whose local edits status would not show). Any status output or
/// non-`H` tag is `Known(false)`; spawn failure, timeout, signal or any other
/// exit is `Undetermined`.
pub(super) fn linked_checkout_is_clean(dir: &str, include_ignored: bool) -> Determination<bool> {
    // One bounded git probe in `dir`: Known(stdout) only on exit 0.
    let probe = |args: &[&str]| -> Determination<String> {
        let mut cmd = std::process::Command::new("git");
        harness_core::git_env::scrub(&mut cmd).arg("-C").arg(dir);
        cmd.args(["--no-optional-locks", "-c", "core.fsmonitor=false"]);
        cmd.args(args);
        match harness_core::boundary::run_with_timeout(
            &mut cmd,
            std::time::Duration::from_millis(3000),
        ) {
            Determination::Known(out) => out.stdout_allowing(&[0]),
            Determination::Undetermined(why) => Determination::Undetermined(why),
        }
    };
    // `--ignore-submodules=none` overrides `submodule.<name>.ignore` /
    // `diff.ignoreSubmodules` config that would otherwise hide dirty
    // submodule content from the status probe.
    let mut status = vec![
        "status",
        "--porcelain=v1",
        "-z",
        "--untracked-files=all",
        "--ignore-submodules=none",
    ];
    if include_ignored {
        status.push("--ignored");
    }
    match probe(&status) {
        Determination::Known(s) if s.is_empty() => {}
        Determination::Known(_) => return Determination::known(false),
        Determination::Undetermined(u) => return Determination::Undetermined(u),
    }
    // Index entries marked assume-unchanged (lowercase tag) or skip-worktree
    // (`S`) can carry local edits that `git status` does not show. Every
    // entry must carry the plain `H` tag; anything else is not clean.
    match probe(&["ls-files", "-v", "-z"]) {
        Determination::Known(s) => Determination::known(
            s.split('\0')
                .filter(|r| !r.is_empty())
                .all(|r| r.starts_with("H ")),
        ),
        Determination::Undetermined(u) => Determination::Undetermined(u),
    }
}
