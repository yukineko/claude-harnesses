//! Path-trust classifier: is a `Read` target part of the project the operator
//! is working in (trusted) or outside it (untrusted — `/tmp`, the home dir,
//! another project, or a `..`-escaping relative path)?
//!
//! There is no existing trusted/untrusted path notion in this repo; this is
//! new logic. Kept deliberately simple and conservative: anything that cannot
//! be positively resolved inside the project is `Untrusted` or `Indeterminate`,
//! never `Trusted` — the caller (`main::decide_mark`) treats both non-`Trusted`
//! answers the same way (mark the session tainted), so the only distinction
//! that matters operationally is "part of this project" vs "everything else".
//!
//! # "The project" is the REPOSITORY, not just the cwd subtree (backlog eb39308e)
//!
//! Through 0.1.8 `Trusted` had exactly one route: `resolved.starts_with(cwd)`.
//! That equated "the project" with "the subtree below the hook payload's `cwd`",
//! and it produced a false positive severe enough to make the gate unusable for
//! any worktree-based workflow:
//!
//! A condukt/flow subagent is handed a **linked git worktree** to work in
//! (`/mnt/c/tmp/aegis-worktrees/<topic>`, `~/harness-wt/<topic>`,
//! `…/.harness-worktrees/session-<id>`), while the hook payload's `cwd` stays the
//! session's project root — the *main* checkout. So the subagent's very first
//! `Read` of the tree it was created to edit resolved outside `cwd`, classified
//! `Untrusted`, and `mark` recorded `external-read`. The next `Edit`/`Write` was
//! then downgraded to `ask` — and a subagent has no human to answer an `ask`, so
//! the edit never happened. Measured against the real binary before the fix:
//! `cwd`=main checkout + `Read` of `<linked worktree>/a.rs` ⇒
//! `permissionDecision: "ask"`.
//!
//! A linked worktree is not third-party content. It is the same repository — the
//! same objects, the same history, authored by the same operator — checked out
//! twice. So [`classify`] now has a second route to `Trusted`: the target
//! resolves into a worktree of the **same git repository** as `root`
//! ([`git_common_dir`], compared for equality). Both directions work, since the
//! relation is symmetric and either side may be the payload's `cwd`.
//!
//! ## Why this is not a fail-open
//!
//! * **Positive identification only.** [`git_common_dir`] returns `Option`, and
//!   `None` — no `.git` anywhere up the tree, an unparseable `.git` file, a
//!   `commondir`/back-pointer that will not resolve — means *cannot determine*,
//!   which yields `Untrusted`. There is no `unwrap_or(true)`-shaped step and no
//!   "unknown ⇒ probably the same repo" arm (CLAUDE.md §3).
//! * **Both sides must resolve, and must be EQUAL.** A different repository has a
//!   different common dir, so another project on the same machine stays
//!   `Untrusted`. If `root` is not in a repository at all, the rule cannot fire
//!   and `starts_with` remains the only route.
//! * **A hand-written `.git` file is not enough.** Git registers a linked
//!   worktree with a back-pointer (`<gitdir>/gitdir`) naming the `.git` file it
//!   belongs to; this module requires that back-pointer to resolve to the very
//!   file it just read. Forging membership therefore requires write access to the
//!   target repository's own `.git` directory, i.e. an attacker who already owns
//!   the repository — a position from which this classifier was never the
//!   control. Asserted by `a_forged_dot_git_file_does_not_graft_trust`.
//!
//! ## What is deliberately NOT trusted
//!
//! **`~/.claude/` is not allowlisted**, even though reads there also taint and
//! that was investigated as part of eb39308e. It is not one trust domain: it
//! holds operator-authored config (`settings.json`, `agents/`, `skills/`) *next
//! to* `projects/<key>/<id>.jsonl` session transcripts — which embed verbatim
//! `WebFetch`/`WebSearch` output, exactly the provenance this crate exists to
//! track — and `plugins/cache/` + `plugins/marketplaces/`, which hold code
//! fetched from third-party marketplaces. Trusting the tree wholesale would let a
//! transcript replay laundering the taint it recorded. (In practice this is not
//! what blocked the subagents either: skills, agents and `CLAUDE.md` are loaded
//! by the harness, not through the `Read` tool, so they never reach `mark`'s
//! PostToolUse matcher.)

