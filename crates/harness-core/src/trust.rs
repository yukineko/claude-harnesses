//! Workspace trust: a shared gate for honoring *command strings* that come from
//! a project-local config file.
//!
//! Several plugins read a project-local config (`<root>/donegate.toml`,
//! `reviewgate.toml`, `beacon.toml`, `tdd.toml`, …) that "wins outright" over the
//! home config, and some of those configs carry shell command strings that the
//! plugin later runs (via `sh -c`) from a Stop / SessionStart hook. That means
//! merely opening (or cloning) a repository that ships such a file would run
//! attacker-controlled commands with the user's privileges.
//!
//! This module is the safe-by-default boundary: project-sourced commands are only
//! honored once the user has explicitly **trusted** that project root, mirroring
//! VS Code Workspace Trust / git's `safe.directory`.
//!
//! **What a plugin does with "untrusted" is the plugin's decision, and it is not
//! uniform.** Most of the consumers below fall back to the (trusted) home config
//! or built-in defaults. `donegate` deliberately does not: for a *gate*, "a check
//! set was declared and I am refusing to run it" is a refusal to judge, so it
//! blocks rather than rendering the refusal as "no checks" (CLAUDE.md 3). Do not
//! assume a single downstream meaning for the answer this module returns.
//!
//! Two resolvers, and they are not interchangeable:
//! * [`is_trusted`] — exact canonical-path membership. Unchanged.
//! * [`resolve`] — the same, plus **worktree inheritance** (a linked git
//!   worktree of a trusted repository is trusted). Returns the three-valued
//!   [`Trust`] so the caller can say *how* it was reached. Only `donegate`
//!   consumes this today; see its docs for the migration argument.
//!
//! The trust list lives in `~/.harness/trust.toml`:
//! ```toml
//! trusted = ["/abs/path/to/project", "/another/repo"]
//! ```
//! Paths are stored canonicalized (absolute) so a relative or `..`-laden cwd can
//! never spoof a trusted entry. The escape hatch `HARNESS_TRUST_ALL=1` trusts
//! every project (for CI / single-tenant machines that accept the risk).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::{base_dir, env_bool};

/// On-disk form of the trust list.
#[derive(Debug, Default, Serialize, Deserialize)]
struct TrustFile {
    #[serde(default)]
    trusted: Vec<String>,
}

/// Path to the shared trust list (`~/.harness/trust.toml`).
pub fn trust_path() -> PathBuf {
    base_dir("harness").join("trust.toml")
}

/// Env escape hatch: `HARNESS_TRUST_ALL` set truthy trusts every project.
pub fn trust_all() -> bool {
    env_bool("HARNESS_TRUST_ALL").unwrap_or(false)
}

/// Normalize a project root to a canonical absolute key. Falls back to the path
/// as given when it can't be canonicalized (e.g. it doesn't exist yet), so the
/// stored key and a later lookup of the same path still agree.
fn normalize(root: &Path) -> PathBuf {
    std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf())
}

/// Read the trust list for a **membership query** (is this root trusted?).
///
/// An unreadable or unparseable file yields an empty list here, and that is
/// only acceptable because every reader of it answers "not trusted" from an
/// empty list — the restricted side for a trust decision (a project config's
/// commands are then not honored). It must never feed a *write*: persisting
/// the empty default would wipe every existing grant. Writers use
/// [`read_file_strict`].
fn load_file() -> TrustFile {
    read_file_strict().unwrap_or_default()
}

/// Read the trust list for a **read-modify-write**. Absent file = empty list
/// (nothing trusted yet). Any other read error, or a file that does not parse,
/// is an `Err`: the caller must not write, because the only thing it could
/// write back is a list that has lost the entries it failed to read.
fn read_file_strict() -> std::io::Result<TrustFile> {
    let path = trust_path();
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(TrustFile::default()),
        Err(e) => {
            return Err(std::io::Error::new(
                e.kind(),
                format!("cannot read trust list {}: {e}", path.display()),
            ))
        }
    };
    toml::from_str::<TrustFile>(&text).map_err(|e| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "trust list {} does not parse; refusing to rewrite it (fix or remove it by hand): {e}",
                path.display()
            ),
        )
    })
}

