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

use harness_core::git_probe::{probe_repo_scrubbed, RepoProbe};
use harness_core::verdict::Determination;

/// Whether the bytes at a path survive being destroyed.
#[derive(Debug, Clone, PartialEq, Eq)]
#[must_use]
pub enum Recovery {
    /// The path does not exist. Truncating or writing it destroys nothing —
    /// there are no prior bytes to lose.
    NothingToDestroy,
    /// The path is inside a git work tree, and `git status` answered for it.
    ///
    /// **This is a wider claim than its name suggests, on purpose.** Since the
    /// operator ruling of 2026-09-18 ([`decide_recovery`]), every path inside
    /// a checkout whose git state was *read* lands here — tracked-and-clean,
    /// tracked-with-uncommitted-changes, and untracked. Only the first of those
    /// is literally restorable by `git restore`; the rest are treated as
    /// recoverable by operator decision, not by observation. A path whose
    /// `git status` did not answer does NOT land here (operator ruling
    /// 2026-10-02): it is [`Recovery::Undetermined`].
    ///
    /// The variant keeps its name so the diff of that ruling stays legible, but
    /// do not read it as "git holds these bytes" — read it as "inside a work
    /// tree, so the COST axis raises no objection". It does not settle the
    /// WHOSE-TREE axis: the truncating-redirect rule in `detect.rs` still
    /// refuses a recoverable target that is neither inside one of the
    /// session's safe roots nor a recognised worktree checkout.
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
            // Nor does it claim the gate never refuses such a path (backlog
            // 2dc70bc5): the redirect rule appends this text to a Deny when the
            // tree is not this session's, so that claim contradicted the verdict.
            Recovery::RecoverableFromGit => "it is inside a git work tree".to_string(),
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
/// | ✓ | `Repo` | `TrackedClean` / `TrackedDirty` / `Untracked` | `RecoverableFromGit` |
/// | ✓ | `Repo` | `Undetermined` | `Undetermined` |
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
///
/// # The `Repo` + `GitState::Undetermined` row (operator ruling 2026-10-02)
///
/// The 2026-09-18 collapse also swept in a `git status` that did not answer
/// (spawn failure, timeout, non-zero exit). The operator ruled on that row
/// separately: *"Can't read -> not recoverable"* (backlog 7778b634). Being
/// inside a work tree relaxes the answer only for a git state that was
/// actually read; an unread one is forwarded as `Undetermined` and resolves
/// restrictively at the call site (CLAUDE.md §3). Pinned by
/// `unreadable_git_state_inside_a_work_tree_is_undetermined` and the
/// fault-injection test `blind_boundary_never_leaves_a_recoverability_probe_permissive`.
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
        // A git status that did not answer is not a read state, and an
        // unread state supports no recoverability claim (ruling 2026-10-02).
        RepoProbe::Repo => match git {
            GitState::Undetermined => Recovery::Undetermined(
                "it exists inside a git work tree, but `git status` did not answer for it, so \
whether these bytes are recoverable is unknown"
                    .to_string(),
            ),
            // Every state git actually reported, including Untracked and
            // TrackedDirty. Operator ruling 2026-09-18 — see above.
            GitState::TrackedClean | GitState::TrackedDirty | GitState::Untracked => {
                Recovery::RecoverableFromGit
            }
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
///
/// Both git children (the repo probe and `git status`) run with the inherited
/// `GIT_*` relocation variables removed ([`harness_core::git_env::scrub`]):
/// the question is which repository, if any, contains `path` on disk, and a
/// hook-inherited `GIT_DIR` would otherwise make git answer about an unrelated
/// repository — a non-repo file read as a clean tracked one and its truncation
/// was allowed (`tests/reversible_probe_inherited_git_dir.rs`).
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
    let repo = probe_repo_scrubbed(dir);
    if !matches!(repo, RepoProbe::Repo) {
        return decide_recovery(true, repo, GitState::Undetermined);
    }
    let mut cmd = std::process::Command::new("git");
    harness_core::git_env::scrub(&mut cmd)
        .current_dir(dir)
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

/// Timeout for each git invocation of [`probe_tree`] (and of
/// [`probe_tree_with`]). Only tests reach a different bound, through
/// [`probe_tree_with_timeout`].
const TREE_GIT_TIMEOUT: Duration = Duration::from_millis(1500);

/// A `git` command for `dir` with the inherited `GIT_*` variables that would
/// point git at a different repository, index or object store removed
/// ([`harness_core::git_env::scrub`] — so the probe answers about the tree on
/// disk and not about whatever the hook inherited), the settings that can make
/// `git status` under-report switched off (`core.fsmonitor`,
/// `core.untrackedCache`) and,
/// when `literal`, pathspec magic disabled (`--literal-pathspecs`).
/// `check-ignore` rejects `--literal-pathspecs` outright (exit 128, "pathspec
/// magic not supported by this command" — observed), so it is called with
/// `literal = false`; its arguments are paths this crate built from literal
/// operands (no glob characters reach here — `scope`'s literal-path check).
fn tree_git(git: &std::ffi::OsStr, dir: &Path, literal: bool) -> std::process::Command {
    let mut cmd = std::process::Command::new(git);
    harness_core::git_env::scrub(&mut cmd);
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

/// Run `cmd` bounded by `timeout` (production: [`TREE_GIT_TIMEOUT`]); stdout
/// only for an exit code in `ok_codes`, together with the code. Anything else
/// — including not finishing within `timeout` — is `Undetermined`.
fn run_tree_git(
    cmd: &mut std::process::Command,
    ok_codes: &[i32],
    timeout: Duration,
) -> Determination<(i32, String)> {
    match harness_core::boundary::run_with_timeout(cmd, timeout) {
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
    probe_tree_with(std::ffi::OsStr::new("git"), path, build_candidates)
}

/// [`probe_tree`] with the `git` program named explicitly — the seam the
/// failure-mode tests use to substitute a git that fails, hangs or prints
/// garbage. Production always passes `git`. Each invocation is bounded by
/// [`TREE_GIT_TIMEOUT`] (it is [`probe_tree_with_timeout`] with that bound).
pub(crate) fn probe_tree_with(
    git: &std::ffi::OsStr,
    path: &str,
    build_candidates: &[String],
) -> Determination<TreeGitFacts> {
    probe_tree_with_timeout(git, path, build_candidates, TREE_GIT_TIMEOUT)
}

/// [`probe_tree_with`] with the per-invocation bound named explicitly — the
/// seam tests use when a git that must ANSWER (a clean fake, a real
/// repository) would otherwise depend on finishing inside the production
/// 1.5 s [`TREE_GIT_TIMEOUT`] on a loaded machine. No production call site
/// reaches it with any bound other than [`TREE_GIT_TIMEOUT`]. A git that does
/// not finish within `timeout` is `Undetermined`, as in production.
pub(crate) fn probe_tree_with_timeout(
    git: &std::ffi::OsStr,
    path: &str,
    build_candidates: &[String],
    timeout: Duration,
) -> Determination<TreeGitFacts> {
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
        tree_git(git, dir, true).args(["rev-parse", "--show-toplevel"]),
        &[0],
        timeout,
    ) {
        Determination::Known((_, out)) => out.trim().to_string(),
        Determination::Undetermined(u) => return Determination::Undetermined(u),
    };
    if !toplevel.starts_with('/') {
        return Determination::undetermined("git printed a work-tree root that is not absolute");
    }
    let status_out = match run_tree_git(
        tree_git(git, dir, true)
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
        timeout,
    ) {
        Determination::Known((_, out)) => out,
        Determination::Undetermined(u) => return Determination::Undetermined(u),
    };
    let Some(status) = parse_status_z(&status_out) else {
        return Determination::undetermined("git status printed a record blastguard cannot parse");
    };
    let ls_out = match run_tree_git(
        tree_git(git, dir, true)
            .args(["ls-files", "-z", "-s", "-v", "--"])
            .arg(path),
        &[0],
        timeout,
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
            tree_git(git, dir, false)
                .args(["check-ignore", "-q", "--"])
                .arg(cand),
            &[0, 1],
            timeout,
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

/// What the index holds at a path that does NOT exist on disk, for the
/// `git checkout [<tree-ish>] -- <pathspec>` verdict
/// ([`crate::detect`]'s checkout arm; backlog fp 2026-11-08).
///
/// Overwriting an absent path destroys no working-tree bytes. The only
/// uncommitted state it can destroy is an index entry that differs from HEAD
/// (a staged version), which `git checkout <rev> -- <path>` overwrites.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AbsentPathIndex {
    /// `git diff --cached --quiet` exited 0: nothing at or below the path is
    /// staged differently from HEAD, so the checkout has nothing uncommitted
    /// to destroy.
    MatchesHead,
    /// `git diff --cached --quiet` exited 1: a staged version at or below the
    /// path differs from HEAD and the checkout would overwrite it.
    DiffersFromHead,
}

/// The injected form of [`probe_absent_path`], carried by
/// [`crate::scope::SafeRoots`] (attached only by the hook binary through
/// [`crate::scope::SafeRoots::with_git_absent_probe`]). The argument is the
/// canonical absolute path of the pathspec.
pub type GitAbsentProbe = fn(&str) -> Determination<AbsentPathIndex>;

/// The wired [`GitAbsentProbe`]. Real filesystem and subprocess I/O.
///
/// `Known` ONLY when ALL of these were observed:
///
///   * `lstat` on `path` fails with `NotFound` (the path exists — even as a
///     dangling symlink — or `lstat` fails any other way: `Undetermined`);
///   * the nearest existing ancestor of `path` is a real directory, and
///     `git rev-parse --show-toplevel` run there exits 0 with an absolute root
///     that `path` lies strictly inside (not a repository, or a path outside
///     it: `Undetermined`);
///   * `git ls-files -z -s -v -- <path>` exits 0, parses, and reports no
///     gitlink and no tag other than `H` (assume-unchanged / skip-worktree /
///     unmerged entries: `Undetermined`);
///   * `git diff --cached --quiet --no-ext-diff --no-textconv -- <path>` exits
///     0 ([`AbsentPathIndex::MatchesHead`]) or 1
///     ([`AbsentPathIndex::DiffersFromHead`]). Any other exit, a failure to
///     run or a timeout is `Undetermined`.
///
/// Every git call uses `--literal-pathspecs` and is bounded by
/// [`TREE_GIT_TIMEOUT`].
pub fn probe_absent_path(path: &str) -> Determination<AbsentPathIndex> {
    probe_absent_path_with_timeout(std::ffi::OsStr::new("git"), path, TREE_GIT_TIMEOUT)
}

/// [`probe_absent_path`] with the `git` program and the per-invocation bound
/// named explicitly (the test seam; production passes `git` and
/// [`TREE_GIT_TIMEOUT`]).
pub(crate) fn probe_absent_path_with_timeout(
    git: &std::ffi::OsStr,
    path: &str,
    timeout: Duration,
) -> Determination<AbsentPathIndex> {
    let p = Path::new(path);
    match std::fs::symlink_metadata(p) {
        Ok(_) => return Determination::undetermined(format!("`{path}` exists on disk")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            return Determination::undetermined(format!("`{path}` could not be examined ({e})"))
        }
    }
    let mut dir: Option<&Path> = None;
    for anc in p.ancestors().skip(1) {
        match std::fs::symlink_metadata(anc) {
            Ok(m) if m.file_type().is_dir() => {
                dir = Some(anc);
                break;
            }
            Ok(_) => {
                return Determination::undetermined(format!(
                    "`{}` is not a directory",
                    anc.display()
                ))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Determination::undetermined(format!(
                    "`{}` could not be examined ({e})",
                    anc.display()
                ))
            }
        }
    }
    let Some(dir) = dir else {
        return Determination::undetermined("no existing ancestor directory");
    };
    let toplevel = match run_tree_git(
        tree_git(git, dir, true).args(["rev-parse", "--show-toplevel"]),
        &[0],
        timeout,
    ) {
        Determination::Known((_, out)) => out.trim().to_string(),
        Determination::Undetermined(u) => return Determination::Undetermined(u),
    };
    if !toplevel.starts_with('/') {
        return Determination::undetermined("git printed a work-tree root that is not absolute");
    }
    if !path.starts_with(&format!("{}/", toplevel.trim_end_matches('/'))) {
        return Determination::undetermined("the path is not inside the work tree git reported");
    }
    let ls_out = match run_tree_git(
        tree_git(git, dir, true)
            .args(["ls-files", "-z", "-s", "-v", "--"])
            .arg(path),
        &[0],
        timeout,
    ) {
        Determination::Known((_, out)) => out,
        Determination::Undetermined(u) => return Determination::Undetermined(u),
    };
    match parse_ls_files_sv_z(&ls_out) {
        Some((_, false, false)) => {}
        Some(_) => {
            return Determination::undetermined(
                "an index entry is a gitlink or assume-unchanged / skip-worktree / unmerged",
            )
        }
        None => {
            return Determination::undetermined(
                "git ls-files printed a record blastguard cannot parse",
            )
        }
    }
    match run_tree_git(
        tree_git(git, dir, true)
            .args([
                "diff",
                "--cached",
                "--quiet",
                "--no-ext-diff",
                "--no-textconv",
                "--",
            ])
            .arg(path),
        &[0, 1],
        timeout,
    ) {
        Determination::Known((0, _)) => Determination::known(AbsentPathIndex::MatchesHead),
        Determination::Known((1, _)) => Determination::known(AbsentPathIndex::DiffersFromHead),
        Determination::Known((code, _)) => {
            Determination::undetermined(format!("git diff --cached exited {code}"))
        }
        Determination::Undetermined(u) => Determination::Undetermined(u),
    }
}

/// Whether what `git restore --staged --worktree -- <path>` would discard at
/// one path is already held by a commit (issue #1604, user ruling 2026-11-08):
/// the index blob AND the raw working-tree bytes of the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestoreContent {
    /// Both the index blob and the working-tree blob were observed in some
    /// commit reachable from a ref: the restore loses no content.
    InCommits,
    /// At least one of them is in no commit that blastguard found; the
    /// string names which (`index` / `working tree`) for the reason text.
    NotInAnyCommit(String),
}

/// The injected form of [`probe_restore_content`], carried by
/// [`crate::scope::SafeRoots`] (attached only by the hook binary through
/// [`crate::scope::SafeRoots::with_git_restore_probe`]). The argument is the
/// canonical absolute path of one literal `git restore` pathspec.
pub type GitRestoreProbe = fn(&str) -> Determination<RestoreContent>;

/// Parse `git ls-files -z -s -v -- <path>` output that must describe exactly
/// ONE ordinary file entry at `rel`: tag `H`, mode `100644`/`100755`, stage
/// `0`. Returns the index blob id. `None` for anything else (no entry, more
/// than one — `<path>` named a directory —, a different path, a gitlink or
/// symlink mode, an unmerged stage, an assume-unchanged / skip-worktree tag,
/// a malformed record).
pub(crate) fn parse_single_file_entry(stdout: &str, rel: &str) -> Option<String> {
    let mut recs = stdout.split('\0').filter(|f| !f.is_empty());
    let rec = recs.next()?;
    if recs.next().is_some() {
        return None;
    }
    let (meta, path) = rec.split_once('\t')?;
    let parts: Vec<&str> = meta.split(' ').collect();
    let [tag, mode, sha, stage] = parts.as_slice() else {
        return None;
    };
    if *tag != "H" || !matches!(*mode, "100644" | "100755") || *stage != "0" || path != rel {
        return None;
    }
    is_object_id(sha).then(|| (*sha).to_string())
}

/// A full hexadecimal object id (SHA-1 or SHA-256 length).
fn is_object_id(s: &str) -> bool {
    matches!(s.len(), 40 | 64) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The wired [`GitRestoreProbe`]. Real filesystem and subprocess I/O.
///
/// `Known(InCommits)` ONLY when ALL of these were observed:
///
///   * `lstat` on `path` reports a regular file (absent, a symlink, a
///     directory, or an `lstat` failure: `Undetermined`);
///   * `git rev-parse --show-toplevel`, run in its parent, exits 0 with an
///     absolute root that `path` lies strictly inside;
///   * `git ls-files -z -s -v -- <path>` reports exactly one entry, at that
///     path, tag `H`, mode `100644`/`100755`, stage `0`
///     ([`parse_single_file_entry`]);
///   * `git hash-object --no-filters -- <path>` prints an object id — the id
///     of the RAW working-tree bytes, so a clean/smudge filter or `autocrlf`
///     can only make the bytes look uncommitted (restrictive), never the
///     reverse;
///   * for the index id and the working-tree id: `git cat-file -e <id>` exits
///     0, and `git log --all -n1 --format=%H --find-object=<id> -- <rel>`
///     exits 0 and prints a commit id.
///
/// `cat-file -e` exiting 1 (no such object) or `log` printing nothing is
/// `Known(NotInAnyCommit)`. Any other exit, a timeout or unparseable output is
/// `Undetermined`.
///
/// Why this check, and what it does not prove: `git rev-list --objects --all`
/// would answer "is this blob reachable from any ref" exactly but walks every
/// tree of every commit (too slow on large repositories). `log --find-object`
/// limited to the path walks commits only and diffs just that path (commit
/// graph changed-path filters make it cheap; measured 0.3 s on this
/// repository's 6219 commits, 2026-11-08). It finds a commit in which the blob
/// appears or disappears AT THIS PATH, and such a commit (or its parent) holds
/// the blob. It misses a blob committed only under a different path, or only
/// introduced by a merge commit's resolution (merges are not diffed) — both
/// read as `NotInAnyCommit`, the restrictive side. File mode is not compared:
/// a changed executable bit is not content. Every call is bounded by
/// [`TREE_GIT_TIMEOUT`]; a repository too large to answer inside it is
/// `Undetermined`.
pub fn probe_restore_content(path: &str) -> Determination<RestoreContent> {
    probe_restore_content_with_timeout(std::ffi::OsStr::new("git"), path, TREE_GIT_TIMEOUT)
}

/// [`probe_restore_content`] with the `git` program and the per-invocation
/// bound named explicitly (the test seam; production passes `git` and
/// [`TREE_GIT_TIMEOUT`]).
pub(crate) fn probe_restore_content_with_timeout(
    git: &std::ffi::OsStr,
    path: &str,
    timeout: Duration,
) -> Determination<RestoreContent> {
    let p = Path::new(path);
    match std::fs::symlink_metadata(p) {
        Ok(m) if m.file_type().is_file() => {}
        Ok(_) => {
            return Determination::undetermined(format!("`{path}` is not a regular file"));
        }
        Err(e) => {
            return Determination::undetermined(format!("`{path}` could not be examined ({e})"))
        }
    }
    let Some(dir) = p.parent() else {
        return Determination::undetermined("path has no parent directory");
    };
    let toplevel = match run_tree_git(
        tree_git(git, dir, true).args(["rev-parse", "--show-toplevel"]),
        &[0],
        timeout,
    ) {
        Determination::Known((_, out)) => out.trim().to_string(),
        Determination::Undetermined(u) => return Determination::Undetermined(u),
    };
    if !toplevel.starts_with('/') {
        return Determination::undetermined("git printed a work-tree root that is not absolute");
    }
    let top_prefix = format!("{}/", toplevel.trim_end_matches('/'));
    let Some(rel) = path.strip_prefix(&top_prefix).filter(|r| !r.is_empty()) else {
        return Determination::undetermined("the path is not inside the work tree git reported");
    };
    let top = Path::new(&toplevel);
    let ls_out = match run_tree_git(
        tree_git(git, top, true)
            .args(["ls-files", "-z", "-s", "-v", "--"])
            .arg(rel),
        &[0],
        timeout,
    ) {
        Determination::Known((_, out)) => out,
        Determination::Undetermined(u) => return Determination::Undetermined(u),
    };
    let Some(index_id) = parse_single_file_entry(&ls_out, rel) else {
        return Determination::undetermined(
            "the index does not hold exactly one ordinary, merged, unflagged file entry there",
        );
    };
    let worktree_id = match run_tree_git(
        tree_git(git, top, false)
            .args(["hash-object", "--no-filters", "--"])
            .arg(rel),
        &[0],
        timeout,
    ) {
        Determination::Known((_, out)) => out.trim().to_string(),
        Determination::Undetermined(u) => return Determination::Undetermined(u),
    };
    if !is_object_id(&worktree_id) {
        return Determination::undetermined("git hash-object printed no object id");
    }
    let mut checks = vec![("index", index_id)];
    if checks[0].1 != worktree_id {
        checks.push(("working tree", worktree_id));
    }
    for (which, id) in checks {
        match run_tree_git(
            tree_git(git, top, false).args(["cat-file", "-e", &id]),
            &[0, 1],
            timeout,
        ) {
            Determination::Known((0, _)) => {}
            Determination::Known((1, _)) => {
                return Determination::known(RestoreContent::NotInAnyCommit(which.to_string()))
            }
            Determination::Known((code, _)) => {
                return Determination::undetermined(format!("git cat-file -e exited {code}"))
            }
            Determination::Undetermined(u) => return Determination::Undetermined(u),
        }
        let found = match run_tree_git(
            tree_git(git, top, true)
                .args([
                    "-c",
                    "log.showSignature=false",
                    "log",
                    "--all",
                    "-n1",
                    "--format=%H",
                    "--no-ext-diff",
                    "--no-textconv",
                ])
                .arg(format!("--find-object={id}"))
                .arg("--")
                .arg(rel),
            &[0],
            timeout,
        ) {
            Determination::Known((_, out)) => out.trim().to_string(),
            Determination::Undetermined(u) => return Determination::Undetermined(u),
        };
        if found.is_empty() {
            return Determination::known(RestoreContent::NotInAnyCommit(which.to_string()));
        }
        if !is_object_id(&found) {
            return Determination::undetermined(
                "git log printed something that is not a commit id",
            );
        }
    }
    Determination::known(RestoreContent::InCommits)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bound for test gits that must answer (not hang): wide enough that CPU
    /// load cannot push a correct answer past it, finite so a real hang still
    /// fails the test.
    const ANSWERING_GIT_TIMEOUT: Duration = Duration::from_secs(30);

    /// [`probe_tree`] against the real `git`, under [`ANSWERING_GIT_TIMEOUT`].
    fn probe_real(path: &str, build_candidates: &[String]) -> Determination<TreeGitFacts> {
        probe_tree_with_timeout(
            std::ffi::OsStr::new("git"),
            path,
            build_candidates,
            ANSWERING_GIT_TIMEOUT,
        )
    }

    #[test]
    fn run_tree_git_only_knows_listed_exit_codes_within_the_timeout() {
        let sh = |script: &str| {
            let mut c = std::process::Command::new("sh");
            c.args(["-c", script]);
            c
        };
        // Commands that must ANSWER get a generous bound so a loaded machine
        // cannot turn them into a timeout; only the hang uses the production
        // bound, because exercising it is that case's point.
        assert_eq!(
            run_tree_git(&mut sh("printf ok"), &[0], ANSWERING_GIT_TIMEOUT),
            Determination::known((0, "ok".to_string()))
        );
        assert_eq!(
            run_tree_git(&mut sh("exit 1"), &[0, 1], ANSWERING_GIT_TIMEOUT),
            Determination::known((1, String::new()))
        );
        assert!(matches!(
            run_tree_git(
                &mut sh("printf partial; exit 128"),
                &[0],
                ANSWERING_GIT_TIMEOUT
            ),
            Determination::Undetermined(_)
        ));
        assert!(matches!(
            run_tree_git(&mut sh("exec sleep 5"), &[0], TREE_GIT_TIMEOUT),
            Determination::Undetermined(_)
        ));
    }

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
    /// tree* is the whole answer for this axis for every git state that was
    /// read: each such `GitState` row under `RepoProbe::Repo` resolves to
    /// `RecoverableFromGit`, so nothing in a checkout is denied or asked about.
    ///
    /// This replaced rows that used to answer `Unrecoverable` — see
    /// `tracked_but_dirty_*` and `untracked_*` below. **CHANGED 2026-10-02**:
    /// this loop used to include `GitState::Undetermined`; the operator ruled
    /// that an unreadable git state is not recoverable, so that row moved to
    /// `unreadable_git_state_inside_a_work_tree_is_undetermined`.
    #[test]
    fn inside_a_work_tree_is_recoverable_for_every_read_git_state() {
        for git in [
            GitState::TrackedClean,
            GitState::TrackedDirty,
            GitState::Untracked,
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

    /// **Operator ruling 2026-10-02** (backlog 7778b634): *"Can't read -> not
    /// recoverable"*. A `git status` that did not answer must be forwarded as
    /// `Undetermined`, never collapsed into `RecoverableFromGit`.
    #[test]
    fn unreadable_git_state_inside_a_work_tree_is_undetermined() {
        let r = decide_recovery(true, RepoProbe::Repo, GitState::Undetermined);
        assert!(matches!(r, Recovery::Undetermined(_)), "{r:?}");
        assert!(
            !r.is_recoverable(),
            "git did not answer, yet the path was judged recoverable: {r:?}"
        );
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
        // Fixture git must not follow a GIT_DIR/GIT_INDEX_FILE/... inherited
        // from a git hook running the suite.
        let mut cmd = std::process::Command::new("git");
        let ok = harness_core::git_env::scrub(&mut cmd)
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
    /// the probe (the real git must answer, so under [`ANSWERING_GIT_TIMEOUT`]
    /// rather than the production bound).
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

        let clean = known_facts(probe_real(&r("src"), &[]));
        assert!(clean.status.is_empty(), "{clean:?}");
        assert_eq!(clean.tracked, 2);
        assert!(!clean.submodule && !clean.hidden_index_state);
        assert_eq!(
            std::path::Path::new(&clean.toplevel)
                .canonicalize()
                .unwrap(),
            repo
        );

        let t = known_facts(probe_real(&r("target/debug"), &[r("target")]));
        assert_eq!(t.tracked, 0);
        assert!(t.status.iter().all(|(xy, _)| xy == "!!"), "{t:?}");
        assert_eq!(t.build_dirs, vec![(r("target"), true)]);
        let tracked_build = known_facts(probe_real(&r("src"), &[r("src")]));
        assert_eq!(tracked_build.build_dirs, vec![(r("src"), false)]);

        let s = known_facts(probe_real(&r("secrets"), &[]));
        assert_eq!(s.status, vec![("!!".to_string(), "secrets/".to_string())]);

        std::fs::write(repo.join("src/a.rs"), "changed").unwrap();
        std::fs::write(repo.join("src/sub/new.rs"), "n").unwrap();
        let dirty = known_facts(probe_real(&r("src"), &[]));
        let codes: Vec<&str> = dirty.status.iter().map(|(xy, _)| xy.as_str()).collect();
        assert!(codes.contains(&" M") && codes.contains(&"??"), "{dirty:?}");

        git_in(&repo, &["update-index", "--skip-worktree", "src/sub/b.rs"]);
        assert!(known_facts(probe_real(&r("src/sub"), &[])).hidden_index_state);

        // Not a repository, and a path that does not exist: no facts.
        let plain = base.join("plain").canonicalize().unwrap();
        assert!(matches!(
            probe_real(plain.to_str().unwrap(), &[]),
            Determination::Undetermined(_)
        ));
        assert!(matches!(
            probe_real(&r("nope"), &[]),
            Determination::Undetermined(_)
        ));
        let _ = std::fs::remove_dir_all(&base);
    }

    // --- issue #1604: probe_restore_content / parse_single_file_entry ---
    // Written by an independent test writer, not the implementer.

    const OID_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const OID_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn entry(tag: &str, mode: &str, oid: &str, stage: &str, path: &str) -> String {
        format!("{tag} {mode} {oid} {stage}\t{path}\0")
    }

    #[test]
    fn parse_single_file_entry_accepts_only_one_ordinary_merged_entry_at_the_path() {
        // Controls: the two accepted shapes.
        let ok = entry("H", "100644", OID_A, "0", "a.sh");
        assert_eq!(
            parse_single_file_entry(&ok, "a.sh"),
            Some(OID_A.to_string())
        );
        let exe = entry("H", "100755", OID_A, "0", "d/a.sh");
        assert_eq!(
            parse_single_file_entry(&exe, "d/a.sh"),
            Some(OID_A.to_string())
        );
        let sha256 = "c".repeat(64);
        assert_eq!(
            parse_single_file_entry(&entry("H", "100644", &sha256, "0", "a"), "a"),
            Some(sha256.clone())
        );
        // No entry at all (untracked / absent).
        assert_eq!(parse_single_file_entry("", "a.sh"), None);
        // Two records: the pathspec named a directory (or matched twice).
        let two = format!(
            "{}{}",
            entry("H", "100644", OID_A, "0", "d/a"),
            entry("H", "100644", OID_B, "0", "d/b")
        );
        assert_eq!(parse_single_file_entry(&two, "d/a"), None);
        // Unmerged stages, singly and as the usual 1/2/3 triple.
        for stage in ["1", "2", "3"] {
            let un = entry("H", "100644", OID_A, stage, "a.sh");
            assert_eq!(parse_single_file_entry(&un, "a.sh"), None, "stage {stage}");
        }
        let triple = format!(
            "{}{}{}",
            entry("H", "100644", OID_A, "1", "a.sh"),
            entry("H", "100644", OID_B, "2", "a.sh"),
            entry("H", "100644", OID_A, "3", "a.sh")
        );
        assert_eq!(parse_single_file_entry(&triple, "a.sh"), None);
        // Flag tags: skip-worktree `S`, assume-unchanged lowercase, others.
        for tag in ["S", "h", "s", "M", "R", "C", "K", "?"] {
            let t = entry(tag, "100644", OID_A, "0", "a.sh");
            assert_eq!(parse_single_file_entry(&t, "a.sh"), None, "tag {tag}");
        }
        // Symlink and gitlink modes, and other non-file modes.
        for mode in ["120000", "160000", "040000", "100664", "644"] {
            let m = entry("H", mode, OID_A, "0", "a.sh");
            assert_eq!(parse_single_file_entry(&m, "a.sh"), None, "mode {mode}");
        }
        // A record for a different path (prefix, suffix, case).
        for path in ["a.sh.bak", "b/a.sh", "A.sh", "a", ""] {
            let p = entry("H", "100644", OID_A, "0", path);
            assert_eq!(parse_single_file_entry(&p, "a.sh"), None, "path {path:?}");
        }
        // Malformed: short oid, non-hex oid, missing tab, extra field.
        assert_eq!(
            parse_single_file_entry(&entry("H", "100644", "abc", "0", "a.sh"), "a.sh"),
            None
        );
        let nonhex = "g".repeat(40);
        assert_eq!(
            parse_single_file_entry(&entry("H", "100644", &nonhex, "0", "a.sh"), "a.sh"),
            None
        );
        assert_eq!(
            parse_single_file_entry(&format!("H 100644 {OID_A} 0 a.sh\0"), "a.sh"),
            None
        );
        assert_eq!(
            parse_single_file_entry(&format!("H 100644 {OID_A} 0 x\ta.sh\0"), "a.sh"),
            None
        );
        assert_eq!(parse_single_file_entry("garbage\0", "a.sh"), None);
    }

    /// Real git under [`ANSWERING_GIT_TIMEOUT`].
    fn restore_probe_real(path: &str) -> Determination<RestoreContent> {
        probe_restore_content_with_timeout(std::ffi::OsStr::new("git"), path, ANSWERING_GIT_TIMEOUT)
    }

    /// Fixture git with hooks disabled, so a global hooksPath cannot run
    /// anything inside the temp repository.
    fn rgit(dir: &std::path::Path, args: &[&str]) -> String {
        let mut cmd = std::process::Command::new("git");
        let out = harness_core::git_env::scrub(&mut cmd)
            .arg("-C")
            .arg(dir)
            .args([
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "commit.gpgSign=false",
            ])
            .args(args)
            .output()
            .expect("git runs");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// A fresh repo `<tmp>/bg-restore-<tag>-<pid>/r` with `a.sh` = "base\n"
    /// committed. Returns (scratch base, canonical repo).
    fn restore_repo(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let base = std::env::temp_dir().join(format!("bg-restore-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let repo = base.join("r");
        std::fs::create_dir_all(&repo).unwrap();
        rgit(&repo, &["init", "-q"]);
        std::fs::write(repo.join("a.sh"), "base\n").unwrap();
        rgit(&repo, &["add", "a.sh"]);
        rgit(&repo, &["commit", "-qm", "base"]);
        let repo = repo.canonicalize().unwrap();
        (base, repo)
    }

    fn at(repo: &std::path::Path, rel: &str) -> String {
        format!("{}/{rel}", repo.display())
    }

    fn assert_undetermined(d: &Determination<RestoreContent>, what: &str) {
        assert!(
            matches!(d, Determination::Undetermined(_)),
            "{what}: expected Undetermined, got {d:?}"
        );
    }

    #[test]
    fn restore_probe_clean_committed_file_is_in_commits() {
        let (base, repo) = restore_repo("clean");
        assert_eq!(
            restore_probe_real(&at(&repo, "a.sh")),
            Determination::known(RestoreContent::InCommits)
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn restore_probe_uncommitted_worktree_change_is_not_in_any_commit() {
        let (base, repo) = restore_repo("wtdirty");
        // Index still == HEAD; only the working tree holds new bytes.
        std::fs::write(repo.join("a.sh"), "edited, never committed\n").unwrap();
        assert_eq!(
            restore_probe_real(&at(&repo, "a.sh")),
            Determination::known(RestoreContent::NotInAnyCommit("working tree".to_string()))
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn restore_probe_staged_but_never_committed_is_not_in_any_commit() {
        let (base, repo) = restore_repo("staged");
        std::fs::write(repo.join("a.sh"), "staged, never committed\n").unwrap();
        rgit(&repo, &["add", "a.sh"]);
        // The blob now EXISTS in the object store (cat-file -e succeeds), but
        // no commit holds it.
        assert_eq!(
            restore_probe_real(&at(&repo, "a.sh")),
            Determination::known(RestoreContent::NotInAnyCommit("index".to_string()))
        );
        // Staged one version, then edited the worktree again: the index is
        // the first thing in no commit.
        std::fs::write(repo.join("a.sh"), "edited again\n").unwrap();
        assert!(
            matches!(
                restore_probe_real(&at(&repo, "a.sh")),
                Determination::Known(RestoreContent::NotInAnyCommit(_))
            ),
            "staged + further edited must not be InCommits"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn restore_probe_staged_committed_content_with_dirty_worktree_is_not_in_any_commit() {
        // The index holds content that IS committed (an older version), but
        // the working tree holds bytes no commit has: still NotInAnyCommit.
        let (base, repo) = restore_repo("idxold");
        std::fs::write(repo.join("a.sh"), "v2\n").unwrap();
        rgit(&repo, &["commit", "-qam", "v2"]);
        rgit(&repo, &["checkout", "-q", "HEAD~1", "--", "a.sh"]);
        std::fs::write(repo.join("a.sh"), "uncommitted\n").unwrap();
        assert_eq!(
            restore_probe_real(&at(&repo, "a.sh")),
            Determination::known(RestoreContent::NotInAnyCommit("working tree".to_string()))
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn restore_probe_content_from_an_older_overwritten_commit_is_in_commits() {
        let (base, repo) = restore_repo("history");
        std::fs::write(repo.join("a.sh"), "v2\n").unwrap();
        rgit(&repo, &["commit", "-qam", "v2"]);
        std::fs::write(repo.join("a.sh"), "v3\n").unwrap();
        rgit(&repo, &["commit", "-qam", "v3"]);
        // Index AND worktree back to the base bytes (only in the first commit,
        // overwritten twice since).
        std::fs::write(repo.join("a.sh"), "base\n").unwrap();
        rgit(&repo, &["add", "a.sh"]);
        assert_eq!(
            restore_probe_real(&at(&repo, "a.sh")),
            Determination::known(RestoreContent::InCommits)
        );
        // Index at v2 (middle commit), worktree at base (first commit).
        std::fs::write(repo.join("a.sh"), "v2\n").unwrap();
        rgit(&repo, &["add", "a.sh"]);
        std::fs::write(repo.join("a.sh"), "base\n").unwrap();
        assert_eq!(
            restore_probe_real(&at(&repo, "a.sh")),
            Determination::known(RestoreContent::InCommits)
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn restore_probe_content_on_another_branch_is_in_commits() {
        let (base, repo) = restore_repo("branch");
        let main = rgit(&repo, &["rev-parse", "--abbrev-ref", "HEAD"]);
        rgit(&repo, &["checkout", "-q", "-b", "other"]);
        std::fs::write(repo.join("a.sh"), "on other\n").unwrap();
        rgit(&repo, &["commit", "-qam", "other"]);
        rgit(&repo, &["checkout", "-q", main.trim()]);
        std::fs::write(repo.join("a.sh"), "on other\n").unwrap();
        rgit(&repo, &["add", "a.sh"]);
        assert_eq!(
            restore_probe_real(&at(&repo, "a.sh")),
            Determination::known(RestoreContent::InCommits)
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn restore_probe_content_only_in_an_unreachable_commit_is_not_in_any_commit() {
        // Committed, then amended away: the commit survives only in the
        // reflog, which is not a ref. The ruling requires a commit reachable
        // from a ref.
        let (base, repo) = restore_repo("amended");
        std::fs::write(repo.join("a.sh"), "amended away\n").unwrap();
        rgit(&repo, &["commit", "-qam", "tmp"]);
        std::fs::write(repo.join("a.sh"), "replacement\n").unwrap();
        rgit(&repo, &["commit", "-q", "--amend", "-am", "tmp2"]);
        std::fs::write(repo.join("a.sh"), "amended away\n").unwrap();
        rgit(&repo, &["add", "a.sh"]);
        assert!(
            matches!(
                restore_probe_real(&at(&repo, "a.sh")),
                Determination::Known(RestoreContent::NotInAnyCommit(_))
            ),
            "content held only by a reflog entry must not count as committed"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn restore_probe_untracked_directory_absent_symlink_and_flagged_are_undetermined() {
        use std::os::unix::fs::symlink;
        let (base, repo) = restore_repo("undet");
        std::fs::write(repo.join("untracked.sh"), "u\n").unwrap();
        assert_undetermined(&restore_probe_real(&at(&repo, "untracked.sh")), "untracked");

        std::fs::create_dir_all(repo.join("d")).unwrap();
        std::fs::write(repo.join("d/x"), "x\n").unwrap();
        rgit(&repo, &["add", "d/x"]);
        rgit(&repo, &["commit", "-qm", "d"]);
        assert_undetermined(&restore_probe_real(&at(&repo, "d")), "directory");

        assert_undetermined(&restore_probe_real(&at(&repo, "nope.sh")), "absent");
        // Tracked, then deleted from the working tree.
        std::fs::remove_file(repo.join("d/x")).unwrap();
        assert_undetermined(&restore_probe_real(&at(&repo, "d/x")), "deleted tracked");

        symlink("a.sh", repo.join("l")).unwrap();
        rgit(&repo, &["add", "l"]);
        rgit(&repo, &["commit", "-qm", "l"]);
        assert_undetermined(&restore_probe_real(&at(&repo, "l")), "tracked symlink");

        rgit(&repo, &["update-index", "--skip-worktree", "a.sh"]);
        assert_undetermined(&restore_probe_real(&at(&repo, "a.sh")), "skip-worktree");
        rgit(&repo, &["update-index", "--no-skip-worktree", "a.sh"]);
        rgit(&repo, &["update-index", "--assume-unchanged", "a.sh"]);
        assert_undetermined(&restore_probe_real(&at(&repo, "a.sh")), "assume-unchanged");
        rgit(&repo, &["update-index", "--no-assume-unchanged", "a.sh"]);
        // Control: the same file without the flag answers.
        assert_eq!(
            restore_probe_real(&at(&repo, "a.sh")),
            Determination::known(RestoreContent::InCommits)
        );

        // Outside any repository.
        let plain = base.join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        std::fs::write(plain.join("f"), "f\n").unwrap();
        let plain = plain.canonicalize().unwrap();
        assert_undetermined(&restore_probe_real(&at(&plain, "f")), "not a repository");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn restore_probe_unmerged_path_is_undetermined() {
        let (base, repo) = restore_repo("unmerged");
        let main = rgit(&repo, &["rev-parse", "--abbrev-ref", "HEAD"]);
        rgit(&repo, &["checkout", "-q", "-b", "side"]);
        std::fs::write(repo.join("a.sh"), "side\n").unwrap();
        rgit(&repo, &["commit", "-qam", "side"]);
        rgit(&repo, &["checkout", "-q", main.trim()]);
        std::fs::write(repo.join("a.sh"), "main\n").unwrap();
        rgit(&repo, &["commit", "-qam", "main"]);
        // Expected to conflict; ignore its exit status.
        let mut cmd = std::process::Command::new("git");
        let _ = harness_core::git_env::scrub(&mut cmd)
            .arg("-C")
            .arg(&repo)
            .args(["-c", "user.email=t@t", "-c", "user.name=t"])
            .args(["-c", "core.hooksPath=/dev/null", "merge", "-q", "side"])
            .output();
        let ls = rgit(&repo, &["ls-files", "-s", "--", "a.sh"]);
        assert!(ls.lines().count() > 1, "fixture must be unmerged: {ls}");
        assert_undetermined(&restore_probe_real(&at(&repo, "a.sh")), "unmerged");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// A fake git answering every step of the restore probe. Each argument is
    /// the shell body for that step; the defaults (see
    /// [`restore_fake_clean`]) make the probe answer `InCommits`.
    struct FakeSteps {
        rev_parse: String,
        ls_files: String,
        hash_object: String,
        cat_file: String,
        log: String,
    }

    fn restore_fake_clean(top: &str) -> FakeSteps {
        FakeSteps {
            rev_parse: format!("echo '{top}'"),
            ls_files: format!("printf 'H 100644 {OID_A} 0\\ta.sh\\000'"),
            hash_object: format!("echo {OID_A}"),
            cat_file: "exit 0".to_string(),
            log: format!("echo {OID_B}"),
        }
    }

    fn write_fake_git(bin: &std::path::Path, name: &str, s: &FakeSteps) -> String {
        use std::os::unix::fs::PermissionsExt;
        let script = format!(
            "#!/bin/sh\ncase \"$*\" in\n  *rev-parse*) {} ;;\n  *ls-files*) {} ;;\n  *hash-object*) {} ;;\n  *cat-file*) {} ;;\n  *\" log \"*) {} ;;\n  *) exit 97 ;;\nesac\n",
            s.rev_parse, s.ls_files, s.hash_object, s.cat_file, s.log
        );
        let p = bin.join(name);
        std::fs::write(&p, script).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p.to_str().unwrap().to_string()
    }

    #[test]
    fn restore_probe_failing_garbage_or_hanging_git_is_never_in_commits() {
        let base = std::env::temp_dir().join(format!("bg-restore-fake-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let repo = base.join("r");
        let bin = base.join("bin");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(repo.join("a.sh"), "x\n").unwrap();
        let repo = repo.canonicalize().unwrap();
        let top = repo.to_str().unwrap().to_string();
        let file = at(&repo, "a.sh");
        let probe = |git: &str, timeout: Duration| {
            probe_restore_content_with_timeout(std::ffi::OsStr::new(git), &file, timeout)
        };

        // Control: the fake answers every step -> InCommits, so each variant
        // below differs from a reaching fixture in exactly one step.
        let clean = write_fake_git(&bin, "git-clean", &restore_fake_clean(&top));
        assert_eq!(
            probe(&clean, ANSWERING_GIT_TIMEOUT),
            Determination::known(RestoreContent::InCommits)
        );
        // Control: worktree hash differs from the index id -> both checked;
        // still InCommits when both are found.
        let other_hash = format!("echo {OID_B}");
        let mut s = restore_fake_clean(&top);
        s.hash_object = other_hash.clone();
        let two = write_fake_git(&bin, "git-two", &s);
        assert_eq!(
            probe(&two, ANSWERING_GIT_TIMEOUT),
            Determination::known(RestoreContent::InCommits)
        );

        // (step, shell body) — each variant replaces ONE step of the clean
        // fake with a failure, garbage or a hang.
        let partial_log = format!("printf '{OID_B}'; exit 128");
        let undetermined: Vec<(&str, &str, &str)> = vec![
            ("rev-parse exits 128", "rev_parse", "exit 128"),
            ("rev-parse relative", "rev_parse", "echo relative/top"),
            (
                "rev-parse other root",
                "rev_parse",
                "echo /definitely/not/top",
            ),
            ("rev-parse hangs", "rev_parse", "exec sleep 5"),
            ("ls-files exits 128", "ls_files", "exit 128"),
            ("ls-files garbage", "ls_files", "printf garbage"),
            ("ls-files empty", "ls_files", "exit 0"),
            ("hash-object exits 128", "hash_object", "exit 128"),
            ("hash-object garbage", "hash_object", "echo nope"),
            ("hash-object empty", "hash_object", "exit 0"),
            ("cat-file exits 128", "cat_file", "exit 128"),
            ("cat-file exits 2", "cat_file", "exit 2"),
            ("cat-file hangs", "cat_file", "exec sleep 5"),
            ("log exits 128", "log", "exit 128"),
            ("log exits 1", "log", "exit 1"),
            ("log garbage", "log", "echo not-a-commit"),
            ("log prints then fails", "log", &partial_log),
            ("log hangs", "log", "exec sleep 5"),
        ];
        for (i, (name, step, body)) in undetermined.iter().enumerate() {
            let mut s = restore_fake_clean(&top);
            let slot = match *step {
                "rev_parse" => &mut s.rev_parse,
                "ls_files" => &mut s.ls_files,
                "hash_object" => &mut s.hash_object,
                "cat_file" => &mut s.cat_file,
                _ => &mut s.log,
            };
            *slot = (*body).to_string();
            let git = write_fake_git(&bin, &format!("git-u{i}"), &s);
            let timeout = if name.contains("hangs") {
                Duration::from_millis(300)
            } else {
                ANSWERING_GIT_TIMEOUT
            };
            assert_undetermined(&probe(&git, timeout), name);
        }

        // Known negatives the fake can express: no such object, no commit.
        let mut s = restore_fake_clean(&top);
        s.cat_file = "exit 1".to_string();
        let no_obj = write_fake_git(&bin, "git-noobj", &s);
        assert_eq!(
            probe(&no_obj, ANSWERING_GIT_TIMEOUT),
            Determination::known(RestoreContent::NotInAnyCommit("index".to_string()))
        );
        let mut s = restore_fake_clean(&top);
        s.log = "exit 0".to_string();
        let no_commit = write_fake_git(&bin, "git-nocommit", &s);
        assert_eq!(
            probe(&no_commit, ANSWERING_GIT_TIMEOUT),
            Determination::known(RestoreContent::NotInAnyCommit("index".to_string()))
        );
        // Index found, worktree id (different) not found: names the worktree.
        let mut s = restore_fake_clean(&top);
        s.hash_object = other_hash.clone();
        s.cat_file = format!("case \"$*\" in *{OID_B}*) exit 1 ;; *) exit 0 ;; esac");
        let wt = write_fake_git(&bin, "git-wt", &s);
        assert_eq!(
            probe(&wt, ANSWERING_GIT_TIMEOUT),
            Determination::known(RestoreContent::NotInAnyCommit("working tree".to_string()))
        );
        let mut s = restore_fake_clean(&top);
        s.hash_object = other_hash;
        s.log = format!("case \"$*\" in *{OID_B}*) exit 0 ;; *) echo {OID_B} ;; esac");
        let wtl = write_fake_git(&bin, "git-wtl", &s);
        assert_eq!(
            probe(&wtl, ANSWERING_GIT_TIMEOUT),
            Determination::known(RestoreContent::NotInAnyCommit("working tree".to_string()))
        );
        // A git program that does not exist at all.
        assert_undetermined(
            &probe(
                bin.join("no-such-git").to_str().unwrap(),
                ANSWERING_GIT_TIMEOUT,
            ),
            "missing git",
        );
        let _ = std::fs::remove_dir_all(&base);
    }
}
