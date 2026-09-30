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
    /// The path is inside a git work tree.
    ///
    /// **This is a wider claim than its name suggests, on purpose.** Since the
    /// operator ruling of 2026-09-18 ([`decide_recovery`]), *every* path inside
    /// a checkout lands here — tracked-and-clean, tracked-with-uncommitted-
    /// changes, untracked, and paths whose `git status` never answered. Only
    /// the first of those is literally restorable by `git restore`; the rest
    /// are treated as recoverable by operator decision, not by observation.
    ///
    /// The variant keeps its name so the diff of that ruling stays legible, but
    /// do not read it as "git holds these bytes" — read it as "inside a work
    /// tree, and therefore never denied or asked about".
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
            // Deliberately does NOT claim "git restore recovers it": since the
            // 2026-09-18 ruling this variant also covers untracked and dirty
            // paths, for which that sentence would be false (CLAUDE.md §4).
            Recovery::RecoverableFromGit => {
                "it is inside a git work tree, which this gate does not refuse or ask about"
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
/// | ✓ | `Repo` | **any** | `RecoverableFromGit` |
///
/// The `exists = ✗` row is the one that retires the largest single class of
/// false friction, and it is also a §4 correction: the rule it replaces denied
/// with the words *"truncates and overwrites an existing file"* while nothing
/// anywhere in the crate had ever checked whether the file existed.
///
/// # The `Repo` row collapsed on 2026-09-18 — and what that gives up
///
/// This table used to split `Repo` four ways, answering `Unrecoverable` for
/// `TrackedDirty` and `Untracked` and `Undetermined` for `Undetermined`. The
/// operator struck those three rows, verbatim:
///
/// > blastguardが拒否すべきはシステムのファイルであり、gitで復元できるもの
/// > worktreeのファイルを拒否すべきことを禁止する
/// >
/// > またworktree内のファイルの処理をaskするのは禁止する。grepもrmも編集もすべて
///
/// *What blastguard may refuse is system files. Refusing files in a worktree —
/// anything git can restore — is forbidden. Asking about operations on files
/// inside a worktree is likewise forbidden: grep, rm, edits, all of them.*
///
/// The ruling followed a measured false deny: a heredoc writing a scratch file
/// was refused because the path blastguard **guessed** at — it could not
/// resolve the script's variable, so it fell back to the cwd, a worktree
/// directory — came back `Untracked`. The gate was not protecting anything;
/// it was reporting its own failure to resolve a path as danger.
///
/// **The protection this gives up is real, and is stated here rather than
/// hidden (CLAUDE.md §4): uncommitted changes to tracked files, and untracked
/// files inside a checkout, are NOT recoverable by git, and this function now
/// reports them as recoverable anyway.** `RecoverableFromGit` under this table
/// therefore means "inside a work tree", not "git holds these exact bytes" —
/// see [`Recovery::RecoverableFromGit`].
///
/// What did **not** change is the `RepoProbe::Undetermined` row. "We could not
/// determine whether this path is inside a work tree" is not "it is inside
/// one", so it still resolves restrictively (CLAUDE.md §3). Only a *positive*
/// observation of being inside a checkout relaxes the answer. The control test
/// `undetermined_repo_probe_never_reports_recoverable` pins that boundary.
pub fn decide_recovery(exists: bool, repo: RepoProbe, git: GitState) -> Recovery {
    if !exists {
        return Recovery::NothingToDestroy;
    }
    // `git` is deliberately unused for the `Repo` arm: the operator's ruling
    // makes "inside a work tree" the whole answer. It stays in the signature
    // because `RepoProbe::NotRepo`/`Undetermined` callers still compute it and
    // because narrowing the relaxation later must not be an API change.
    let _ = git;
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
        // Every git state, including Untracked, TrackedDirty and a `git status`
        // that did not answer. Operator ruling 2026-09-18 — see above.
        RepoProbe::Repo => Recovery::RecoverableFromGit,
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

/// What git said about everything at or below one path, for the project
/// deletion classes of [`crate::deletion`] (spec 3aa215e1 classes 4 and 5).
///
/// Raw observations only; the decision lives in
/// [`crate::deletion::decide_project`], which is pure and unit-tested row by
/// row. Every field comes from a git invocation that exited with a code this
/// module reasoned about — a probe that could not produce ALL of them returns
/// `Undetermined` instead of a partial value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeGitFacts {
    /// `git rev-parse --show-toplevel`, as git printed it (trimmed).
    pub toplevel: String,
    /// `git status --porcelain=v1 -z --ignored=matching --untracked-files=all`
    /// entries at or below the path: the two-letter `XY` code and the path.
    /// Empty = every byte at or below the path is in HEAD (with the index and
    /// ignore rules as git applied them).
    pub status: Vec<(String, String)>,
    /// Number of index entries at or below the path (`git ls-files`).
    pub tracked: usize,
    /// Some index entry at or below the path is a gitlink (mode `160000`, a
    /// submodule).
    pub submodule: bool,
    /// Some index entry at or below the path carries a tag other than `H`
    /// (`git ls-files -v`): assume-unchanged (lowercase), skip-worktree (`S`),
    /// unmerged, … — states in which `git status` may not report a local
    /// modification, so its silence proves nothing there.
    pub hidden_index_state: bool,
    /// For each build-output candidate the caller passed that lies strictly
    /// inside `toplevel`: whether `git check-ignore` reports it ignored
    /// (tracked paths are never reported ignored by `check-ignore`).
    /// Candidates outside `toplevel` are omitted.
    pub build_dirs: Vec<(String, bool)>,
}

/// The injected form of [`probe_tree`], so [`crate::scope::SafeRoots`] can
/// carry it and unit tests can model a repository without creating one.
///
/// Arguments: the canonical absolute path of the operand, and the
/// build-output directory candidates (canonical absolute paths) to ask
/// `check-ignore` about.
pub type GitTreeProbe = fn(&str, &[String]) -> Determination<TreeGitFacts>;

/// Timeout for each git invocation of [`probe_tree`].
const TREE_GIT_TIMEOUT: Duration = Duration::from_millis(1500);

/// Environment variables that would point git at a different repository,
/// index or object store than the one the path lives in. Removed so the probe
/// answers about the tree on disk and not about whatever the hook inherited.
const GIT_REDIRECT_ENV: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_COMMON_DIR",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_NAMESPACE",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG_COUNT",
];

