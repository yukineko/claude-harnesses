//! Path-trust classifier: is a `Read` target inside the session's **trust
//! domain** (trusted) or outside it (untrusted — `/tmp`, the home dir, another
//! project, or a `..`-escaping relative path)?
//!
//! There is no existing trusted/untrusted path notion in this repo; this is
//! new logic. Kept deliberately simple and conservative: anything that cannot
//! be positively resolved inside the domain is `Untrusted` or `Indeterminate`,
//! never `Trusted` — the caller (`hooks::mark`) treats both non-`Trusted`
//! answers the same way (mark the session tainted), so the only distinction
//! that matters operationally is "inside the domain" vs "everything else".
//!
//! # The trust domain is not the process cwd (0.1.9)
//!
//! Until 0.1.8 the domain was a SINGLE root: the hook's `cwd`. That definition
//! was wrong, not merely narrow, and it was measured wrong (2026-08-05, against
//! the deployed binary, isolated `TAINTGUARD_STATE_DIR`, with an in-repo `Read`
//! and a `Grep` held as anti-vacuity controls that stayed silent in the same
//! run):
//!
//! | tool call                          | 0.1.8 verdict        |
//! |------------------------------------|----------------------|
//! | `Read` of an in-repo file          | silent (control)     |
//! | `Read` of the SESSION WORKTREE     | ask/deny             |
//! | `Read` of the declared SCRATCHPAD  | ask/deny             |
//! | `Read` of the user settings file   | ask/deny             |
//! | `WebFetch`/`WebSearch`             | ask/deny             |
//! | `Read` with no `file_path`         | ask/deny (correct)   |
//! | `Grep`                             | silent (control)     |
//!
//! Rows 2 and 3 are the defect. CLAUDE.md §8 makes every edit happen in a git
//! worktree, and this harness declares an extra scratchpad directory; both are
//! the SAME trust domain as `cwd` — the same repository, the same operator,
//! content this session itself wrote. Tainting on them is not a gate reporting
//! a risk, it is a gate with a wrong definition of its own subject: within a
//! few tool calls of any session start every write-class tool is downgraded to
//! `ask`/`deny` until `Stop`, which headless means a deadlock (backlog
//! `270f36fa`). So the definition is redefined here — deliberately, in the
//! widening direction, and ONLY in the widening direction that can be
//! positively proven:
//!
//! * (a) `cwd`, as before;
//! * (b) any git worktree of the **same repository** as `cwd`, established by
//!   resolving the git COMMON dir (`git rev-parse --git-common-dir`) — never by
//!   path shape and never by a parent-directory heuristic, so a directory that
//!   merely looks like a worktree, or an unrelated repository that happens to
//!   sit next door, cannot claim trust ([`TrustDomain::classify`]);
//! * (c) roots listed explicitly in [`TRUSTED_ROOTS_ENV`].
//!
//! Everything else is unchanged, on purpose: indeterminate still means
//! untrusted, and both still taint. Widening the region must not soften the
//! fail-closed direction (CLAUDE.md §3), so every branch that could not
//! positively resolve a path INTO the domain — including a git probe that
//! failed — keeps its old, restrictive answer.
//!
//! # What this crate still does NOT do
//!
//! A separately filed permissive-direction defect (backlog `5b0e9fe1`) is
//! deliberately NOT addressed here: the PostToolUse matcher is
//! `WebFetch|WebSearch|Read` and `decide_mark` has a catch-all `Ok(())`, so a
//! `Grep` over an external path, a shell read of an external file, and a shell
//! fetch of a URL all import external content WITHOUT tainting. Nothing in this
//! module narrows that hole; the `Grep` control row above is silent for that
//! reason too, not only because the path was in-repo.

use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

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
        Trust::Trusted
    } else {
        Trust::Untrusted
    }
}

// ---------------------------------------------------------------------------
// The trust domain (0.1.9)
// ---------------------------------------------------------------------------