/// Every trusted project root, canonicalized.
pub fn list() -> Vec<PathBuf> {
    load_file().trusted.into_iter().map(PathBuf::from).collect()
}

/// Is this project root actually recorded in `~/.harness/trust.toml`?
///
/// Membership ONLY: unlike [`is_trusted`] this deliberately does **not** consult
/// the `HARNESS_TRUST_ALL` escape hatch, so it answers "did a human durably
/// trust this root?" rather than "may commands from this root be honored right
/// now?".
///
/// Use this — never [`is_trusted`] — when the answer decides whether to **write**
/// a durable trust entry. `HARNESS_TRUST_ALL` is a *transient*, per-process
/// hatch; gating a write on it lets a single run with the variable set mint a
/// **permanent** grant that outlives the variable (observed in
/// `condukt`'s worktree-trust propagation, backlog 5151605e F1: a worktree cut
/// from an untrusted repo stayed trusted after the variable was gone).
/// Propagating trust is not the same operation as granting it.
///
/// Reading (i.e. deciding whether to honor a project config in this process) is
/// the case the hatch exists for and should keep using [`is_trusted`].
pub fn is_listed(root: &Path) -> bool {
    let key = normalize(root);
    load_file().trusted.iter().any(|t| Path::new(t) == key)
}

/// Is this project root trusted to run commands sourced from its project-local
/// config? `HARNESS_TRUST_ALL` short-circuits to `true`; otherwise the root must
/// be present (canonicalized) in `~/.harness/trust.toml`. Default: `false`.
///
/// This is the predicate for *honoring* a project config in the current process.
/// For anything that persists a grant, use [`is_listed`].
pub fn is_trusted(root: &Path) -> bool {
    if trust_all() {
        return true;
    }
    is_listed(root)
}

/// How a project root's trust was resolved, kept as three answers rather than a
/// bool so "inherited from the repository this is a worktree of" is *visible* to
/// the caller (and to the operator it reports to) instead of being
/// indistinguishable from an explicit entry.
///
/// [`Trust::Untrusted`] is the restricted side: every failure to resolve
/// (unreadable `.git`, un-canonicalizable path, a gitfile whose shape we do not
/// recognize) lands here rather than on a trusted answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Trust {
    /// The root is listed in `~/.harness/trust.toml`, or `HARNESS_TRUST_ALL` is
    /// set truthy.
    Direct,
    /// The root is a **linked git worktree** whose main working tree is trusted,
    /// so it inherits that trust. Carries the main working tree that granted it,
    /// so a caller can name it when explaining the decision.
    InheritedFromMainWorktree(PathBuf),
    /// Not trusted.
    Untrusted,
}

impl Trust {
    /// True for both trusted answers. Named (rather than `From<Trust> for bool`)
    /// so every collapse to a bool is explicit at the call site.
    #[must_use]
    pub fn is_trusted(&self) -> bool {
        !matches!(self, Trust::Untrusted)
    }
}

/// Resolve trust for a project root, honoring **worktree inheritance**: a linked
/// git worktree of a repository whose main working tree is trusted is itself
/// trusted.
///
/// # Why inheritance is not a loosening
///
/// Trusting repository `R` means "I accept running the command strings that
/// `R`'s working tree contains". A linked worktree of `R` shares `R`'s object
/// database and is a checkout of `R`'s history: anything that can appear in the
/// worktree's `donegate.toml` could be checked out in `R`'s own working tree by
/// `git checkout`, with identical privileges. So the blast radius of inheriting
/// is exactly the blast radius already accepted when `R` was trusted — refusing
/// to inherit does not shrink it, it only makes the trust answer wrong for a
/// directory whose contents are `R`'s.
///
/// The measured need: this repo's own CLAUDE.md 8 mandates that *all* work
/// happen in session worktrees, while [`is_trusted`] matches a canonical path
/// exactly. Every session worktree was therefore untrusted, which — once a
/// gate stops rendering "untrusted" as "nothing to check" — would block every
/// session at once.
///
/// # What it does not do
///
/// * It does not walk parent directories. Only the single `worktree ->
///   main working tree` hop is followed, and only when the `.git` *gitfile*
///   proves the relationship.
/// * It does not trust the `.git` gitfile's claim. The gitfile is writable by
///   whoever controls `root`, so the relationship is accepted only when the
///   main working tree's own `.git` is the named common git dir, the named
///   `worktrees/<name>` admin dir exists as a real directory, AND git's
///   back-pointer `worktrees/<name>/gitdir` resolves to `root/.git` (see
///   `main_worktree_of`). Forging those requires write access inside the
///   trusted repository's `.git`.
/// * It does not consult `git` as a subprocess: the answer is read from the
///   files above, so there is no exit status to ignore and no `PATH` to
///   depend on.
/// * Every unreadable / unrecognized / un-canonicalizable step returns
///   [`Trust::Untrusted`] — the restricted side.
#[must_use]
pub fn resolve(root: &Path) -> Trust {
    if is_trusted(root) {
        return Trust::Direct;
    }
    match main_worktree_of(root) {
        Some(main) if is_trusted(&main) => Trust::InheritedFromMainWorktree(main),
        _ => Trust::Untrusted,
    }
}