/// A `git` command for `dir` with the settings that can make `git status`
/// under-report switched off (`core.fsmonitor`, `core.untrackedCache`) and,
/// when `literal`, pathspec magic disabled (`--literal-pathspecs`).
/// `check-ignore` rejects `--literal-pathspecs` outright (exit 128, "pathspec
/// magic not supported by this command" — observed), so it is called with
/// `literal = false`; its arguments are paths this crate built from literal
/// operands (no glob characters reach here — `scope`'s literal-path check).
fn tree_git(dir: &Path, literal: bool) -> std::process::Command {
    let mut cmd = std::process::Command::new("git");
    for var in GIT_REDIRECT_ENV {
        cmd.env_remove(var);
    }
    if literal {
        cmd.arg("--literal-pathspecs");
    }
    cmd.args([
        "-c",
        "core.fsmonitor=false",
        "-c",
        "core.untrackedCache=false",
    ])
    .arg("-C")
    .arg(dir);
    cmd
}

/// Run `cmd` bounded by [`TREE_GIT_TIMEOUT`]; stdout only for an exit code in
/// `ok_codes`, together with the code. Anything else is `Undetermined`.
fn run_tree_git(cmd: &mut std::process::Command, ok_codes: &[i32]) -> Determination<(i32, String)> {
    match harness_core::boundary::run_with_timeout(cmd, TREE_GIT_TIMEOUT) {
        Determination::Known(out) => {
            let code = out.code();
            match out.stdout_allowing(ok_codes) {
                Determination::Known(stdout) => Determination::known((code, stdout)),
                Determination::Undetermined(u) => Determination::Undetermined(u),
            }
        }
        Determination::Undetermined(u) => Determination::Undetermined(u),
    }
}

/// Parse `git status --porcelain=v1 -z` output. `None` = not in the shape
/// porcelain v1 promises (a record shorter than `XY path`), which the caller
/// reads as `Undetermined`. A rename/copy record (`R`/`C` in either column)
/// carries a second NUL-terminated path, which is consumed.
pub fn parse_status_z(stdout: &str) -> Option<Vec<(String, String)>> {
    let mut out = Vec::new();
    let mut fields = stdout.split('\0').filter(|f| !f.is_empty());
    while let Some(rec) = fields.next() {
        if rec.len() < 4 || rec.as_bytes()[2] != b' ' || !rec.is_char_boundary(2) {
            return None;
        }
        let xy = rec[..2].to_string();
        let path = rec[3..].to_string();
        if xy.contains('R') || xy.contains('C') {
            fields.next()?;
        }
        out.push((xy, path));
    }
    Some(out)
}