/// Colon-separated list of ABSOLUTE roots that share `cwd`'s trust domain.
///
/// # Why an explicit knob is unavoidable (measured 2026-08-05)
///
/// There is NO channel by which the harness declares its additional working
/// directories to a hook. This was enumerated, not assumed:
///
/// * `harness_core::hook::HookInput` (`crates/harness-core/src/hook.rs`, `pub
///   struct HookInput`) carries no such field — it has `cwd`, and nothing else
///   that names a second root;
/// * no `CLAUDE_*` environment variable carries it (the hook's environment was
///   enumerated);
/// * `~/.claude/settings.json`'s `permissions` object has only `allow` and
///   `deny` keys — there is no `additionalDirectories` to read.
///
/// So additional working directories CANNOT be auto-discovered by this hook.
/// A later reader should not assume auto-discovery was merely overlooked: it
/// was looked for and it does not exist. If a channel is ever added, prefer it
/// over this knob and delete the knob.
///
/// Entries are ignored (never widen the domain) when they are empty, relative
/// (a relative root would resolve against the hook PROCESS's cwd, which is not
/// the payload's `cwd` and would drift between the `mark`/`gate`/`clear`
/// invocations — the same argument `state::STATE_DIR_ENV` makes), an
/// unexpanded `~`, or the filesystem root `/` (a `/` root dissolves the gate
/// entirely; sanitising a knob must not be able to disable the check —
/// CLAUDE.md §3).
pub const TRUSTED_ROOTS_ENV: &str = "TAINTGUARD_TRUSTED_ROOTS";

/// The repository's worktree registry, as observed from `cwd`.
///
/// `common` is the canonical git COMMON dir of `cwd`'s repository — the thing
/// that makes two working trees the same repository. `roots` are the working
/// trees git itself lists for it; each one must still prove it reports the same
/// common dir before it may grant trust (see [`TrustDomain::classify`]).
#[derive(Debug, Clone)]
struct Registry {
    common: PathBuf,
    roots: Vec<PathBuf>,
}

/// The set of roots that count as "inside the project" for one hook invocation.
#[derive(Debug, Clone)]
pub struct TrustDomain {
    /// The hook payload's `cwd`. Trusted by definition — it is the session's
    /// own project root.
    primary: PathBuf,
    /// Roots declared through [`TRUSTED_ROOTS_ENV`].
    configured: Vec<PathBuf>,
    /// `None` = the git probe could not answer, so NO worktree may be trusted
    /// (the strict side).
    registry: Option<Registry>,
}

impl TrustDomain {
    /// `cwd` alone — the pre-0.1.9 domain, and the fallback whenever the git
    /// probe cannot answer.
    pub fn strict(cwd: &Path) -> Self {
        Self {
            primary: cwd.to_path_buf(),
            configured: Vec::new(),
            registry: None,
        }
    }

    /// `cwd` plus explicitly configured roots, with no git registry.
    pub fn with_configured_roots(cwd: &Path, configured: Vec<PathBuf>) -> Self {
        Self {
            primary: cwd.to_path_buf(),
            configured,
            registry: None,
        }
    }

    /// The real domain for `cwd`: the git worktree registry plus the roots
    /// declared through [`TRUSTED_ROOTS_ENV`].
    ///
    /// The registry half is fallible, and a failure resolves to the STRICT
    /// side: `probe_registry` returns `None` for a `cwd` that is in no
    /// repository, for a git that could not be run at all, for a git that ran
    /// and exited non-zero, and for a common dir that could not be
    /// canonicalized — and `None` means no worktree can be trusted, not "trust
    /// them anyway". Widening the domain on an answer that was never obtained
    /// would be the same class of mistake as this whole redefinition is
    /// correcting, only in the dangerous direction (CLAUDE.md §3).
    ///
    /// The configured half is NOT derived from that probe — it is an operator's
    /// explicit declaration — so it survives a git failure. Dropping it too
    /// would re-create the deadlock (the scratchpad tainting again) whenever
    /// git hiccups, and would make an unrelated subprocess failure silently
    /// revoke a declaration the operator can see in their own environment.
    pub fn resolve(cwd: &Path) -> Self {
        Self {
            primary: cwd.to_path_buf(),
            configured: parse_configured_roots(std::env::var(TRUSTED_ROOTS_ENV).ok().as_deref()),
            registry: probe_registry(cwd),
        }
    }

    /// True when no worktree registry was resolved, i.e. the git-derived half
    /// of the domain is empty. Exposed so a test can assert that the strict
    /// fallback was actually taken rather than inferring it from a verdict.
    pub fn is_strict(&self) -> bool {
        self.registry.is_none()
    }

    /// The roots declared through [`TRUSTED_ROOTS_ENV`].
    pub fn configured_roots(&self) -> &[PathBuf] {
        &self.configured
    }