/// If `root` is a **linked** git worktree, the main working tree it belongs to.
///
/// A linked worktree's `.git` is a *file* (never a directory) whose first line
/// is `gitdir: <path to>/<common-git-dir>/worktrees/<name>` (gitrepository-layout(5)).
/// The main working tree is the parent of that common git dir. The gitfile is
/// written by whoever controls `root`, so nothing it says is taken on faith;
/// `Some` is returned only when ALL of these are verified:
///
/// 1. the gitdir path has the shape `<common>/worktrees/<name>`;
/// 2. the candidate main working tree's own `.git` canonicalizes to `<common>`;
/// 3. `<common>/worktrees/<name>` exists as a real directory (not a symlink);
/// 4. git's back-pointer `<common>/worktrees/<name>/gitdir` (absolute, or
///    relative to that admin dir) canonicalizes to the same path as
///    `canonicalize(root/.git)`.
///
/// 3 and 4 live inside the trusted repository's `.git`, so a forged gitfile in
/// an arbitrary directory (naming a nonexistent `worktrees/<name>`, or an
/// existing one that belongs to a different worktree) cannot satisfy them
/// without write access to that `.git`. A bare repository (whose worktrees have
/// no main working tree to inherit from), a `GIT_DIR` in an unusual location,
/// or any read / parse / canonicalize failure yields `None` — the restricted
/// side, since the only caller uses `Some` to *grant* trust.
fn main_worktree_of(root: &Path) -> Option<PathBuf> {
    let dot_git = root.join(".git");
    // A main working tree has a `.git` DIRECTORY and inherits from nobody.
    if !std::fs::symlink_metadata(&dot_git).ok()?.is_file() {
        return None;
    }
    let text = std::fs::read_to_string(&dot_git).ok()?;
    let target = text
        .lines()
        .find_map(|l| l.trim().strip_prefix("gitdir:"))?
        .trim();
    if target.is_empty() {
        return None;
    }
    let target = Path::new(target);
    let git_dir = if target.is_absolute() {
        target.to_path_buf()
    } else {
        root.join(target)
    };

    // .../<common-git-dir>/worktrees/<name>
    let worktrees = git_dir.parent()?;
    if worktrees.file_name()? != "worktrees" {
        return None;
    }
    let common = worktrees.parent()?;
    let candidate = common.parent()?;

    // Verify the inference instead of trusting the path shape: the candidate's
    // own `.git` must BE this common git dir.
    let common_key = std::fs::canonicalize(common).ok()?;
    let candidate_key = std::fs::canonicalize(candidate.join(".git")).ok()?;
    if common_key != candidate_key {
        return None;
    }

    // The gitfile is attacker-writable: anyone can drop a `.git` FILE naming
    // `<trusted-main>/.git/worktrees/<anything>` into any directory, and every
    // check above would pass. What only git (or someone who can already write
    // inside the trusted repo's `.git`) can produce is the per-worktree admin
    // dir and its BACK-POINTER, so both are required:
    //
    // 1. `git_dir` (`.../worktrees/<name>`) must exist as a real directory
    //    (not a symlink, not a file).
    if !std::fs::symlink_metadata(&git_dir).ok()?.is_dir() {
        return None;
    }
    // 2. `<git_dir>/gitdir` (gitrepository-layout(5): "the path to the .git
    //    file" of the linked worktree) must canonicalize to THIS root's `.git`.
    //    git writes it absolute, or relative to `git_dir` under
    //    `worktree.useRelativePaths`.
    let back = std::fs::read_to_string(git_dir.join("gitdir")).ok()?;
    let back = back.lines().next()?.trim();
    if back.is_empty() {
        return None;
    }
    let back = Path::new(back);
    let back = if back.is_absolute() {
        back.to_path_buf()
    } else {
        git_dir.join(back)
    };
    let back_key = std::fs::canonicalize(back).ok()?;
    let own_key = std::fs::canonicalize(&dot_git).ok()?;
    if back_key != own_key {
        return None;
    }
    Some(candidate.to_path_buf())
}