/// Parse `git ls-files -z -s -v` output into (entry count, any gitlink, any
/// tag other than `H`). `None` = a record not shaped `TAG MODE SHA STAGE\tPATH`.
pub fn parse_ls_files_sv_z(stdout: &str) -> Option<(usize, bool, bool)> {
    let mut count = 0;
    let mut submodule = false;
    let mut hidden = false;
    for rec in stdout.split('\0').filter(|f| !f.is_empty()) {
        let (meta, _path) = rec.split_once('\t')?;
        let mut parts = meta.split(' ');
        let tag = parts.next()?;
        let mode = parts.next()?;
        if tag.is_empty() || mode.is_empty() {
            return None;
        }
        count += 1;
        if mode == "160000" {
            submodule = true;
        }
        if tag != "H" {
            hidden = true;
        }
    }
    Some((count, submodule, hidden))
}

/// The wired [`GitTreeProbe`]. Real filesystem and subprocess I/O; only the
/// hook binary attaches it ([`crate::scope::SafeRoots::with_git_tree_probe`]).
///
/// `Undetermined` — which the caller reads as "not Allow" — when the path does
/// not exist (`lstat` fails), when it is not inside a git work tree, and when
/// any git invocation fails, times out, exits with a code not listed here, or
/// prints something unparseable. Each invocation is bounded by
/// [`TREE_GIT_TIMEOUT`].
pub fn probe_tree(path: &str, build_candidates: &[String]) -> Determination<TreeGitFacts> {
    let p = Path::new(path);
    let meta = match std::fs::symlink_metadata(p) {
        Ok(m) => m,
        Err(e) => {
            return Determination::undetermined(format!(
                "`{path}` could not be examined ({e}), so what deleting it would lose is unknown"
            ))
        }
    };
    let dir: &Path = if meta.file_type().is_dir() {
        p
    } else {
        match p.parent() {
            Some(d) => d,
            None => return Determination::undetermined("path has no parent directory"),
        }
    };
    let toplevel = match run_tree_git(
        tree_git(dir, true).args(["rev-parse", "--show-toplevel"]),
        &[0],
    ) {
        Determination::Known((_, out)) => out.trim().to_string(),
        Determination::Undetermined(u) => return Determination::Undetermined(u),
    };
    if !toplevel.starts_with('/') {
        return Determination::undetermined("git printed a work-tree root that is not absolute");
    }
    let status_out = match run_tree_git(
        tree_git(dir, true)
            .args([
                "status",
                "--porcelain=v1",
                "-z",
                "--ignored=matching",
                "--untracked-files=all",
                "--",
            ])
            .arg(path),
        &[0],
    ) {
        Determination::Known((_, out)) => out,
        Determination::Undetermined(u) => return Determination::Undetermined(u),
    };
    let Some(status) = parse_status_z(&status_out) else {
        return Determination::undetermined("git status printed a record blastguard cannot parse");
    };
    let ls_out = match run_tree_git(
        tree_git(dir, true)
            .args(["ls-files", "-z", "-s", "-v", "--"])
            .arg(path),
        &[0],
    ) {
        Determination::Known((_, out)) => out,
        Determination::Undetermined(u) => return Determination::Undetermined(u),
    };
    let Some((tracked, submodule, hidden_index_state)) = parse_ls_files_sv_z(&ls_out) else {
        return Determination::undetermined(
            "git ls-files printed a record blastguard cannot parse",
        );
    };
    let top_prefix = format!("{}/", toplevel.trim_end_matches('/'));
    let mut build_dirs = Vec::new();
    for cand in build_candidates {
        if !cand.starts_with(&top_prefix) {
            continue;
        }
        // `check-ignore` exits 0 = ignored, 1 = not ignored; anything else
        // (128: not a repo, a path outside it, …) is not an answer.
        match run_tree_git(
            tree_git(dir, false)
                .args(["check-ignore", "-q", "--"])
                .arg(cand),
            &[0, 1],
        ) {
            Determination::Known((code, _)) => build_dirs.push((cand.clone(), code == 0)),
            Determination::Undetermined(u) => return Determination::Undetermined(u),
        }
    }
    Determination::known(TreeGitFacts {
        toplevel,
        status,
        tracked,
        submodule,
        hidden_index_state,
        build_dirs,
    })
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

    /// **The 2026-09-18 operator ruling, pinned.** Being *inside a git work
    /// tree* is the whole answer for this axis: every `GitState` row under
    /// `RepoProbe::Repo` resolves to `RecoverableFromGit`, so nothing in a
    /// checkout is ever denied or asked about.
    ///
    /// This replaced three rows that used to answer `Unrecoverable` /
    /// `Undetermined` — see `tracked_but_dirty_*`, `untracked_*` and
    /// `undetermined_never_reports_recoverable` below for what each of them
    /// asserted before, and why it changed.
    #[test]
    fn inside_a_work_tree_is_always_recoverable_regardless_of_git_state() {
        for git in [
            GitState::TrackedClean,
            GitState::TrackedDirty,
            GitState::Untracked,
            GitState::Undetermined,
        ] {
            let r = decide_recovery(true, RepoProbe::Repo, git);
            assert_eq!(
                r,
                Recovery::RecoverableFromGit,
                "a path inside a git work tree must never be denied or asked \
                 about (operator ruling 2026-09-18); git state was {git:?}"
            );
            assert!(r.is_recoverable(), "{r:?}");
        }
    }

    /// **CHANGED 2026-09-18** (was `tracked_but_dirty_is_unrecoverable`, which
    /// asserted `Unrecoverable`). The operator ruled that files inside a work
    /// tree are never denied or asked about, uncommitted bytes included. The
    /// protection this gives up is real and is documented on `decide_recovery`.
    #[test]
    fn tracked_but_dirty_is_allowed_inside_a_work_tree() {
        assert_eq!(
            decide_recovery(true, RepoProbe::Repo, GitState::TrackedDirty),
            Recovery::RecoverableFromGit
        );
    }

    /// **CHANGED 2026-09-18** (was `untracked_is_unrecoverable`, which asserted
    /// `Unrecoverable`). This is the row that produced the false deny that
    /// prompted the ruling: a scratch write was refused because the path
    /// blastguard *guessed* at — the cwd, a worktree directory — was untracked.
    #[test]
    fn untracked_is_allowed_inside_a_work_tree() {
        assert_eq!(
            decide_recovery(true, RepoProbe::Repo, GitState::Untracked),
            Recovery::RecoverableFromGit
        );
    }

    /// CONTROL — must hold before and after the 2026-09-18 change.
    ///
    /// Only the `RepoProbe::Repo` arm was relaxed. "We could not determine
    /// whether this path is inside a work tree" is NOT "it is inside one", so
    /// it stays `Undetermined` and resolves restrictively (CLAUDE.md §3).
    /// If this test ever goes green by turning into `RecoverableFromGit`, the
    /// change was too wide.
    #[test]
    fn undetermined_repo_probe_never_reports_recoverable() {
        for git in [
            GitState::TrackedClean,
            GitState::TrackedDirty,
            GitState::Untracked,
            GitState::Undetermined,
        ] {
            let r = decide_recovery(true, RepoProbe::Undetermined, git);
            assert!(matches!(r, Recovery::Undetermined(_)), "{r:?}");
            assert!(
                !r.is_recoverable(),
                "an undetermined repo probe must not read as recoverable"
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

    // --- probe_tree (deletion classes 4/5) ---

    #[test]
    fn parse_status_z_reads_records_and_consumes_rename_sources() {
        assert_eq!(parse_status_z(""), Some(vec![]));
        assert_eq!(
            parse_status_z("?? a\0 M b c\0R  new\0old\0!! d/\0"),
            Some(vec![
                ("??".to_string(), "a".to_string()),
                (" M".to_string(), "b c".to_string()),
                ("R ".to_string(), "new".to_string()),
                ("!!".to_string(), "d/".to_string()),
            ])
        );
        assert_eq!(parse_status_z("xx\0"), None);
        assert_eq!(parse_status_z("R  new\0"), None);
    }

    #[test]
    fn parse_ls_files_counts_and_flags_non_h_tags_and_gitlinks() {
        assert_eq!(parse_ls_files_sv_z(""), Some((0, false, false)));
        let h = "H 100644 e69de29bb2d1d6434b8b29ae775ad8c2e48c5391 0\ta\0";
        assert_eq!(parse_ls_files_sv_z(h), Some((1, false, false)));
        let s = "S 100644 e69de29bb2d1d6434b8b29ae775ad8c2e48c5391 0\ta\0";
        assert_eq!(parse_ls_files_sv_z(s), Some((1, false, true)));
        let low = "h 100644 e69de29bb2d1d6434b8b29ae775ad8c2e48c5391 0\ta\0";
        assert_eq!(parse_ls_files_sv_z(low), Some((1, false, true)));
        let sub = "H 160000 e69de29bb2d1d6434b8b29ae775ad8c2e48c5391 0\tm\0";
        assert_eq!(parse_ls_files_sv_z(sub), Some((1, true, false)));
        assert_eq!(parse_ls_files_sv_z("garbage\0"), None);
    }

    fn git_in(dir: &std::path::Path, args: &[&str]) {
        let ok = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.email=t@t", "-c", "user.name=t"])
            .args(args)
            .status()
            .expect("git runs")
            .success();
        assert!(ok, "git {args:?}");
    }

    #[allow(clippy::panic)]
    fn known_facts(d: Determination<TreeGitFacts>) -> TreeGitFacts {
        match d {
            Determination::Known(f) => f,
            Determination::Undetermined(u) => panic!("expected facts, got {u:?}"),
        }
    }

    /// Against a REAL repository: what each class of content looks like to
    /// the probe.
    #[test]
    fn probe_tree_observes_a_real_repository() {
        let base = std::env::temp_dir().join(format!("bg-tree-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let repo = base.join("r");
        std::fs::create_dir_all(repo.join("src/sub")).unwrap();
        std::fs::create_dir_all(repo.join("target/debug")).unwrap();
        std::fs::create_dir_all(repo.join("secrets")).unwrap();
        std::fs::create_dir_all(base.join("plain")).unwrap();
        std::fs::write(repo.join(".gitignore"), "/target\nsecrets/\n").unwrap();
        std::fs::write(repo.join("src/a.rs"), "a").unwrap();
        std::fs::write(repo.join("src/sub/b.rs"), "b").unwrap();
        std::fs::write(repo.join("target/debug/x"), "x").unwrap();
        std::fs::write(repo.join("secrets/k"), "k").unwrap();
        git_in(&repo, &["init", "-q"]);
        git_in(&repo, &["add", ".gitignore", "src"]);
        git_in(&repo, &["commit", "-qm", "i"]);
        let repo = repo.canonicalize().unwrap();
        let r = |p: &str| format!("{}/{p}", repo.display());

        let clean = known_facts(probe_tree(&r("src"), &[]));
        assert!(clean.status.is_empty(), "{clean:?}");
        assert_eq!(clean.tracked, 2);
        assert!(!clean.submodule && !clean.hidden_index_state);
        assert_eq!(
            std::path::Path::new(&clean.toplevel)
                .canonicalize()
                .unwrap(),
            repo
        );

        let t = known_facts(probe_tree(&r("target/debug"), &[r("target")]));
        assert_eq!(t.tracked, 0);
        assert!(t.status.iter().all(|(xy, _)| xy == "!!"), "{t:?}");
        assert_eq!(t.build_dirs, vec![(r("target"), true)]);
        let tracked_build = known_facts(probe_tree(&r("src"), &[r("src")]));
        assert_eq!(tracked_build.build_dirs, vec![(r("src"), false)]);

        let s = known_facts(probe_tree(&r("secrets"), &[]));
        assert_eq!(s.status, vec![("!!".to_string(), "secrets/".to_string())]);

        std::fs::write(repo.join("src/a.rs"), "changed").unwrap();
        std::fs::write(repo.join("src/sub/new.rs"), "n").unwrap();
        let dirty = known_facts(probe_tree(&r("src"), &[]));
        let codes: Vec<&str> = dirty.status.iter().map(|(xy, _)| xy.as_str()).collect();
        assert!(codes.contains(&" M") && codes.contains(&"??"), "{dirty:?}");

        git_in(&repo, &["update-index", "--skip-worktree", "src/sub/b.rs"]);
        assert!(known_facts(probe_tree(&r("src/sub"), &[])).hidden_index_state);

        // Not a repository, and a path that does not exist: no facts.
        let plain = base.join("plain").canonicalize().unwrap();
        assert!(matches!(
            probe_tree(plain.to_str().unwrap(), &[]),
            Determination::Undetermined(_)
        ));
        assert!(matches!(
            probe_tree(&r("nope"), &[]),
            Determination::Undetermined(_)
        ));
        let _ = std::fs::remove_dir_all(&base);
    }
}
