//! Is destroying this path recoverable afterwards?
//!
//! This module exists because the gate was answering the wrong question. Every
//! rule that generated friction was asking some form of *"can I prove this
//! command has no effect?"* — and that question is **undecidable** from a
//! command line. A program reaches an effect without spelling any recognisable
//! name (aliased imports, `getattr`, string-built identifiers), so a scan that
//! finds no known effect token has established nothing. The gate then had two
//! bad options: call the unknown clean (a fail-open, CLAUDE.md §3), or ask
//! about it (which it did — 205 of the 223 non-allow verdicts measured over 25
//! transcripts were asks about work that destroyed nothing).
//!
//! The operator's ruling replaced the question, verbatim:
//!
//! > 健全とは戻せない変更でもない限り自律的に実行できること。破壊的な変更を
//! > 暗黙に実施しないこと。よみかきに一々許可をもとめるのは健全でもなんでもない。
//! > 無駄
//!
//! *Sound means: anything short of an unrecoverable change runs autonomously.
//! Destructive changes are never made implicitly. Asking permission for reads
//! and writes is not soundness, it is waste.*
//!
//! That is a different axis, and — this is the part that matters — it is a
//! **decidable** one. "Does this program have effects" cannot be answered by
//! looking. "Can these bytes be recovered after they are gone" can: the file
//! either exists or it does not, and it either has a copy in git or it does
//! not. Both are observations, not predictions (CLAUDE.md §2).
//!
//! So the three-valued answer here is never a guess. [`Recovery::Undetermined`]
//! means a probe failed, and it surfaces — it is not folded into either real
//! answer (CLAUDE.md §3).
//!
//! # What this module deliberately does NOT decide
//!
//! Recoverability is not the only reason to surface a command. Writing
//! `.githooks/pre-commit` is perfectly recoverable from git and must still be
//! surfaced, because a silently disabled gate makes every later verdict
//! meaningless. That axis stays where it already lives — `exclude::is_protected_path`
//! and `protected_path_block` — and callers must consult it FIRST. This module
//! answers one question only.

use std::path::{Path, PathBuf};
use std::time::Duration;

use harness_core::git_probe::{probe_repo, RepoProbe};
use harness_core::verdict::Determination;

/// Whether the bytes at a path survive being destroyed.
#[derive(Debug, Clone, PartialEq, Eq)]
#[must_use]
pub enum Recovery {
    /// The path does not exist. Truncating or writing it destroys nothing —
    /// there are no prior bytes to lose.
    NothingToDestroy,
    /// Tracked by git and byte-identical to the index and HEAD, so
    /// `git restore` brings it back exactly.
    RecoverableFromGit,
    /// The bytes exist only here. Losing them is final; the reason names what
    /// makes it final.
    Unrecoverable(String),
    /// A probe did not answer. This is NOT "recoverable" — it is the absence of
    /// an answer, and it resolves to the restrictive side at the call site.
    Undetermined(String),
}

impl Recovery {
    /// True only for the two answers that actually establish recoverability.
    /// `Undetermined` is not one of them.
    pub fn is_recoverable(&self) -> bool {
        matches!(
            self,
            Recovery::NothingToDestroy | Recovery::RecoverableFromGit
        )
    }

    /// The human-readable half of a verdict: what was observed about this path.
    pub fn describe(&self) -> String {
        match self {
            Recovery::NothingToDestroy => "it does not exist yet".to_string(),
            Recovery::RecoverableFromGit => {
                "it is tracked by git with no uncommitted changes, so `git restore` recovers it"
                    .to_string()
            }
            Recovery::Unrecoverable(why) | Recovery::Undetermined(why) => why.clone(),
        }
    }
}

/// What `git status --porcelain --ignored` said about one path.
///
/// Kept separate from [`Recovery`] so the decision table below is pure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitState {
    /// Tracked, and matching HEAD and the index: no local bytes at risk.
    TrackedClean,
    /// Tracked, but carrying staged or unstaged modifications. Those
    /// modifications exist nowhere else.
    TrackedDirty,
    /// Not tracked (including `.gitignore`d). Git holds no copy at all.
    Untracked,
    /// git did not answer.
    Undetermined,
}