/// Add a project root to the trust list (idempotent). Returns the canonical key
/// that was recorded.
///
/// Fails closed: an existing trust file that cannot be read or parsed is an
/// `Err` and the file is left untouched (rewriting it would drop every entry
/// that could not be read). The read-modify-write is serialized across
/// processes by an exclusive lock on a sibling lock file; failing to take that
/// lock is an `Err`, never an unserialized write (which would lose a
/// concurrent writer's entry). The new list is written atomically (tmp +
/// fsync + rename), so a concurrent reader never sees a truncated file.
pub fn add(root: &Path) -> std::io::Result<PathBuf> {
    let key = normalize(root);
    let key_str = key.to_string_lossy().into_owned();

    let _lock = TrustLock::acquire()?;
    let mut file = read_file_strict()?;
    if !file.trusted.iter().any(|t| t == &key_str) {
        file.trusted.push(key_str);
        write_file(&file)?;
    }
    Ok(key)
}

/// Remove a project root from the trust list (idempotent). Returns `true` if an
/// entry was actually removed. Same fail-closed read, lock and atomic write as
/// [`add`]: an unreadable / unparseable file is an `Err`, not "nothing to
/// remove".
pub fn remove(root: &Path) -> std::io::Result<bool> {
    let key = normalize(root);
    let _lock = TrustLock::acquire()?;
    let mut file = read_file_strict()?;
    let before = file.trusted.len();
    file.trusted.retain(|t| Path::new(t) != key);
    let removed = file.trusted.len() != before;
    if removed {
        write_file(&file)?;
    }
    Ok(removed)
}

/// Upper bound on waiting for the trust-list lock. Holders only do one small
/// read + write, so hitting this means a stuck holder; the caller gets an
/// `Err` rather than a write that races it.
const LOCK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
const LOCK_BACKOFF: std::time::Duration = std::time::Duration::from_millis(5);

/// Exclusive advisory lock (`flock(2)`) on `trust.toml.lock`, held for the
/// whole read-modify-write. `flock` belongs to the open file description, so
/// it serializes threads of one process as well as separate processes, and it
/// is released by the kernel when the holder exits — no stale-lock stealing.
struct TrustLock {
    _file: std::fs::File,
}

impl TrustLock {
    #[cfg(unix)]
    fn acquire() -> std::io::Result<Self> {
        use std::os::unix::io::AsRawFd;
        let path = trust_path().with_extension("toml.lock");
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)?;
        let start = std::time::Instant::now();
        loop {
            // SAFETY: `file` owns a valid open descriptor for this call.
            let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
            if rc == 0 {
                return Ok(TrustLock { _file: file });
            }
            let err = std::io::Error::last_os_error();
            let contended = err.kind() == std::io::ErrorKind::WouldBlock
                || err.kind() == std::io::ErrorKind::Interrupted;
            if !contended {
                return Err(std::io::Error::new(
                    err.kind(),
                    format!("cannot lock trust list {}: {err}", path.display()),
                ));
            }
            if start.elapsed() >= LOCK_TIMEOUT {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    format!(
                        "timed out after {LOCK_TIMEOUT:?} waiting for trust list lock {}",
                        path.display()
                    ),
                ));
            }
            std::thread::sleep(LOCK_BACKOFF);
        }
    }

    /// No cross-process lock is implemented off unix, so a write there cannot
    /// be serialized; refuse it rather than risk a lost update.
    #[cfg(not(unix))]
    fn acquire() -> std::io::Result<Self> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "trust list writes need a cross-process lock, which is only implemented on unix",
        ))
    }
}