    /// Classify `target` against the whole domain.
    ///
    /// `Trusted` iff SOME root positively resolves `target` inside itself.
    /// Otherwise the answer is the PRIMARY root's own verdict, unchanged — so
    /// every distinction the single-root classifier drew survives verbatim: an
    /// empty target, an unexpanded `~`, a `..` escape, an unresolvable `cwd`
    /// all still answer exactly what they answered in 0.1.8. Widening may only
    /// turn a non-`Trusted` answer into `Trusted`; it can never turn an
    /// `Untrusted` into an `Indeterminate` or vice versa, and it can never
    /// weaken the fail-closed direction.
    ///
    /// A registry root must ALSO prove it still shares `cwd`'s git common dir
    /// before it may grant trust. Containment inside a listed path is not
    /// enough: a registered worktree directory can be deleted and replaced by
    /// an entirely different repository, and then the path shape is all that is
    /// left of the relationship — which is precisely what must not be allowed
    /// to decide. `shares_common_dir` is only consulted for a root that would
    /// otherwise grant trust, so this costs at most one extra `git` call, and
    /// zero for the overwhelmingly common in-`cwd` read.
    pub fn classify(&self, target: &str) -> Trust {
        let primary = classify(&self.primary, target);
        if primary == Trust::Trusted {
            return Trust::Trusted;
        }
        for root in &self.configured {
            if classify(root, target) == Trust::Trusted {
                return Trust::Trusted;
            }
        }
        if let Some(registry) = &self.registry {
            for root in &registry.roots {
                if classify(root, target) == Trust::Trusted
                    && shares_common_dir(root, &registry.common)
                {
                    return Trust::Trusted;
                }
            }
        }
        primary
    }
}

/// Parse [`TRUSTED_ROOTS_ENV`]'s value into roots, dropping every entry that
/// cannot be honoured (see the constant's docs for why each rule exists).
pub fn parse_configured_roots(raw: Option<&str>) -> Vec<PathBuf> {
    raw.unwrap_or_default()
        .split(':')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .filter(|entry| !entry.starts_with('~'))
        .filter(|entry| Path::new(entry).is_absolute())
        // `/` would make the whole filesystem the trust domain, i.e. it would
        // turn the gate off through a value rather than through a switch.
        // Clamping a knob must never be able to disable the check it feeds.
        .filter(|entry| Path::new(entry) != Path::new("/"))
        .map(PathBuf::from)
        .collect()
}