use std::path::{Component, Path, PathBuf};

/// Trust verdict for a single path, relative to a project root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trust {
    /// Resolves to somewhere inside `root`.
    Trusted,
    /// Resolves to somewhere outside `root`.
    Untrusted,
    /// Could not be resolved at all (empty target, or `root` itself could not
    /// be canonicalized). Callers must treat this the same as `Untrusted`
    /// (fail-closed) — see the module docs.
    Indeterminate,
}

/// Collapse `.`/`..` components of `path` **lexically** (no filesystem access,
/// no symlink resolution) — used as the fallback when `path` does not exist
/// yet (so `Path::canonicalize` would fail) but still needs `..`-escape
/// detection. A leading `..` past the root simply has nothing left to pop,
/// which is fine: the result still won't `starts_with` the root.
fn normalize_lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Resolve the canonical **git common dir** of the repository containing `start`
/// — the single directory shared by a repository's main checkout and all of its
/// linked worktrees, and therefore a usable identity for "the same repository".
///
/// Returns `None` for *cannot determine*, which callers must treat as "not the
/// same repository" (see the module docs): no `.git` at or above `start`, a `.git`
/// entry that is neither a regular file nor a directory, a `.git` file that does
/// not carry a resolvable `gitdir:` line, a linked-worktree admin dir whose
/// back-pointer does not name the `.git` file that led here, or a path that will
/// not canonicalize.
///
/// The two on-disk shapes, both handled:
///
/// * **Main checkout** — `<root>/.git` is a DIRECTORY. It *is* the common dir.
/// * **Linked worktree** — `<wt>/.git` is a FILE containing
///   `gitdir: <repo>/.git/worktrees/<name>`. That admin dir holds `commondir`
///   (relative, typically `../..`) pointing back at `<repo>/.git`, and `gitdir`,
///   the back-pointer to the `.git` file above. `commondir` is what makes two
///   different worktrees agree on one identity; the `gitdir` back-pointer is what
///   makes the claim unforgeable without write access to `<repo>/.git`.
///
/// Read with plain `std::fs` rather than by shelling out to
/// `git rev-parse --git-common-dir`: this runs inside a PreToolUse/PostToolUse
/// hook on every matching tool call, and `harness_core::discovery::git_toplevel`
/// (the existing subprocess-based resolver) answers a different question
/// (toplevel, not common dir) and cannot distinguish "git failed" from "not a
/// repo" without the `git_probe` machinery. `harness_core::git_probe::dot_git_present`
/// is the closest existing helper but returns only a `bool`, which cannot express
/// *which* repository — the whole point here.
fn git_common_dir(start: &Path) -> Option<PathBuf> {
    for dir in start.ancestors() {
        let dot_git = dir.join(".git");
        // `symlink_metadata`, not `metadata`: a symlink at `.git` is a shape git
        // itself does not write, so it is not identified — not followed and
        // guessed about.
        let Ok(meta) = std::fs::symlink_metadata(&dot_git) else {
            continue;
        };
        if meta.is_dir() {
            return dot_git.canonicalize().ok();
        }
        if !meta.is_file() {
            // Present but an unrecognised kind: stop rather than walk further up
            // and attribute this path to some ancestor repository.
            return None;
        }
        let text = std::fs::read_to_string(&dot_git).ok()?;
        let pointer = text
            .lines()
            .find_map(|line| line.trim().strip_prefix("gitdir:"))?
            .trim();
        if pointer.is_empty() {
            return None;
        }
        let pointer_path = Path::new(pointer);
        let gitdir = if pointer_path.is_absolute() {
            pointer_path.to_path_buf()
        } else {
            dir.join(pointer_path)
        };
        let gitdir = gitdir.canonicalize().ok()?;

        // UNFORGEABILITY: `<gitdir>/gitdir` must name the `.git` file just read.
        // Only a writer of `<repo>/.git/worktrees/<name>/` can satisfy this.
        let back = std::fs::read_to_string(gitdir.join("gitdir")).ok()?;
        let back = Path::new(back.trim()).canonicalize().ok()?;
        if back != dot_git.canonicalize().ok()? {
            return None;
        }

        // `commondir` is stored relative to the admin dir. Absent means the admin
        // dir is itself the common dir.
        let common = match std::fs::read_to_string(gitdir.join("commondir")) {
            Ok(rel) => {
                let rel = rel.trim();
                if rel.is_empty() {
                    return None;
                }
                let rel_path = Path::new(rel);
                if rel_path.is_absolute() {
                    rel_path.to_path_buf()
                } else {
                    gitdir.join(rel_path)
                }
            }
            Err(_) => gitdir,
        };
        return common.canonicalize().ok();
    }
    None
}