/// Atomic replace: write a process-unique tmp file, fsync it, rename it over
/// the trust list, then fsync the directory so the rename itself is durable.
/// Callers hold [`TrustLock`].
fn write_file(file: &TrustFile) -> std::io::Result<()> {
    use std::io::Write;
    let path = trust_path();
    let parent = path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    std::fs::create_dir_all(&parent)?;
    let body = toml::to_string(file).map_err(std::io::Error::other)?;
    let tmp = path.with_extension(format!("toml.tmp.{}", std::process::id()));
    let result = (|| {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(body.as_bytes())?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, &path)?;
        std::fs::File::open(&parent)?.sync_all()
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    // Tests mutate the process-global HOME and HARNESS_TRUST_ALL, so they must
    // not run concurrently -- with each other OR with any other module's env
    // tests. Keeping the whole sequence in one #[test] only bought the first
    // half of that, and this comment used to claim the rest; config::tests
    // moves HOME too, and did it under a different lock. The guard below is
    // what actually holds. See crate::test_env for the measured failure.
    #[test]
    fn trust_roundtrip_and_env_override() {
        let _guard = crate::test_env::lock();
        let home = tempfile::tempdir().unwrap();
        let proj = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", home.path());
        std::env::remove_var("HARNESS_TRUST_ALL");

        let root = proj.path();

        // Default: an unregistered project is untrusted.
        assert!(!is_trusted(root), "fresh project must be untrusted");
        assert!(list().is_empty());

        // After add(): trusted, and listed (canonicalized).
        let key = add(root).unwrap();
        assert!(is_trusted(root), "added project must be trusted");
        assert_eq!(list(), vec![key.clone()]);

        // add() is idempotent — no duplicate entries.
        add(root).unwrap();
        assert_eq!(list().len(), 1, "add must be idempotent");

        // The trust file actually exists on disk where we expect it.
        assert!(trust_path().exists());

        // remove() reverses it.
        assert!(remove(root).unwrap());
        assert!(!is_trusted(root), "removed project must be untrusted");
        assert!(!remove(root).unwrap(), "remove is idempotent");

        // HARNESS_TRUST_ALL trusts everything regardless of the list.
        std::env::set_var("HARNESS_TRUST_ALL", "1");
        assert!(is_trusted(root), "HARNESS_TRUST_ALL must override");
        std::env::set_var("HARNESS_TRUST_ALL", "0");
        assert!(!is_trusted(root), "HARNESS_TRUST_ALL=0 must not trust");
        std::env::remove_var("HARNESS_TRUST_ALL");
    }

    /// `is_listed` is the *membership* predicate: the transient env hatch must
    /// not be able to answer it. Callers that decide whether to WRITE a durable
    /// trust entry gate on this, so a `true` here from `HARNESS_TRUST_ALL`
    /// would turn a per-process hatch into a permanent grant (backlog 5151605e
    /// F1). Serialized by the same crate-wide env guard as the test above.
    #[test]
    fn is_listed_ignores_the_env_escape_hatch() {
        let _guard = crate::test_env::lock();
        let home = tempfile::tempdir().unwrap();
        let proj = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", home.path());
        std::env::remove_var("HARNESS_TRUST_ALL");

        let root = proj.path();
        assert!(!is_listed(root), "fresh project must not be listed");

        // The hatch flips `is_trusted` but must NOT flip membership.
        std::env::set_var("HARNESS_TRUST_ALL", "1");
        assert!(is_trusted(root), "apparatus: the hatch must be in effect");
        assert!(
            !is_listed(root),
            "HARNESS_TRUST_ALL made an unlisted root look listed: a transient hatch would then \
             authorize writing a permanent grant"
        );

        // And with the hatch still on, a genuinely listed root IS listed
        // (anti-vacuity: `is_listed` is not just a constant `false`).
        add(root).unwrap();
        assert!(is_listed(root), "an added root must be listed");
        std::env::remove_var("HARNESS_TRUST_ALL");
        assert!(is_listed(root), "…with or without the hatch");
        remove(root).unwrap();
        assert!(!is_listed(root), "a removed root must not be listed");
    }

    fn git(cwd: &Path, args: &[&str]) -> bool {
        std::process::Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn git_available() -> bool {
        std::process::Command::new("git")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// Worktree inheritance, both directions, in one `#[test]` -- serialized
    /// against every other env-mutating test by `crate::test_env::lock()`, not
    /// by being a single function (it mutates the process-global `HOME`, like
    /// the roundtrip test above).
    #[test]
    fn resolve_inherits_trust_from_the_main_worktree_but_only_when_it_is_trusted() {
        let _guard = crate::test_env::lock();
        if !git_available() {
            eprintln!("SKIPPED resolve_inherits_...: git unavailable");
            return;
        }
        let home = tempfile::tempdir().unwrap();
        let base = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", home.path());
        std::env::remove_var("HARNESS_TRUST_ALL");

        let main = base.path().join("main");
        std::fs::create_dir_all(&main).unwrap();
        for args in [
            &["init", "-q", "-b", "main"][..],
            &["config", "user.email", "t@t.com"][..],
            &["config", "user.name", "t"][..],
        ] {
            assert!(git(&main, args), "git {args:?}");
        }
        std::fs::write(main.join("f.txt"), "x\n").unwrap();
        assert!(git(&main, &["add", "-A"]));
        assert!(git(&main, &["commit", "-qm", "seed"]));

        let wt = base.path().join("wt");
        assert!(git(
            &main,
            &["worktree", "add", "-q", wt.to_str().unwrap(), "HEAD"]
        ));
        // Apparatus: this must really be a LINKED worktree, or the test proves
        // nothing about inheritance.
        assert!(wt.join(".git").is_file(), "apparatus: linked worktree");
        assert!(main.join(".git").is_dir(), "apparatus: main working tree");

        // 1. Nothing trusted ⇒ neither is trusted. Inheritance must not invent
        //    trust out of "is a worktree".
        assert_eq!(resolve(&main), Trust::Untrusted);
        assert_eq!(
            resolve(&wt),
            Trust::Untrusted,
            "a worktree of an UNTRUSTED repo must stay untrusted"
        );

        // 2. Trust the main working tree ⇒ the worktree inherits, and says so.
        let key = add(&main).unwrap();
        assert_eq!(resolve(&main), Trust::Direct);
        match resolve(&wt) {
            Trust::InheritedFromMainWorktree(from) => assert_eq!(
                std::fs::canonicalize(&from).unwrap(),
                std::fs::canonicalize(&key).unwrap(),
                "inheritance must name the main working tree that granted it"
            ),
            other => panic!("worktree must inherit the main tree's trust; got {other:?}"),
        }
        // The legacy exact-match resolver is deliberately unchanged, so the five
        // other consumers keep their current behaviour until each migrates.
        assert!(
            !is_trusted(&wt),
            "is_trusted must remain exact-match — migrating the other crates is a separate change"
        );

        // 3. Untrusting the main tree revokes the inherited trust too.
        assert!(remove(&main).unwrap());
        assert_eq!(resolve(&wt), Trust::Untrusted);

        // 4. Trusting the WORKTREE directly still works and reports Direct.
        add(&wt).unwrap();
        assert_eq!(resolve(&wt), Trust::Direct);
        assert!(remove(&wt).unwrap());

        // 5. A plain directory that is not a worktree at all inherits nothing.
        let plain = base.path().join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        assert_eq!(resolve(&plain), Trust::Untrusted);
        assert_eq!(main_worktree_of(&plain), None);

        // 6. A `.git` FILE whose gitdir does not have the `…/worktrees/<name>`
        //    shape must not resolve to a parent (no path-shape guessing).
        let bogus = base.path().join("bogus");
        std::fs::create_dir_all(&bogus).unwrap();
        std::fs::write(bogus.join(".git"), format!("gitdir: {}\n", main.display())).unwrap();
        assert_eq!(main_worktree_of(&bogus), None);
        add(&main).unwrap();
        assert_eq!(
            resolve(&bogus),
            Trust::Untrusted,
            "a gitfile pointing at a trusted repo's TREE (not its worktrees dir) must not inherit"
        );
        assert!(remove(&main).unwrap());
    }
}