/// Run `git -C dir <args>`, returning its stdout only when git actually ran AND
/// exited zero AND said something.
///
/// The exit status is part of the decision, deliberately: a checker that
/// crashed is not a checker that passed, and `git` prints an empty stdout on
/// plenty of failures, so reading stdout alone would read a failure as an
/// answer. `GIT_DIR` and friends are removed because an inherited value would
/// silently redirect the probe at a repository that is not the one under
/// `dir` — the question here is about `dir`, not about whatever the ambient
/// environment last pointed at.
fn git_stdout(dir: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_INDEX_FILE")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let trimmed = text.trim().to_string();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

/// The canonical git common dir of the repository containing `dir`, or `None`
/// when that cannot be established.
///
/// `--git-common-dir` may print a path relative to `dir`; `--absolute-git-dir`
/// has no common-dir equivalent, so the relative form is resolved here (the
/// same join `condukt::maintree::observe_tree_role` performs for the same
/// reason).
fn common_dir(dir: &Path) -> Option<PathBuf> {
    let raw = PathBuf::from(git_stdout(dir, &["rev-parse", "--git-common-dir"])?);
    let absolute = if raw.is_absolute() {
        raw
    } else {
        dir.join(raw)
    };
    std::fs::canonicalize(absolute).ok()
}

/// The working trees git itself registers for `cwd`'s repository.
///
/// This is the common dir's own registry (`$GIT_COMMON_DIR/worktrees/*`), read
/// through git rather than by guessing at paths — a directory that merely looks
/// like a worktree, or sits next to one, appears nowhere in it.
fn probe_registry(cwd: &Path) -> Option<Registry> {
    let common = common_dir(cwd)?;
    let listing = git_stdout(cwd, &["worktree", "list", "--porcelain"])?;
    let roots: Vec<PathBuf> = listing
        .lines()
        .filter_map(|line| line.strip_prefix("worktree "))
        .map(|path| PathBuf::from(path.trim()))
        .filter(|path| !path.as_os_str().is_empty())
        .collect();
    if roots.is_empty() {
        return None;
    }
    Some(Registry { common, roots })
}

/// Does `candidate` still report `common` as its git common dir?
///
/// False on every failure — an unrunnable git, a non-zero exit, a path that
/// cannot be canonicalized. "I could not check" is not "it checks out".
fn shares_common_dir(candidate: &Path, common: &Path) -> bool {
    common_dir(candidate).is_some_and(|c| c == *common)
}

/// Classify `target` against the full trust domain of `cwd` — the entry point
/// the `mark` hook uses.
///
/// The `cwd` check runs first and short-circuits, so the common case (a read of
/// a file in the project the session is sitting in) spawns no subprocess at
/// all; the git probe only runs for a target `cwd` alone would have tainted,
/// which is exactly the set of reads whose verdict this redefinition is about.
pub fn classify_in_domain(cwd: &Path, target: &str) -> Trust {
    match classify(cwd, target) {
        Trust::Trusted => Trust::Trusted,
        _ => TrustDomain::resolve(cwd).classify(target),
    }
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

    // ── trust domain (0.1.9) ────────────────────────────────────────────────
    //
    // These fixtures are REAL repositories built with real `git init` /
    // `git worktree add` in temp dirs, deliberately not mocked: the whole
    // question is whether the common-dir resolution actually distinguishes
    // "another working tree of my repository" from "a different repository
    // that happens to sit next door", and a mocked probe would answer that
    // question by assumption instead of by observation.

    /// Run git in `dir`, asserting it succeeded. Hermetic (`GIT_CONFIG_GLOBAL`
    /// / `GIT_CONFIG_SYSTEM` neutralised, identity supplied) so the developer's
    /// own git config cannot change what the fixture is.
    fn git_ok(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_AUTHOR_NAME", "taintguard-test")
            .env("GIT_AUTHOR_EMAIL", "taintguard@example.invalid")
            .env("GIT_COMMITTER_NAME", "taintguard-test")
            .env("GIT_COMMITTER_EMAIL", "taintguard@example.invalid")
            .output()
            .expect("git runs (a missing git is a real failure, not a skip)");
        assert!(
            out.status.success(),
            "git {args:?} in {} failed: {}",
            dir.display(),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// A repository at `<parent>/<name>` with exactly one commit (a worktree
    /// cannot be added to a repository with no HEAD).
    fn init_repo(parent: &Path, name: &str) -> PathBuf {
        let repo = parent.join(name);
        std::fs::create_dir_all(&repo).unwrap();
        git_ok(&repo, &["init", "-q", "-b", "main"]);
        std::fs::write(repo.join("seed.txt"), "seed").unwrap();
        git_ok(&repo, &["add", "seed.txt"]);
        git_ok(&repo, &["commit", "-q", "-m", "seed", "--no-gpg-sign"]);
        repo
    }

    fn add_worktree(repo: &Path, at: &Path, branch: &str) -> PathBuf {
        std::fs::create_dir_all(at.parent().unwrap()).unwrap();
        git_ok(
            repo,
            &["worktree", "add", "-q", "-b", branch, &at.to_string_lossy()],
        );
        at.to_path_buf()
    }

    /// POSITIVE. CLAUDE.md §8 forces every edit into a linked worktree, so a
    /// `Read` of one is a read of this session's own project — the same repo,
    /// the same operator, content this session wrote. It must be `Trusted`,
    /// from the main tree and from the worktree alike.
    #[test]
    fn linked_worktree_of_the_same_repository_is_trusted() {
        let tmp = temp_root("linked-worktree");
        let repo = init_repo(tmp.path(), "repo");
        // Deliberately NOT under `repo`: real worktrees live in
        // `~/.condukt/worktrees/...` while the repo lives in `~/src/harness`,
        // so containment can never come from a parent-directory relation.
        let wt = add_worktree(&repo, &tmp.path().join("worktrees").join("wt"), "feature");

        assert_eq!(
            classify_in_domain(&repo, &wt.join("seed.txt").to_string_lossy()),
            Trust::Trusted,
            "a linked worktree of the same repository is the same trust domain"
        );
        assert_eq!(
            classify_in_domain(&wt, &repo.join("seed.txt").to_string_lossy()),
            Trust::Trusted,
            "and symmetrically: from inside the worktree, the main tree is too"
        );
    }

    /// NEGATIVE / ANTI-VACUITY CONTROL for the test above.
    ///
    /// Without this, "trust linked worktrees" collapses into "trust every
    /// sibling directory", which is a different and WORSE wrong gate. The
    /// fixture is shape-identical to the positive one — an unrelated repo whose
    /// worktree sits in the very same parent directory as ours — so the only
    /// thing that can separate them is the common dir.
    ///
    /// The final assertion pins that this test is not passing merely because
    /// the widening does not exist: the registry must have been resolved (the
    /// machinery was live) and still have said `Untrusted`.
    #[test]
    fn unrelated_repository_at_a_sibling_path_is_untrusted() {
        let tmp = temp_root("unrelated-repo");
        let worktrees = tmp.path().join("worktrees");
        let mine = init_repo(tmp.path(), "mine");
        let theirs = init_repo(tmp.path(), "theirs");
        let _my_wt = add_worktree(&mine, &worktrees.join("mine-wt"), "mine-feature");
        let their_wt = add_worktree(&theirs, &worktrees.join("theirs-wt"), "their-feature");

        assert_eq!(
            classify_in_domain(&mine, &theirs.join("seed.txt").to_string_lossy()),
            Trust::Untrusted,
            "another repository is another trust domain, however close it sits"
        );
        assert_eq!(
            classify_in_domain(&mine, &their_wt.join("seed.txt").to_string_lossy()),
            Trust::Untrusted,
            "and so is ITS worktree, even sharing a parent dir with ours"
        );
        assert!(
            !TrustDomain::resolve(&mine).is_strict(),
            "liveness pin: the registry must have resolved, so the two \
             assertions above are the widened code path saying no — not the \
             absence of any widening at all"
        );
    }

    /// A directory that merely OCCUPIES a registered worktree path cannot claim
    /// trust: the registry still lists the path, but git reports a different
    /// common dir for what is there now.
    #[test]
    fn a_registered_path_now_holding_a_foreign_repository_is_untrusted() {
        let tmp = temp_root("replaced-worktree");
        let mine = init_repo(tmp.path(), "mine");
        let wt = add_worktree(&mine, &tmp.path().join("worktrees").join("wt"), "feature");

        std::fs::remove_dir_all(&wt).unwrap();
        let foreign = init_repo(wt.parent().unwrap(), "wt");
        assert_eq!(foreign, wt);

        // The stale entry is still in the registry — otherwise this test would
        // be asserting nothing about the common-dir check. (git prints the
        // CANONICAL path — on macOS a temp dir's `/var/...` is a symlink to
        // `/private/var/...` — so compare canonicalized, not textually.)
        let listing = git_ok(&mine, &["worktree", "list", "--porcelain"]);
        let canonical_wt = std::fs::canonicalize(&wt).unwrap();
        assert!(
            listing
                .lines()
                .filter_map(|l| l.strip_prefix("worktree "))
                .any(|p| std::fs::canonicalize(p.trim()).ok() == Some(canonical_wt.clone())),
            "fixture precondition: the registry still lists the path; got {listing}"
        );

        assert_eq!(
            classify_in_domain(&mine, &wt.join("seed.txt").to_string_lossy()),
            Trust::Untrusted,
            "registered path, foreign repository — the path shape must not decide"
        );
    }

    /// The strict fallback: when the git probe cannot answer, the domain is cwd
    /// ALONE. A real linked worktree of a real repository stays `Untrusted`,
    /// because widening on an unverified answer is exactly what CLAUDE.md §3
    /// forbids.
    #[test]
    fn a_failed_git_probe_falls_back_to_cwd_alone() {
        let tmp = temp_root("probe-failure");
        let repo = init_repo(tmp.path(), "repo");
        let wt = add_worktree(&repo, &tmp.path().join("worktrees").join("wt"), "feature");

        // `orphan` is in no repository at all, so `git rev-parse
        // --git-common-dir` exits non-zero there.
        let orphan = tmp.path().join("orphan");
        std::fs::create_dir_all(&orphan).unwrap();

        let domain = TrustDomain::resolve(&orphan);
        assert!(
            domain.is_strict(),
            "a probe that cannot answer must produce no registry at all"
        );
        assert_eq!(
            domain.classify(&wt.join("seed.txt").to_string_lossy()),
            Trust::Untrusted
        );
        assert_eq!(
            classify_in_domain(&orphan, &wt.join("seed.txt").to_string_lossy()),
            Trust::Untrusted
        );
    }

    /// A `strict` domain is the pre-0.1.9 behaviour exactly.
    #[test]
    fn strict_domain_trusts_nothing_but_cwd() {
        let tmp = temp_root("strict");
        let repo = init_repo(tmp.path(), "repo");
        let wt = add_worktree(&repo, &tmp.path().join("worktrees").join("wt"), "feature");
        let domain = TrustDomain::strict(&repo);
        assert!(domain.is_strict());
        assert_eq!(
            domain.classify(&wt.join("seed.txt").to_string_lossy()),
            Trust::Untrusted
        );
        assert_eq!(
            domain.classify(&repo.join("seed.txt").to_string_lossy()),
            Trust::Trusted
        );
    }

    /// The declared-additional-directory case (the scratchpad): a root that no
    /// hook channel can discover, so the operator declares it.
    #[test]
    fn a_configured_root_extends_the_domain_and_only_that_root() {
        let tmp = temp_root("configured-root");
        let cwd = tmp.path().join("project");
        let scratch = tmp.path().join("scratchpad");
        let elsewhere = tmp.path().join("elsewhere");
        for d in [&cwd, &scratch, &elsewhere] {
            std::fs::create_dir_all(d).unwrap();
            std::fs::write(d.join("note.md"), "x").unwrap();
        }

        let domain = TrustDomain::with_configured_roots(&cwd, vec![scratch.clone()]);
        assert_eq!(
            domain.classify(&scratch.join("note.md").to_string_lossy()),
            Trust::Trusted
        );
        assert_eq!(
            domain.classify(&elsewhere.join("note.md").to_string_lossy()),
            Trust::Untrusted,
            "declaring one root must not declare its neighbours"
        );
        assert_eq!(
            domain.classify(&scratch.join("..").join("elsewhere").to_string_lossy()),
            Trust::Untrusted,
            "and `..` must not escape a configured root either"
        );
    }

    /// An unusable entry must never widen the domain, and must not take a
    /// usable neighbour down with it.
    #[test]
    fn configured_roots_parse_drops_entries_that_cannot_be_honoured() {
        assert!(parse_configured_roots(None).is_empty());
        assert!(parse_configured_roots(Some("")).is_empty());
        assert!(parse_configured_roots(Some("   ")).is_empty());
        assert!(
            parse_configured_roots(Some("relative/dir")).is_empty(),
            "a relative root resolves against the hook PROCESS's cwd"
        );
        assert!(
            parse_configured_roots(Some("~/scratch")).is_empty(),
            "an unexpanded tilde is not a resolved path"
        );
        assert!(
            parse_configured_roots(Some("/")).is_empty(),
            "a `/` root would dissolve the gate; sanitising must not disable it"
        );
        assert_eq!(
            parse_configured_roots(Some("/a/b:/c")),
            vec![PathBuf::from("/a/b"), PathBuf::from("/c")]
        );
        assert_eq!(
            parse_configured_roots(Some("/:rel:  :/good")),
            vec![PathBuf::from("/good")],
            "a rejected entry must not take a good one with it"
        );
    }

    /// `resolve` must actually READ the env var — a knob nothing reads is not a
    /// knob. (Guarded by the crate-wide env lock: `set_var` is process-global.)
    #[test]
    fn resolve_reads_the_configured_roots_env_var() {
        let _guard = crate::state::env_lock_for_test();
        let tmp = temp_root("configured-env");
        let cwd = tmp.path().join("project");
        let scratch = tmp.path().join("scratchpad");
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::create_dir_all(&scratch).unwrap();
        std::fs::write(scratch.join("note.md"), "x").unwrap();

        std::env::set_var(TRUSTED_ROOTS_ENV, &scratch);
        let domain = TrustDomain::resolve(&cwd);
        std::env::remove_var(TRUSTED_ROOTS_ENV);

        assert_eq!(domain.configured_roots(), std::slice::from_ref(&scratch));
        assert_eq!(
            domain.classify(&scratch.join("note.md").to_string_lossy()),
            Trust::Trusted
        );
    }
}