/// Do `root` and `resolved` belong to the SAME git repository?
///
/// `false` unless BOTH sides positively resolve to a common dir AND the two are
/// equal — "either side could not be determined" is `false`, not "probably yes".
///
/// `resolved` may not exist yet (a `Read`/`Write` of a not-yet-created file), so
/// the walk starts at its nearest existing-or-not ancestor directory;
/// [`git_common_dir`] itself only inspects ancestors that actually have a `.git`
/// entry, so a missing leaf costs nothing.
fn same_repository(root_canonical: &Path, resolved: &Path) -> bool {
    let Some(root_repo) = git_common_dir(root_canonical) else {
        return false;
    };
    let start = if resolved.is_dir() {
        resolved
    } else {
        match resolved.parent() {
            Some(parent) => parent,
            None => return false,
        }
    };
    match git_common_dir(start) {
        Some(target_repo) => target_repo == root_repo,
        None => false,
    }
}

/// Classify `target` (a `Read` tool's `file_path`, as given — may be relative
/// or absolute) against `root` (the hook's `cwd`).
///
/// Resolution: `target` is joined onto `root` when relative, then resolved
/// with `canonicalize()` (follows symlinks — a symlink inside the root
/// pointing outside it correctly classifies as `Untrusted`) when the path
/// exists on disk; when it does not (a `Read` of a not-yet-created file, or a
/// test fixture that never touches disk), falls back to the lexical
/// normalization above. `root` itself is always canonicalized; if that fails,
/// there is no trustworthy boundary to compare against, so the answer is
/// `Indeterminate`.
pub fn classify(root: &Path, target: &str) -> Trust {
    let trimmed = target.trim();
    if trimmed.is_empty() {
        return Trust::Indeterminate;
    }
    // A literal, unexpanded leading `~` (`~/secret`, `~otheruser/x`) is never
    // what a real `Read` tool_input carries (Claude Code always resolves it
    // to an absolute path first) — so seeing one here means the shape is
    // unverified from this function's own resolution. Naively joining it onto
    // `root` would create a literal `<root>/~/secret` path that lexically
    // starts_with `root` and misclassifies as `Trusted`. Defensive: anything
    // we cannot confidently resolve to inside the root is `Untrusted`, not
    // `Trusted` (see FIX #4 in the crate's issue history).
    if trimmed.starts_with('~') {
        return Trust::Untrusted;
    }
    let Ok(root_canonical) = root.canonicalize() else {
        return Trust::Indeterminate;
    };
    let target_path = Path::new(target);
    let joined = if target_path.is_absolute() {
        target_path.to_path_buf()
    } else {
        // Join onto the CANONICAL root, not the raw one: on macOS a tempdir
        // path (`/var/folders/...`) is itself a symlink to
        // `/private/var/folders/...`, so joining onto the raw root and only
        // canonicalizing the root separately would compare two differently-
        // rooted paths and misclassify every not-yet-existing in-repo target
        // as escaping (see `harness_core::store::context_state_dir`'s own
        // canonicalize-before-join discipline for the same reason).
        root_canonical.join(target_path)
    };
    let resolved = joined
        .canonicalize()
        .unwrap_or_else(|_| normalize_lexical(&joined));
    if resolved.starts_with(&root_canonical) {
        return Trust::Trusted;
    }
    // Outside the cwd subtree, but possibly still the same repository checked out
    // a second time (a linked git worktree) — see the module docs, "The project is
    // the REPOSITORY". Positive identification only; anything unresolved falls
    // through to `Untrusted`.
    if same_repository(&root_canonical, &resolved) {
        return Trust::Trusted;
    }
    Trust::Untrusted
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(name: &str) -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix(&format!("taintguard-classify-{name}-"))
            .tempdir()
            .expect("tempdir")
    }

    #[test]
    fn in_repo_relative_path_is_trusted() {
        let root = temp_root("relative");
        std::fs::write(root.path().join("a.rs"), "x").unwrap();
        assert_eq!(classify(root.path(), "a.rs"), Trust::Trusted);
    }

    #[test]
    fn in_repo_absolute_path_is_trusted() {
        let root = temp_root("absolute");
        let sub = root.path().join("crates").join("x");
        std::fs::create_dir_all(&sub).unwrap();
        let f = sub.join("main.rs");
        std::fs::write(&f, "x").unwrap();
        assert_eq!(classify(root.path(), &f.to_string_lossy()), Trust::Trusted);
    }

    #[test]
    fn sibling_dir_outside_root_is_untrusted() {
        let root = temp_root("sibling-root");
        let inside = root.path().join("inside");
        std::fs::create_dir_all(&inside).unwrap();
        let outside_base = tempfile::Builder::new()
            .prefix("taintguard-classify-sibling-outside-")
            .tempdir()
            .unwrap();
        let outside = outside_base.path().join("secret.txt");
        std::fs::write(&outside, "s").unwrap();
        assert_eq!(
            classify(&inside, &outside.to_string_lossy()),
            Trust::Untrusted
        );
    }

    #[test]
    fn dotdot_escape_is_untrusted() {
        let root = temp_root("dotdot");
        let inside = root.path().join("inside");
        std::fs::create_dir_all(&inside).unwrap();
        // Escapes `inside` back up to a sibling of the project root — must not
        // be trusted just because the string starts inside the tree.
        assert_eq!(classify(&inside, "../../etc/passwd"), Trust::Untrusted);
    }

    #[test]
    fn dotdot_that_resolves_back_inside_is_trusted() {
        let root = temp_root("dotdot-back-inside");
        let sub = root.path().join("a").join("b");
        std::fs::create_dir_all(&sub).unwrap();
        // a/b/../c lexically resolves to a/c, still inside root.
        assert_eq!(classify(root.path(), "a/b/../c.rs"), Trust::Trusted);
    }

    #[test]
    fn empty_target_is_indeterminate() {
        let root = temp_root("empty");
        assert_eq!(classify(root.path(), ""), Trust::Indeterminate);
        assert_eq!(classify(root.path(), "   "), Trust::Indeterminate);
    }

    #[test]
    fn unresolvable_root_is_indeterminate() {
        let missing_root = Path::new("/definitely/does/not/exist/taintguard-root");
        assert_eq!(classify(missing_root, "a.rs"), Trust::Indeterminate);
    }

    #[test]
    fn nonexistent_target_still_classifies_lexically() {
        let root = temp_root("nonexistent-target");
        // The file does not exist (a `Read` of a not-yet-created path), so
        // canonicalize fails; the lexical fallback must still classify it as
        // inside the root.
        assert_eq!(classify(root.path(), "not-yet-created.rs"), Trust::Trusted);
        assert_eq!(
            classify(root.path(), "../outside-not-yet-created.rs"),
            Trust::Untrusted
        );
    }

    #[test]
    fn tmp_style_absolute_path_is_untrusted() {
        let root = temp_root("tmp-style");
        assert_eq!(
            classify(root.path(), "/tmp/some-scratch-file"),
            Trust::Untrusted
        );
    }

    /// FIX #4: an unexpanded leading `~` must never be trusted just because
    /// joining it onto the root lexically starts with the root.
    #[test]
    fn unexpanded_tilde_path_is_untrusted() {
        let root = temp_root("tilde");
        assert_eq!(classify(root.path(), "~/secret"), Trust::Untrusted);
        assert_eq!(classify(root.path(), "~"), Trust::Untrusted);
        assert_eq!(classify(root.path(), "~otheruser/x"), Trust::Untrusted);
    }

    // -----------------------------------------------------------------------
    // Sibling git worktrees of the SAME repository (backlog eb39308e)
    // -----------------------------------------------------------------------

    /// Run `git` with `args`, panicking with its stderr when it fails, so a
    /// broken fixture never silently degrades into "the property under test did
    /// not hold". Real `git`, deliberately: the linked-worktree on-disk format
    /// (`.git` file → `gitdir:` → `commondir`/`gitdir` back-pointer) is what
    /// [`git_common_dir`] parses, and a hand-built fixture alone could drift
    /// away from what git actually writes.
    fn git(args: &[&str]) {
        let out = std::process::Command::new("git")
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .output()
            .expect("git must be on PATH to run this test");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// A real repository with one commit, plus a real linked worktree created
    /// OUTSIDE it. Returns `(holder, main_checkout, linked_worktree)`.
    fn repo_with_linked_worktree(name: &str) -> (tempfile::TempDir, PathBuf, PathBuf) {
        let holder = temp_root(name);
        let main = holder.path().join("repo");
        std::fs::create_dir_all(&main).unwrap();
        let main_s = main.to_string_lossy().into_owned();
        git(&["-C", &main_s, "init", "-q", "-b", "main"]);
        std::fs::write(main.join("a.rs"), "fn main() {}").unwrap();
        git(&["-C", &main_s, "add", "-A"]);
        git(&[
            "-C",
            &main_s,
            "-c",
            "user.email=t@example.com",
            "-c",
            "user.name=t",
            "commit",
            "-qm",
            "init",
        ]);
        // Deliberately a SIBLING of the repo, not a child: that is the shape
        // condukt/flow uses (`/mnt/c/tmp/aegis-worktrees/<topic>`,
        // `~/harness-wt/<topic>`), and it is precisely why `starts_with(root)`
        // alone rejected it.
        let linked = holder.path().join("linked");
        let linked_s = linked.to_string_lossy().into_owned();
        git(&[
            "-C", &main_s, "worktree", "add", "-q", &linked_s, "-b", "topic", "main",
        ]);
        (holder, main, linked)
    }

    /// eb39308e (THE REGRESSION): a `Read` of a file in a sibling git worktree
    /// of the SAME repository must be `Trusted`.
    ///
    /// This is the shape every condukt/flow subagent runs in: the hook payload's
    /// `cwd` is the session's project root (the main checkout), while the
    /// subagent has been handed an absolute path inside a linked worktree that is
    /// NOT under that root. Before this fix `starts_with(root_canonical)` was the
    /// only route to `Trusted`, so that read classified `Untrusted` → `mark`
    /// recorded `external-read` → the very next `Edit`/`Write` was downgraded to
    /// ask/deny, and the subagent could not edit the tree it was created to edit.
    #[test]
    fn sibling_git_worktree_of_the_same_repo_is_trusted() {
        let (_holder, main, linked) = repo_with_linked_worktree("same-repo-linked");
        let target = linked.join("a.rs");
        assert_eq!(
            classify(&main, &target.to_string_lossy()),
            Trust::Trusted,
            "a file in a linked worktree of the same repo is the operator's own \
             checkout, not untrusted-provenance content"
        );
    }

    /// The symmetric direction: `cwd` is the linked worktree (a subagent that
    /// really did `cd` into it) and the target is in the main checkout. Same
    /// repository, so also `Trusted` — the relation must not depend on which
    /// side happens to be the payload's `cwd`.
    #[test]
    fn main_checkout_read_from_inside_a_linked_worktree_is_trusted() {
        let (_holder, main, linked) = repo_with_linked_worktree("same-repo-reverse");
        let target = main.join("a.rs");
        assert_eq!(classify(&linked, &target.to_string_lossy()), Trust::Trusted);
    }

    /// A not-yet-created file inside a sibling worktree of the same repo is
    /// still `Trusted`: the repository identity is resolved from the nearest
    /// EXISTING ancestor, so `canonicalize` failing on the leaf must not flip
    /// the answer (that would make the fix work only for files that already
    /// exist — i.e. not for a subagent creating one).
    #[test]
    fn not_yet_created_file_in_a_sibling_worktree_is_trusted() {
        let (_holder, main, linked) = repo_with_linked_worktree("same-repo-newfile");
        let target = linked.join("subdir").join("brand-new.rs");
        assert_eq!(classify(&main, &target.to_string_lossy()), Trust::Trusted);
    }

    /// CONTROL (anti-vacuity): a DIFFERENT git repository is still `Untrusted`.
    /// Without this, "every path that lives in some git repo is trusted" would
    /// pass the three tests above while opening a hole wide enough to drive
    /// another project's secrets through.
    #[test]
    fn a_different_git_repository_is_still_untrusted() {
        let (_holder_a, main_a, _linked_a) = repo_with_linked_worktree("other-repo-a");
        let (_holder_b, main_b, linked_b) = repo_with_linked_worktree("other-repo-b");
        assert_eq!(
            classify(&main_a, &main_b.join("a.rs").to_string_lossy()),
            Trust::Untrusted,
            "another repository's main checkout is not this repository"
        );
        assert_eq!(
            classify(&main_a, &linked_b.join("a.rs").to_string_lossy()),
            Trust::Untrusted,
            "another repository's linked worktree is not this repository either"
        );
    }

    /// CONTROL (anti-vacuity): a path in no git repository at all stays
    /// `Untrusted`. `git_common_dir` returning `None` must fail CLOSED, never
    /// "unknown ⇒ same repo".
    #[test]
    fn a_path_in_no_repository_is_still_untrusted() {
        let (_holder, main, _linked) = repo_with_linked_worktree("no-repo-target");
        let outside = temp_root("no-repo-outside");
        let target = outside.path().join("secret.txt");
        std::fs::write(&target, "s").unwrap();
        assert_eq!(classify(&main, &target.to_string_lossy()), Trust::Untrusted);
    }

    /// CONTROL (anti-vacuity): when the ROOT is not a git repository, the
    /// worktree rule cannot apply at all — `starts_with` remains the only route
    /// to `Trusted`, so an outside path stays `Untrusted`.
    #[test]
    fn non_git_root_gains_no_worktree_trust() {
        let root = temp_root("non-git-root");
        let (_holder, main, _linked) = repo_with_linked_worktree("non-git-root-other");
        assert_eq!(
            classify(root.path(), &main.join("a.rs").to_string_lossy()),
            Trust::Untrusted
        );
    }

    /// SPOOFING CONTROL: a hand-written `.git` FILE whose `gitdir:` points into
    /// the real repository's `.git` must NOT graft its directory onto that
    /// repository's trust domain.
    ///
    /// Git registers a linked worktree by writing a BACK-POINTER
    /// (`<gitdir>/gitdir`) that names the `.git` file it belongs to.
    /// [`git_common_dir`] requires that back-pointer to resolve to the very file
    /// it just read, so forging trust needs write access to the project's own
    /// `.git` directory — at which point the attacker already owns the repo and
    /// this classifier is not the control that was supposed to stop them.
    #[test]
    fn a_forged_dot_git_file_does_not_graft_trust() {
        let (_holder, main, linked) = repo_with_linked_worktree("forged");
        // Reuse the REAL linked worktree's gitdir, so the `gitdir:` line points
        // at a genuinely-registered worktree admin dir — only the back-pointer
        // fails to name this directory.
        let real_gitdir = std::fs::read_to_string(linked.join(".git")).unwrap();
        let real_gitdir = real_gitdir
            .lines()
            .find_map(|l| l.trim().strip_prefix("gitdir:"))
            .unwrap()
            .trim()
            .to_string();

        let attacker = temp_root("forged-attacker");
        let attacker_dir = attacker.path().join("looks-like-a-worktree");
        std::fs::create_dir_all(&attacker_dir).unwrap();
        std::fs::write(
            attacker_dir.join(".git"),
            format!("gitdir: {real_gitdir}\n"),
        )
        .unwrap();
        let target = attacker_dir.join("payload.md");
        std::fs::write(&target, "untrusted content").unwrap();

        assert_eq!(
            classify(&main, &target.to_string_lossy()),
            Trust::Untrusted,
            "a `.git` file anyone can write must not be enough to claim membership \
             of this repository"
        );
    }

    /// A `.git` file that does not parse (no `gitdir:` line, empty, garbage) is
    /// cannot-determine → `Untrusted`, never "assume same repo".
    #[test]
    fn an_unparseable_dot_git_file_is_untrusted() {
        let (_holder, main, _linked) = repo_with_linked_worktree("bad-dotgit");
        let other = temp_root("bad-dotgit-other");
        let dir = other.path().join("d");
        std::fs::create_dir_all(&dir).unwrap();
        for body in [
            "",
            "not a gitdir line\n",
            "gitdir:\n",
            "gitdir: /nope/nope\n",
        ] {
            std::fs::write(dir.join(".git"), body).unwrap();
            let target = dir.join("f.txt");
            std::fs::write(&target, "x").unwrap();
            assert_eq!(
                classify(&main, &target.to_string_lossy()),
                Trust::Untrusted,
                "a `.git` file body of {body:?} must fail closed"
            );
        }
    }
}