/// Pure decision core — no IO, so every row of the table is unit-testable by a
/// reviewer who does not trust the wiring.
///
/// | exists | repo | git state | → |
/// |---|---|---|---|
/// | ✗ | — | — | `NothingToDestroy` |
/// | ✓ | `NotRepo` | — | `Unrecoverable` (no version control holds a copy) |
/// | ✓ | `Undetermined` | — | `Undetermined` |
/// | ✓ | `Repo` | `TrackedClean` | `RecoverableFromGit` |
/// | ✓ | `Repo` | `TrackedDirty` | `Unrecoverable` (uncommitted bytes) |
/// | ✓ | `Repo` | `Untracked` | `Unrecoverable` (git has no copy) |
/// | ✓ | `Repo` | `Undetermined` | `Undetermined` |
///
/// The `exists = ✗` row is the one that retires the largest single class of
/// false friction, and it is also a §4 correction: the rule it replaces denied
/// with the words *"truncates and overwrites an existing file"* while nothing
/// anywhere in the crate had ever checked whether the file existed.
pub fn decide_recovery(exists: bool, repo: RepoProbe, git: GitState) -> Recovery {
    if !exists {
        return Recovery::NothingToDestroy;
    }
    match repo {
        RepoProbe::NotRepo => Recovery::Unrecoverable(
            "it exists, and it is not inside a git work tree — nothing holds a second copy of \
these bytes"
                .to_string(),
        ),
        RepoProbe::Undetermined => Recovery::Undetermined(
            "it exists, and blastguard could not determine whether it is inside a git work tree, \
so whether these bytes are recoverable is unknown"
                .to_string(),
        ),
        RepoProbe::Repo => match git {
            GitState::TrackedClean => Recovery::RecoverableFromGit,
            GitState::TrackedDirty => Recovery::Unrecoverable(
                "it is tracked by git but carries uncommitted changes — those changes exist \
nowhere else"
                    .to_string(),
            ),
            GitState::Untracked => Recovery::Unrecoverable(
                "it exists but git does not track it, so no committed copy can restore it"
                    .to_string(),
            ),
            GitState::Undetermined => Recovery::Undetermined(
                "it exists inside a git work tree, but `git status` did not answer, so whether \
these bytes are recoverable is unknown"
                    .to_string(),
            ),
        },
    }
}

/// Read one `git status --porcelain --ignored` line for a single path.
///
/// The two-character status field is what distinguishes the cases:
/// * empty output  → tracked and clean
/// * `?? path`     → untracked
/// * `!! path`     → ignored (which is untracked as far as recovery goes)
/// * anything else → tracked with staged/unstaged changes
///
/// Note the empty-output row is only sound BECAUSE `--ignored` is passed:
/// without it an ignored scratch file also prints nothing, and would have been
/// read as "tracked and clean" — a fail-open on exactly the files most likely
/// to hold un-backed-up work.
pub fn decide_git_state(spawned_ok: bool, exit_ok: bool, stdout: &str) -> GitState {
    if !spawned_ok || !exit_ok {
        return GitState::Undetermined;
    }
    let line = stdout.lines().find(|l| !l.trim().is_empty());
    match line {
        None => GitState::TrackedClean,
        Some(l) if l.starts_with("??") || l.starts_with("!!") => GitState::Untracked,
        Some(_) => GitState::TrackedDirty,
    }
}

/// Absolute path for `target`, resolved against `base` when relative.
///
/// Returns `None` when the target is relative and no base is known: guessing a
/// base is how a path in one tree gets judged by the state of a file in
/// another. `None` reaches the caller as `Undetermined`, not as clean.
pub fn resolve(target: &str, base: Option<&str>) -> Option<PathBuf> {
    let norm = crate::exclude::normalize(target);
    if norm.is_empty() {
        return None;
    }
    if norm.starts_with('/') {
        return Some(PathBuf::from(norm));
    }
    let base = base?;
    if !base.starts_with('/') {
        return None;
    }
    Some(PathBuf::from(format!(
        "{}/{}",
        base.trim_end_matches('/'),
        norm
    )))
}

const GIT_TIMEOUT: Duration = Duration::from_millis(1500);

