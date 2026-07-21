//! Shared "is this a git repository?" probe for the gate crates.
//!
//! This exists because the probe was copy-pasted into four crates
//! (`tdd`, `donegate`, `reviewgate`, `propguard`) and every copy carried the
//! same defect: `git rev-parse --is-inside-work-tree` was reduced to a `bool`
//! with `.unwrap_or(false)` / `.is_some()`, so **"git could not be run" became
//! "this is not a git repository"** — which three of the four consumers map
//! straight to `allow`. A PATH-restricted hook, a fork failure under EMFILE, or
//! a repo git refuses to open (dubious ownership, corruption) therefore
//! disabled the gate silently. Four copies also meant a fix could land on one
//! mirror and leave the other three, which is exactly what happened before.
//!
//! The fix is not "default to `true`" — that would block every legitimately
//! non-git project. It is to stop guessing and **corroborate with independent
//! evidence**: when git cannot answer, look for a `.git` entry on the
//! filesystem. No `.git` anywhere above the path is a real observation that the
//! gate is out of scope. A `.git` that exists while git refuses to talk about
//! it is `Undetermined`, and undetermined resolves to the restricted side.

use std::path::Path;

/// Whether a path is inside a git work tree, keeping "we could not tell" as its
/// own answer instead of folding it into "no".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepoProbe {
    /// Confirmed inside a git work tree.
    Repo,
    /// Confirmed *not* in scope — either git said so, or git could not answer
    /// and there is no `.git` anywhere above the path to contradict it.
    NotRepo,
    /// git could not be run, refused to answer, or answered something
    /// unreadable, *and* there is filesystem evidence of a repository. Callers
    /// must treat this as the restricted side (block / undetermined changeset),
    /// never as `NotRepo`.
    Undetermined,
}

/// Pure decision core — no IO, so every row of the table below is unit-testable.
///
/// * `spawned_ok` — git produced an exit status at all (`false` = spawn/IO error).
/// * `exit_ok` — that exit status was a success. Meaningless when `!spawned_ok`.
/// * `stdout` — git's stdout. Trimmed here; real git prints `true\n`.
/// * `dot_git_found` — independent evidence of a repository (see
///   [`dot_git_present`], plus any `GIT_DIR` override the caller honours).
///
/// | spawned | exit_ok | stdout | .git | → |
/// |---|---|---|---|---|
/// | ✓ | ✓ | `true` | any | `Repo` |
/// | ✓ | ✓ | `false` | any | `NotRepo` (inside a `.git` dir: out of scope) |
/// | ✓ | ✓ | anything else | any | `Undetermined` |
/// | ✓ | ✗ | any | ✓ | `Undetermined` |
/// | ✓ | ✗ | any | ✗ | `NotRepo` |
/// | ✗ | — | — | ✓ | `Undetermined` |
/// | ✗ | — | — | ✗ | `NotRepo` |
///
/// The empty-stdout row matters: a success with nothing on stdout used to be
/// indistinguishable from a success saying `true`, because the old probe read
/// only the exit status. "It exited 0" is not the same as "it answered yes".
pub fn decide_repo_probe(
    spawned_ok: bool,
    exit_ok: bool,
    stdout: &str,
    dot_git_found: bool,
) -> RepoProbe {
    // A spawn failure has no exit status and no stdout; refuse to read either,
    // so a stale value can never manufacture an answer.
    if !spawned_ok {
        return if dot_git_found {
            RepoProbe::Undetermined
        } else {
            RepoProbe::NotRepo
        };
    }
    if !exit_ok {
        // git ran and said no. Believe it only if the filesystem agrees —
        // otherwise this is a repo git declined to open, not a non-repo.
        return if dot_git_found {
            RepoProbe::Undetermined
        } else {
            RepoProbe::NotRepo
        };
    }
    match stdout.trim() {
        "true" => RepoProbe::Repo,
        // Inside the `.git` directory itself. A real, readable answer meaning
        // "no work tree here", so it is genuinely out of scope.
        "false" => RepoProbe::NotRepo,
        // Exited 0 while saying something this code does not understand. That
        // is not a yes and not a no.
        _ => RepoProbe::Undetermined,
    }
}

/// Filesystem-only corroboration: does a `.git` entry exist at `root` or any
/// ancestor? Deliberately independent of the `git` binary — it is the evidence
/// used to decide whether git's silence is credible.
///
/// Matches a `.git` **file** as well as a directory: linked worktrees and
/// submodules record `gitdir: …` in a plain file, and missing that would make
/// every worktree look like a non-repo the moment git stopped responding.
///
/// Does **not** consult `GIT_DIR`; that override is composed in by
/// [`probe_repo`] so this stays a pure filesystem question.
pub fn dot_git_present(root: &Path) -> bool {
    let mut cur = Some(root);
    while let Some(dir) = cur {
        if dir.join(".git").exists() {
            return true;
        }
        cur = dir.parent();
    }
    false
}

/// The wired probe: spawn `git rev-parse --is-inside-work-tree` once in `root`,
/// then adjudicate with [`decide_repo_probe`], corroborating with
/// [`dot_git_present`] or a `GIT_DIR` override.
///
/// `GIT_DIR` counts as evidence of a repository because it explicitly points at
/// one; if it is set and git still cannot answer, "not a repo" is not a
/// credible reading.
pub fn probe_repo(root: &Path) -> RepoProbe {
    let evidence = dot_git_present(root) || std::env::var_os("GIT_DIR").is_some();
    match std::process::Command::new("git")
        .current_dir(root)
        .args(["rev-parse", "--is-inside-work-tree"])
        .output()
    {
        Ok(out) => decide_repo_probe(
            true,
            out.status.success(),
            &String::from_utf8_lossy(&out.stdout),
            evidence,
        ),
        Err(_) => decide_repo_probe(false, false, "", evidence),
    }
}