/// The wired probe. One `git status` spawn, bounded, and only when the path
/// actually exists — the non-existent case is answered by the filesystem alone
/// and costs nothing.
pub fn probe(target: &str, base: Option<&str>) -> Recovery {
    // AN UNEXPANDED TARGET IS NOT AN ABSENT ONE. `$HOME/.ssh/id_ed25519` names
    // a real file, but the literal text with `$HOME` still in it names nothing,
    // so `symlink_metadata` fails and the absence row would report
    // `NothingToDestroy` — turning "blastguard cannot see where this points" into
    // "there is nothing there". Measured: this exact command reached Allow
    // while the 0.2.58 rule denied it, which makes it a regression introduced by
    // asking the filesystem at all. Checked before any IO, since the whole point
    // is that the IO would answer about the wrong path (CLAUDE.md §3).
    if target.contains('$') || target.contains('`') {
        return Recovery::Undetermined(format!(
            "`{target}` contains a shell expansion, so blastguard cannot tell which file this \
names, let alone whether its contents are recoverable"
        ));
    }
    let Some(path) = resolve(target, base) else {
        return Recovery::Undetermined(format!(
            "`{target}` is a relative path and blastguard does not know which directory it is \
relative to, so it cannot tell whether these bytes are recoverable"
        ));
    };
    // `symlink_metadata`, not `metadata`: a dangling symlink still occupies the
    // name, and `>` through it creates the target rather than destroying it.
    if std::fs::symlink_metadata(&path).is_err() {
        return decide_recovery(false, RepoProbe::NotRepo, GitState::Undetermined);
    }
    let dir = path.parent().unwrap_or(Path::new("/"));
    let repo = probe_repo(dir);
    if !matches!(repo, RepoProbe::Repo) {
        return decide_recovery(true, repo, GitState::Undetermined);
    }
    let mut cmd = std::process::Command::new("git");
    cmd.current_dir(dir)
        .args(["status", "--porcelain", "--ignored", "--"])
        .arg(&path);
    let git = match harness_core::boundary::run_with_timeout(&mut cmd, GIT_TIMEOUT) {
        // `stdout_allowing(&[0])` is where a crashed checker stops looking like
        // a clean one: any code but 0 yields `Undetermined`, so an empty stdout
        // from a git that fell over cannot read as `TrackedClean`.
        Determination::Known(out) => match out.stdout_allowing(&[0]) {
            Determination::Known(stdout) => decide_git_state(true, true, &stdout),
            Determination::Undetermined(_) => decide_git_state(true, false, ""),
        },
        Determination::Undetermined(_) => decide_git_state(false, false, ""),
    };
    decide_recovery(true, repo, git)
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- decide_recovery: one test per row of the documented table ---

    #[test]
    fn absent_path_destroys_nothing() {
        assert_eq!(
            decide_recovery(false, RepoProbe::NotRepo, GitState::Undetermined),
            Recovery::NothingToDestroy
        );
        // Absent wins regardless of the other two axes: there are no bytes.
        assert_eq!(
            decide_recovery(false, RepoProbe::Repo, GitState::TrackedDirty),
            Recovery::NothingToDestroy
        );
    }

    #[test]
    fn existing_file_outside_a_repo_is_unrecoverable() {
        let r = decide_recovery(true, RepoProbe::NotRepo, GitState::Undetermined);
        assert!(matches!(r, Recovery::Unrecoverable(_)), "{r:?}");
        assert!(!r.is_recoverable());
    }

    #[test]
    fn tracked_and_clean_is_recoverable() {
        assert_eq!(
            decide_recovery(true, RepoProbe::Repo, GitState::TrackedClean),
            Recovery::RecoverableFromGit
        );
    }

    #[test]
    fn tracked_but_dirty_is_unrecoverable() {
        let r = decide_recovery(true, RepoProbe::Repo, GitState::TrackedDirty);
        assert!(matches!(r, Recovery::Unrecoverable(_)), "{r:?}");
    }

    #[test]
    fn untracked_is_unrecoverable() {
        let r = decide_recovery(true, RepoProbe::Repo, GitState::Untracked);
        assert!(matches!(r, Recovery::Unrecoverable(_)), "{r:?}");
    }

    /// Both undetermined rows. Neither may report as recoverable — that is the
    /// §3 invariant this whole module is built around.
    #[test]
    fn undetermined_never_reports_recoverable() {
        for r in [
            decide_recovery(true, RepoProbe::Undetermined, GitState::TrackedClean),
            decide_recovery(true, RepoProbe::Repo, GitState::Undetermined),
        ] {
            assert!(matches!(r, Recovery::Undetermined(_)), "{r:?}");
            assert!(
                !r.is_recoverable(),
                "Undetermined must not read as recoverable"
            );
        }
    }

    // --- decide_git_state ---

    #[test]
    fn empty_status_means_tracked_and_clean() {
        assert_eq!(decide_git_state(true, true, ""), GitState::TrackedClean);
        assert_eq!(
            decide_git_state(true, true, "\n  \n"),
            GitState::TrackedClean
        );
    }

    #[test]
    fn question_marks_and_bangs_mean_untracked() {
        assert_eq!(
            decide_git_state(true, true, "?? a/b.txt\n"),
            GitState::Untracked
        );
        assert_eq!(
            decide_git_state(true, true, "!! target/x\n"),
            GitState::Untracked
        );
    }

    #[test]
    fn any_other_status_code_means_dirty() {
        for s in [
            " M a.rs\n",
            "M  a.rs\n",
            "A  a.rs\n",
            "MM a.rs\n",
            "UU a.rs\n",
        ] {
            assert_eq!(
                decide_git_state(true, true, s),
                GitState::TrackedDirty,
                "{s:?}"
            );
        }
    }

    /// A checker that fell over did not pass. Both failure axes are the same
    /// answer, and it is not `TrackedClean` — which is what an exit-status-blind
    /// reading would have produced for a crashed git (empty stdout).
    #[test]
    fn git_failure_is_undetermined_not_clean() {
        assert_eq!(decide_git_state(false, false, ""), GitState::Undetermined);
        assert_eq!(decide_git_state(true, false, ""), GitState::Undetermined);
        assert_eq!(
            decide_git_state(true, false, "?? x\n"),
            GitState::Undetermined,
            "a non-zero exit invalidates the stdout, however parseable it looks"
        );
    }

    // --- resolve ---

    #[test]
    fn relative_without_a_base_is_not_guessed() {
        assert_eq!(resolve("notes.txt", None), None);
        assert_eq!(
            resolve("notes.txt", Some("relative/base")),
            None,
            "a relative base cannot place a relative target"
        );
    }

    #[test]
    fn relative_resolves_against_an_absolute_base() {
        assert_eq!(
            resolve("a/b.txt", Some("/w/repo")),
            Some(PathBuf::from("/w/repo/a/b.txt"))
        );
        assert_eq!(
            resolve("/etc/hosts", Some("/w/repo")),
            Some(PathBuf::from("/etc/hosts")),
            "an absolute target ignores the base"
        );
    }

    /// `resolve` runs the same `..`-collapsing normaliser the protected-path
    /// checks use, so a traversal cannot name one file and be probed as another.
    #[test]
    fn parent_segments_are_resolved_before_probing() {
        assert_eq!(
            resolve("/tmp/../etc/hosts", None),
            Some(PathBuf::from("/etc/hosts"))
        );
    }

    /// The probe is the permissive direction, so a missing answer has to reach
    /// the caller as `Undetermined` rather than as a cheap "recoverable".
    #[test]
    fn unplaceable_relative_target_probes_as_undetermined() {
        let r = probe("some/relative/path", None);
        assert!(matches!(r, Recovery::Undetermined(_)), "{r:?}");
        assert!(!r.is_recoverable());
    }

    /// An unexpanded `$VAR` in the target must not be answered by statting the
    /// literal text: that path never exists, and `NothingToDestroy` would be
    /// reporting "nothing there" for a file the shell is about to resolve and
    /// truncate. Regression pin — `echo x > $HOME/.ssh/id_ed25519` reached
    /// Allow before this check existed.
    #[test]
    fn unexpanded_target_is_undetermined_not_absent() {
        for t in [
            "$HOME/.ssh/id_ed25519",
            "${TMPDIR}/out",
            "/tmp/$(date +%s).log",
            "`echo /etc/hosts`",
        ] {
            let r = probe(t, Some("/home/someone/repo"));
            assert!(matches!(r, Recovery::Undetermined(_)), "{t}: {r:?}");
            assert!(!r.is_recoverable(), "{t} must not read as recoverable");
        }
    }

    /// End-to-end against the real filesystem: a path that does not exist is
    /// answered without git being consulted at all.
    #[test]
    fn absent_path_probes_as_nothing_to_destroy() {
        let dir = std::env::temp_dir().join(format!("bg-rev-{}", std::process::id()));
        let missing = dir.join("definitely-not-here.txt");
        assert_eq!(
            probe(&missing.to_string_lossy(), None),
            Recovery::NothingToDestroy
        );
    }
}
