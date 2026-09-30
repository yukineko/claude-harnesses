// テスト内の unwrap/expect は意図的な assert であって fail-open ではないので許可する。
// production 側は workspace の [workspace.lints.clippy] で deny のまま。
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]
//! WHERE a destructive operand lands — the axis [`crate::detect`]'s delete
//! rules did not have.
//!
//! # The defect this module closes
//!
//! Until 0.2.51 every destructive-shape rule in this crate judged the SHAPE of
//! the command and never the LOCATION of its target. Measured on 0.2.50 (this
//! worktree, real PreToolUse payloads piped into the hook binary):
//!
//! ```text
//! rm -rf target   -> deny: recursive rm (-r) can delete an entire directory tree
//! rm -rf /tmp/foo -> deny: recursive rm (-r) can delete an entire directory tree
//! rm -rf /usr/lib -> deny: recursive rm (-r) can delete an entire directory tree
//! rm -rf /        -> deny: recursive rm (-r) can delete an entire directory tree
//! ```
//!
//! Four commands whose blast radii differ by many orders of magnitude, one
//! verdict, one reason string. That is not a strict gate, it is an
//! uninformative one, and an uninformative gate teaches evasion: the operator
//! cannot clear the `rm -rf target` they meant, so they reach for a longer way
//! round (a python `shutil.rmtree`, a `Write`-then-`bash script.sh`, a
//! `--dangerously-skip-permissions` session). Every one of those routes is
//! LESS analysed than the `rm` that was denied, so the gate's own false
//! positives degrade the protection it exists to provide.
//!
//! # What replaced it, and why it is not the carve-out that was removed
//!
//! `detect.rs` used to carry an `is_temp_scratch` predicate that allowed any
//! redirect target under `/tmp` etc. It was removed as a universal bypass: it
//! was a raw `starts_with` prefix test over a path whose `..` was not resolved,
//! so `/tmp/../etc/hosts` passed it and thereby disabled the whole rule. The
//! comment block that records its removal (`detect.rs`, "D1") lists exactly
//! what a location model must handle before it may be trusted: `..`, `~`,
//! `$TMPDIR`, and symlinks. This module answers all four, and the answers are
//! the reason it is a different proposition from the predicate that was
//! deleted:
//!
//!   * `..` — collapsed LEXICALLY here ([`normalize_abs`]), BEFORE the
//!     symlink resolver runs, and any residue (a relative path that climbs
//!     above its own base) is [`Determination::Undetermined`], never a
//!     placement. Lexical collapse is NOT what the kernel does: it resolves
//!     `a/lnk/..` to the parent of `lnk`'s TARGET. So a `..` that follows a
//!     symlinked component is placed at a path the kernel will not touch
//!     (`<root>/lnk/../x` is classified as `<root>/x`). This module does not
//!     close that; its callers must not grant anything on the strength of a
//!     `..`-bearing spelling (the worktree `Allow` refuses any `..` — see
//!     below; the safe-root `Ask` does NOT, see "Known gap" at the end);
//!   * `~`, `$VAR`, backticks, `$(…)`, globs, braces, quotes, `{}` (a
//!     `find -exec` placeholder) — the operand is not a literal path at all, so
//!     it is `Undetermined`. This module NEVER expands a shell construct and
//!     never guesses what one expands to;
//!   * `$TMPDIR` — read from the HOOK's own environment by the binary and
//!     passed in as a literal root ([`SafeRoots::new`]), which is a completely
//!     different act from expanding the string `$TMPDIR` found in a command;
//!   * symlinks — resolved by an INJECTED resolver ([`RealPathResolver`]) so
//!     that the hook binary can canonicalise against the real filesystem while
//!     [`crate::detect`] stays pure. With no resolver, or with a resolver that
//!     fails, the answer is `Undetermined` — not `Inside`.
//!
//! And the outcome differs too. That carve-out produced `Allow`; the placement
//! against the SAFE roots ([`SafeRoots::classify`]) produces at most a
//! `Decision::Ask`, which [`crate::interactive`] hardens back to a `Deny`
//! everywhere no human can answer (headless, condukt workers, cron). For those
//! roots it buys an interactive operator a one-keypress confirmation for a
//! bounded blast radius, and buys an unattended agent nothing at all. The one
//! exception is the next section, and it is an exception by user ruling, not
//! by inference.
//!
//! # Worktree storage roots — the one placement that yields `Allow`
//!
//! User ruling 2026-09-30 (backlog 873651b9): "worktreeへの削除を制限しているhookを
//! 排除して" — a recursive rm strictly inside the worktree storage roots is
//! ALLOWED with no confirmation, headless included. Trigger: a subagent could
//! not `rm -rf ~/.condukt/worktrees/pgv-target` (flat Deny: that directory was
//! not a safe root at all).
//!
//! The worktree storage roots are a SEPARATE set from the safe roots, built by
//! [`SafeRoots::new`] from the same inputs and consulted only through
//! [`SafeRoots::classify_worktree`], which only `detect`'s recursive-`rm` arm
//! calls (no other verb gains anything from them):
//!
//!   * `$HOME/.condukt/worktrees` — only when `HOME` is absolute;
//!   * for each of the payload `cwd` and `CLAUDE_PROJECT_DIR`: the prefix up to
//!     and including its last `.harness-worktrees` component when it has one,
//!     else `dirname(P)/.harness-worktrees`.
//!
//! A candidate becomes a root ONLY if, at the moment the model is built, it
//! EXISTS as a REAL directory that nothing on the way down to it redirects
//! (backlog 873651b9, second pass — the first pass accepted a root by its
//! resolved path alone, and an independent verifier escaped it with
//! `ln -s ~/Documents <parent>/.harness-worktrees` followed by
//! `rm -rf <parent>/.harness-worktrees/precious`):
//!
//!   * the ANCHOR is canonicalised with the injected resolver: `$HOME` for
//!     `$HOME/.condukt/worktrees`, `dirname(root)` for `.harness-worktrees`.
//!     Symlinks ABOVE the anchor (`/tmp` -> `/private/tmp`) are resolved and
//!     accepted — the anchor is an input of the session, not of the command;
//!   * every component BELOW the anchor (`.condukt` and `worktrees`; or
//!     `.harness-worktrees`) is `lstat`ed and must be a directory that is NOT a
//!     symlink. A symlinked `.condukt`, a symlinked `worktrees`, a symlinked
//!     `.harness-worktrees`, a regular file, or a component that does not exist
//!     means NO root — a root that does not exist is never resolved via its
//!     parent any more;
//!   * any IO error while checking (permission, not-found, a non-UTF-8 path)
//!     is "not a root";
//!   * the result must still pass the derived-root rules ([`NEVER_A_ROOT`], not
//!     `$HOME`, [`MIN_DERIVED_ROOT_COMPONENTS`]) and must be its own real path
//!     according to the resolver.
//!
//! The `lstat` is real filesystem I/O done here, in [`SafeRoots::new`], and only
//! when a resolver was supplied (the resolver is the signal that the caller is
//! the filesystem-aware hook binary). With no resolver there are no worktree
//! roots at all; [`SafeRoots::none`] has none of them either.
//!
//! # What actually stops each escape (be precise — several checks overlap)
//!
//!   * A root that is ALREADY a symlink (or has a symlinked `.condukt` /
//!     `worktrees`) when the hook runs: rejected by TWO independent checks in
//!     [`worktree_storage_roots`] — the `lstat` probe ([`is_real_dir`]) and the
//!     final "the root must be its own real path per the resolver" check. The
//!     resolver check alone already rejects every such case, and an operand
//!     reached through the symlink canonicalises to its target, which is not
//!     inside the (rejected or real) root either. So the `lstat` probe is
//!     DEFENCE IN DEPTH: the test suite does not isolate it (a mutation that
//!     makes it follow symlinks survives the tests), and it must not be cited
//!     as the thing that closes these cases.
//!   * A root that does not exist yet: rejected by the probe. No root is
//!     derived via its parent.
//!   * The SAME command swapping the root before its `rm` runs
//!     (`ln -s X <root> && rm -rf <root>/p`,
//!     `mv <root> away && ln -s X <root> && rm -rf <root>/x`): NOT stopped here
//!     — every check in this module is made at JUDGE time. It is stopped by the
//!     whole-command rule in `detect` (`worktree_rm_eligible`): the worktree
//!     `Allow` is granted only when every segment of the top-level command is a
//!     bare `rm` or `cd` built from plain word characters.
//!   * A relative `rm` operand after a `cd` that may fail or not apply
//!     (`cd <root>/s & rm -rf src`, `cd <root>/missing; rm -rf src`,
//!     `cd <root>/missing || rm -rf src`, `cd <root>/s | rm -rf src`): the cwd
//!     walk in `detect` assumes every `cd` succeeds and applies, so it would
//!     judge `<root>/s/src` while `rm` deletes `<cwd>/src`. Stopped by the cd
//!     rule of `worktree_rm_eligible`: if the command contains ANY `cd`, every
//!     `rm` operand must be absolute. With no `cd`, a relative operand is
//!     judged against the payload cwd, which is the runtime cwd.
//!   * A `..` after a symlinked component (`<root>/w/lnk/../v2`, relative
//!     `lnk/../x`, nested `sub/lnk2/../x`, trailing `<root>/w/x/..` with `x` a
//!     symlink out): NOT stopped here — this module collapses `..` lexically
//!     before resolving symlinks and would place all four Inside. Stopped by
//!     the `..` rule of `worktree_rm_eligible`: if any word of any `rm`
//!     segment has a `..` component, the worktree `Allow` is not available at
//!     all (even for a `..` that would stay inside). `detect::worktree_confined`
//!     also refuses a base (payload cwd) with a `..` component; Claude Code
//!     sends a canonical cwd, so that check is defence in depth.
//!
//! NOT closed by this module or by `detect`:
//!
//!   * cross-call TOCTOU — a DIFFERENT tool call (an earlier background job, a
//!     concurrent session) that creates, moves or links something between this
//!     judgement and the `rm`;
//!   * things an EARLIER command already MOVED into a real root
//!     (`mv ~/Documents <root>/x`, then `rm -rf <root>/x` is Allow — `x` is a
//!     real directory strictly inside the root). A SYMLINK placed there instead
//!     is not Allow: the operand canonicalises Outside.
//!
//! The resolution rules for OPERANDS are identical to the safe roots': only
//! `Inside` (a strict descendant, after LEXICAL `..` collapse and then symlink
//! resolution) counts; the storage root itself (`IsRoot`), `Outside`, and every
//! `Undetermined` do not, and the rm arm then keeps the verdict it had before
//! this rule. Because that `..` handling is lexical, the worktree `Allow` never
//! reaches this classification for a `..`-bearing operand (the `..` rule
//! above).
//! Protected-path precedence runs first in `detect`, so a protected path under
//! a worktree is still denied.
//!
//! # The fail-closed direction
//!
//! There are three answers, per CLAUDE.md §3, and only ONE of them may relax a
//! verdict: [`Placement::Inside`], which means "resolved, and provably a strict
//! descendant of a root this session is allowed to destroy things in" (a safe
//! root, or — for recursive rm only — a worktree storage root).
//! [`Placement::Outside`] and every `Undetermined` leave the caller's existing
//! Deny exactly as it was. [`Placement::IsRoot`] is a fourth statement that
//! deliberately does NOT collapse into `Inside`: `rm -rf <the project itself>`
//! takes `.git` with it, so the root's own directory is not inside the region
//! this module vouches for.
//!
//! # Known gap — lexical `..` on the safe-root `Ask`
//!
//! [`SafeRoots::classify`] (the safe roots, used by `detect`'s confined `Ask`
//! for `rm`, `truncate`, `find`, `git clean`, redirects, …) has the same
//! lexical-`..`-before-symlink behaviour and NO `..` refusal in front of it:
//! `rm -rf <project>/lnk/../x` with `lnk` a symlink out is placed Inside the
//! project and gets an interactive `Ask` (hardened to `Deny` headless) whose
//! text names the project, while the kernel deletes `<lnk target's
//! parent>/x`. The cwd walk in `detect` also joins `cd` targets lexically, so
//! `cd lnk/..` shifts the judged base the same way. The outcome is at most an
//! `Ask` a human must answer, never an `Allow`, but the question it asks is
//! wrong. Not fixed here.

use harness_core::verdict::Determination;

/// A function that maps a lexically-normalised absolute path to its REAL path,
/// resolving symlinked components against the filesystem.
///
/// Injected rather than called directly so [`crate::detect`] keeps the property
/// its doc comment claims ("Detection is pure (no I/O)") and so the unit tests
/// below can model a symlink without creating one. The hook binary passes its
/// own `std::fs::canonicalize`-based implementation; library consumers that must
/// stay pure pass `None` and get `Undetermined` for every operand, i.e. today's
/// behaviour.
///
/// `None` as a RETURN value means "could not be resolved" and resolves
/// restrictively at the call site — it is not "the path is fine as given".
pub type RealPathResolver = fn(&str) -> Option<String>;

/// Where a single, fully-resolved operand sits relative to the safe roots.
///
/// Deliberately four-valued together with the `Undetermined` of the
/// [`Determination`] that wraps it, and deliberately NOT a bool: the two
/// interesting negatives ("somewhere else entirely" and "the root itself") want
/// different verdicts from some callers, and "I could not tell" must not be
/// spellable as either.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Placement {
    /// A strict descendant of `root`. This is the ONLY variant that may relax a
    /// caller's verdict.
    Inside {
        /// The safe root it is under, as normalised by [`SafeRoots::new`].
        root: String,
        /// The resolved absolute path that was classified.
        path: String,
    },
    /// The operand IS a safe root (the project tree's top, `/tmp`, …).
    ///
    /// Separate from `Inside` because deleting the root is not the same act as
    /// deleting something in it: `rm -rf <project>` removes `.git`, every
    /// gate config and the worktree the session is running in. Callers that
    /// delete their operand outright (`rm`) must refuse this; callers whose
    /// operand is a SEARCH root rather than the thing deleted (`find`) may
    /// accept it under their own extra conditions.
    IsRoot {
        /// The safe root the operand resolved to.
        root: String,
    },
    /// Resolved, and under no safe root.
    Outside {
        /// The resolved absolute path that was classified.
        path: String,
    },
}

impl Placement {
    /// The root when this is [`Placement::Inside`], else `None`.
    ///
    /// Exists so call sites read `if let Some(root) = p.inside_root()` instead
    /// of matching, WITHOUT offering a bool: `is_inside()` next to
    /// `Outside`/`IsRoot` would invite `unwrap_or(false)`-shaped code, and the
    /// one thing this module must never do is answer "yes, inside" by default.
    pub fn inside_root(&self) -> Option<&str> {
        match self {
            Placement::Inside { root, .. } => Some(root.as_str()),
            Placement::IsRoot { .. } | Placement::Outside { .. } => None,
        }
    }
}

/// Characters that mean "this token is not a literal path".
///
/// Any of them anywhere in an operand makes the operand `Undetermined`. The set
/// is deliberately wider than strictly necessary (`!`, `~` mid-token, quotes
/// that the tokeniser may already have stripped) because every member of it
/// costs at most a false `Undetermined` — which is today's Deny, i.e. no
/// regression — while a MISSING member is a way to smuggle an unresolved
/// expansion past a placement check.
///
/// `{`/`}` are load-bearing beyond brace expansion: `{}` is `find -exec`'s
/// per-match placeholder, and treating it as a literal filename would have made
/// `find / -exec rm -rf {} +` classify against the cwd and look Inside.
const NOT_A_LITERAL_PATH: &[char] = &[
    '$', '`', '~', '*', '?', '[', ']', '{', '}', '(', ')', '\'', '"', '\\', '|', ';', '&', '<',
    '>', '!', '\n', '\t', ' ',
];

/// Absolute paths that are never a safe root even when the session's cwd or
/// `CLAUDE_PROJECT_DIR` names one.
///
/// A derived root also has to clear [`MIN_DERIVED_ROOT_COMPONENTS`]; this list
/// covers the deep-enough-but-still-catastrophic cases (`/mnt/c/Users` is three
/// components and holds every Windows profile on this host).
const NEVER_A_ROOT: &[&str] = &[
    "/",
    "/bin",
    "/boot",
    "/dev",
    "/etc",
    "/home",
    "/lib",
    "/lib32",
    "/lib64",
    "/mnt",
    "/mnt/c",
    "/mnt/c/Users",
    "/mnt/c/Windows",
    "/mnt/c/Program Files",
    "/opt",
    "/proc",
    "/root",
    "/run",
    "/sbin",
    "/srv",
    "/sys",
    "/usr",
    "/usr/bin",
    "/usr/lib",
    "/usr/local",
    "/usr/local/bin",
    "/usr/share",
    "/var",
    "/var/lib",
    "/var/log",
    "/Applications",
    "/Library",
    "/System",
    "/Users",
    "/Volumes",
];

/// A root derived from the session (cwd, `CLAUDE_PROJECT_DIR`) must be at least
/// this many components deep.
///
/// `/mnt` is one, `/home/yuki` is two — and two is where a plausible project
/// tree starts. The named temp roots (`/tmp`) are exempt: they are not derived
/// from anything the session can influence, they are a fixed list in this file.
const MIN_DERIVED_ROOT_COMPONENTS: usize = 2;

/// The fixed temp roots. Not derived from the environment, so they carry no
/// "what if the session's cwd is `/`" risk and are exempt from
/// [`MIN_DERIVED_ROOT_COMPONENTS`].
const TEMP_ROOTS: &[&str] = &["/tmp", "/var/tmp", "/private/tmp", "/private/var/tmp"];

/// Directories where a file's mere EXISTENCE changes what the machine does.
///
/// This list answers a different question from [`NEVER_A_ROOT`], even though
/// the two overlap. `NEVER_A_ROOT` asks "may a session's cwd be handed out as
/// a safe root?", and is broad on purpose — `/home`, `/Users`, `/Volumes` and
/// `/mnt` are on it because they hold EVERY user's data, not because writing
/// into them reconfigures anything. Those four are deliberately absent here:
/// denying every absolute write under `/Users` would re-create exactly the
/// friction that 0.2.59 measured and retired.
///
/// What is on this list is the set where creating a NEW file is itself the
/// payload:
///
///   * `/etc` (and its macOS real path `/private/etc`) — `sudoers.d`,
///     `paths.d`, `pam.d`, `cron.d`: drop-in directories where one new file is
///     a privilege, a `$PATH` entry, or an authentication rule.
///   * `/Library`, `/System` — macOS `LaunchDaemons`/`LaunchAgents` plists,
///     i.e. persistence. Note this is the ROOT-level `/Library`; a user's own
///     `~/Library` is under `/Users/…` and is not covered here.
///   * `/bin`, `/sbin`, `/lib`, `/lib32`, `/lib64`, `/usr`, `/opt` — the
///     executable and library search paths, where a new file shadows a command
///     that something else is about to run.
///   * `/boot`, `/proc`, `/sys`, `/root` — kernel, boot and the root account.
///
/// `/var` is NOT here, and must not be added: the macOS per-user temp
/// directory (`$TMPDIR`, `/var/folders/…`) and `/var/tmp` both live under it,
/// and both are legitimate scratch space. `/dev` is not here either — the
/// redirect rule's own safe list already names `/dev/null` and friends.
const SYSTEM_DIRS: &[&str] = &[
    "/bin",
    "/boot",
    "/etc",
    "/lib",
    "/lib32",
    "/lib64",
    "/opt",
    "/private/etc",
    "/proc",
    "/root",
    "/sbin",
    "/sys",
    "/usr",
    "/Library",
    "/System",
];

/// True when `path` names, or lies inside, a directory from [`SYSTEM_DIRS`].
///
/// # Why this is a separate axis from recoverability
///
/// [`crate::reversible`] answers "are the bytes at this path recoverable after
/// they are gone?", and it answers it correctly: a path that does not exist
/// yet has no bytes to lose, so it reports `NothingToDestroy`. That is true,
/// and for a file whose CONTENT is the thing at risk it is the whole question.
///
/// It is the wrong question for a file whose EXISTENCE is the thing at risk.
/// `/etc/sudoers.d/evil` does not exist, destroys nothing by being created, and
/// grants root. `reversible`'s own module doc reserves this case in as many
/// words — *"Recoverability is not the only reason to surface a command …
/// That axis stays where it already lives … and callers must consult it
/// FIRST"* — so this predicate is that reserved axis being filled in, not a
/// patch to the recoverability one.
///
/// # Why the answer is Deny and not Ask
///
/// [`crate::detect`]'s `protected_path_block` answers `Ask` because a human
/// legitimately edits their own gate config and only the human can say whether
/// a given edit strengthens or disarms it. There is no matching legitimate
/// case here: nothing an agent does autonomously has a reason to install a file
/// into `/etc` or `/Library/LaunchDaemons`, so the ambiguity that makes `Ask`
/// the honest answer over there does not exist over here.
///
/// # Undetermined
///
/// A relative path is not judged by this predicate at all — it returns `false`
/// — because "which directory is this relative to?" is a question this
/// predicate has no base to answer, and guessing one is how a wrong answer
/// would get manufactured.
///
/// The `false` return for a relative path means "not checked", NEVER "checked
/// and clean". The caller is responsible for handing this predicate an
/// absolute path when one exists. For redirect targets that is done by
/// `place_redirect_target` in [`crate::detect`] (f1c170ab): a relative target
/// after `cd /etc` is re-expressed as `/etc/<target>` before this runs, and one
/// after a `cd` that cannot be resolved is an Ask without reaching here.
/// Before that fix `cd /etc && echo x > paths.d/zz-evil` was ALLOW (measured
/// 2026-09-10 on the deployed 0.2.60 binary and on the commit preceding it).
/// A NEW caller that passes a relative path through this predicate reopens
/// exactly that hole.
///
/// The ABSOLUTE forms this predicate does judge are not fooled by `..`:
/// [`crate::exclude::normalize`] resolves it, so `/tmp/../etc/paths.d/x` and
/// `/etc/../etc/paths.d/x` both match (verified against the deployed binary).
pub fn is_inside_system_dir(path: &str) -> bool {
    let norm = crate::exclude::normalize(path);
    if !norm.starts_with('/') {
        return false;
    }
    SYSTEM_DIRS
        .iter()
        .any(|dir| norm == *dir || norm.starts_with(&format!("{dir}/")))
}

/// The roots this session is allowed to destroy things INSIDE, plus the means
/// to resolve an operand against them.
///
/// Construct with [`SafeRoots::none`] (no model — every operand
/// `Undetermined`, which reproduces this crate's pre-0.2.51 behaviour exactly)
/// or [`SafeRoots::new`].
#[derive(Debug, Clone)]
pub struct SafeRoots {
    /// Normalised, absolute, deduplicated, resolver-canonicalised.
    roots: Vec<String>,
    /// The session's working directory, absolute — the base for a relative
    /// operand when the analyser has not seen a `cd`. `None` disables relative
    /// operands entirely (they become `Undetermined`).
    cwd: Option<String>,
    resolver: Option<RealPathResolver>,
    /// The WORKTREE STORAGE ROOTS (`$HOME/.condukt/worktrees`, and the
    /// `.harness-worktrees` directory derived from `cwd` / `CLAUDE_PROJECT_DIR`),
    /// kept only when each exists as a real, non-symlinked directory below its
    /// canonicalised anchor and passes the derived-root rules (see
    /// [`worktree_storage_roots`]). Consulted ONLY by [`SafeRoots::classify_worktree`], which only
    /// the recursive-`rm` arm of [`crate::detect`] calls — see the module doc,
    /// "Worktree storage roots". Kept separate from `roots` on purpose: adding
    /// these to `roots` would hand every OTHER location-aware verb (`truncate`,
    /// `find -delete`, `git clean`, `chmod -R`, redirects) a confined `Ask`
    /// there, which nobody ruled on.
    worktree_roots: Vec<String>,
}

impl SafeRoots {
    /// No location model: every [`SafeRoots::classify`] call answers
    /// `Undetermined`, so every caller keeps the verdict it had before this
    /// module existed.
    ///
    /// This is what [`crate::detect::detect`] uses, which is why the library
    /// consumers that hand commands to `sh -c` with no human present (condukt's
    /// check runner, specguard's forge, daily's task runner) are unaffected by
    /// this whole feature.
    pub fn none() -> SafeRoots {
        SafeRoots {
            roots: Vec::new(),
            cwd: None,
            resolver: None,
            worktree_roots: Vec::new(),
        }
    }

    /// Build the model from values the CALLER read out of the environment.
    ///
    /// Env reading is the binary's job, not this module's: keeping `new`'s
    /// inputs explicit is what makes every rejection rule below reachable from
    /// a unit test (a root that is too shallow, a root on [`NEVER_A_ROOT`], a
    /// cwd that is `$HOME`, a `TMPDIR` that is relative).
    ///
    /// * `cwd` — the PreToolUse payload's `cwd`.
    /// * `project_dir` — `CLAUDE_PROJECT_DIR`. Often differs from `cwd` when
    ///   the session runs in a git worktree; BOTH become roots, because both are
    ///   trees this session legitimately works in.
    /// * `home` — `$HOME`. Not a root, and used only to REJECT a derived root
    ///   equal to it: a session whose cwd is the home directory must not turn
    ///   the whole home directory into a delete-freely region.
    /// * `tmpdir` — `$TMPDIR`, added to the fixed [`TEMP_ROOTS`] when absolute.
    /// * `resolver` — see [`RealPathResolver`]. `None` means no operand can
    ///   ever be resolved, hence no operand can ever be `Inside`.
    ///
    /// The same inputs also derive the worktree storage roots (see
    /// [`worktree_storage_candidates`]): `home` contributes
    /// `$HOME/.condukt/worktrees` (only when absolute), and `cwd` /
    /// `project_dir` each contribute their `.harness-worktrees` directory — each
    /// ONLY if it currently exists as a real directory with no symlinked
    /// component below its anchor (checked with `lstat`, real filesystem I/O,
    /// and only when `resolver` is `Some`; see [`worktree_storage_roots`]).
    pub fn new(
        cwd: Option<&str>,
        project_dir: Option<&str>,
        home: Option<&str>,
        tmpdir: Option<&str>,
        resolver: Option<RealPathResolver>,
    ) -> SafeRoots {
        let home_norm = home.and_then(normalize_abs);
        let mut roots: Vec<String> = Vec::new();
        for derived in [cwd, project_dir].into_iter().flatten() {
            if let Some(norm) = normalize_abs(derived) {
                if is_acceptable_derived_root(&norm, home_norm.as_deref()) {
                    push_root(&mut roots, norm, resolver);
                }
            }
        }
        for temp in TEMP_ROOTS.iter().copied().chain(tmpdir) {
            if let Some(norm) = normalize_abs(temp) {
                // A temp root still may not be `/` or a system directory: a
                // `TMPDIR=/usr` in the environment must not hand out `/usr`.
                if !NEVER_A_ROOT.contains(&norm.as_str()) {
                    push_root(&mut roots, norm, resolver);
                }
            }
        }
        // The cwd is only usable as a base for relative operands if it is
        // itself an absolute path we could resolve. It does NOT have to be a
        // root: `cd /home/yuki && rm -rf /tmp/x` resolves the operand against
        // `/tmp`, and the home directory being un-rootable is unrelated.
        // Worktree storage roots: see the module doc. The directory probe is
        // real filesystem I/O and is only used when the caller supplied a
        // resolver (i.e. it is the filesystem-aware hook binary).
        let probe: Option<DirProbe> = resolver.map(|_| is_real_dir as DirProbe);
        let worktree_roots =
            worktree_storage_roots(cwd, project_dir, home_norm.as_deref(), resolver, probe);
        let cwd = cwd
            .and_then(normalize_abs)
            .and_then(|c| resolve_with(&c, resolver));
        SafeRoots {
            roots,
            cwd,
            resolver,
            worktree_roots,
        }
    }

    /// True when there is no model at all — [`SafeRoots::classify`] can only
    /// answer `Undetermined`.
    pub fn is_empty(&self) -> bool {
        self.roots.is_empty()
    }

    /// The session's resolved working directory, if it has one. This is the
    /// base [`crate::detect`] passes for a segment whose working directory it
    /// has not seen changed.
    pub fn session_cwd(&self) -> Option<&str> {
        self.cwd.as_deref()
    }

    /// Where does `operand` land?
    ///
    /// `cwd` is the working directory to resolve a RELATIVE operand against —
    /// `None` means the caller does not know it (an unresolvable `cd $VAR`
    /// earlier on the line), and then a relative operand is `Undetermined`
    /// rather than assumed to sit in the session's cwd. That parameter is the
    /// whole reason `cd /usr && rm -rf lib` does not classify `lib` against the
    /// project tree.
    pub fn classify(&self, operand: &str, cwd: Option<&str>) -> Determination<Placement> {
        if self.roots.is_empty() {
            return Determination::undetermined(
                "blastguard has no safe-root model for this session (no project dir / cwd)",
            );
        }
        self.classify_against(&self.roots, operand, cwd)
    }

    /// Where does `operand` land relative to the WORKTREE STORAGE ROOTS?
    ///
    /// Identical resolution to [`SafeRoots::classify`] (literal-path check,
    /// lexical `..` collapsed BEFORE the injected symlink resolver runs — so a
    /// `..` after a symlink is misplaced, and callers must refuse `..`
    /// spellings themselves — longest-root match, `IsRoot`
    /// kept distinct from `Inside`) — only the root set differs. With no
    /// worktree storage roots (always the case for [`SafeRoots::none`]) the
    /// answer is `Undetermined`, which the caller must read as "not allowed".
    pub fn classify_worktree(&self, operand: &str, cwd: Option<&str>) -> Determination<Placement> {
        if self.worktree_roots.is_empty() {
            return Determination::undetermined(
                "blastguard has no worktree storage root for this session (no HOME / cwd / project dir)",
            );
        }
        self.classify_against(&self.worktree_roots, operand, cwd)
    }

    fn classify_against(
        &self,
        roots: &[String],
        operand: &str,
        cwd: Option<&str>,
    ) -> Determination<Placement> {
        if operand.is_empty() {
            return Determination::undetermined("empty operand");
        }
        if operand
            .chars()
            .any(|c| NOT_A_LITERAL_PATH.contains(&c) || c.is_control())
        {
            return Determination::undetermined(
                "operand is not a literal path (expansion, glob, quoting or a find -exec placeholder)",
            );
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

/// A worktree storage root candidate: the trusted ANCHOR (lexically
/// normalised, not yet resolved) and the components BELOW it that make up the
/// root. The anchor may be canonicalised; every component below it must be a
/// real, non-symlink directory (see [`worktree_storage_roots`]).
#[derive(Debug, Clone, PartialEq, Eq)]
struct WorktreeCandidate {
    anchor: String,
    below: Vec<&'static str>,
}

impl WorktreeCandidate {
    /// The candidate spelled lexically (anchor + components), unresolved.
    fn lexical(&self) -> String {
        let mut out = self.anchor.trim_end_matches('/').to_string();
        for c in &self.below {
            out.push('/');
            out.push_str(c);
        }
        out
    }
}

/// The worktree storage root candidates.
///
/// * `$HOME/.condukt/worktrees` — anchor `$HOME`, components `.condukt`,
///   `worktrees`; only when `home` normalised to an absolute path (an unset or
///   relative `HOME` contributes nothing).
/// * for each of `cwd`, `project_dir` that is absolute: if the path has a
///   component named exactly `.harness-worktrees`, the prefix up to and
///   including the LAST such component (the last, not the first, so a nested
///   spelling yields the narrower root — the restrictive choice); otherwise
///   `dirname(P)/.harness-worktrees`. Anchor = the prefix before that
///   component, component = `.harness-worktrees`.
///
/// No sanity rule and no filesystem check is applied here;
/// [`worktree_storage_roots`] applies both before a candidate may become a root.
fn worktree_storage_candidates(
    cwd: Option<&str>,
    project_dir: Option<&str>,
    home_norm: Option<&str>,
) -> Vec<WorktreeCandidate> {
    let mut out: Vec<WorktreeCandidate> = Vec::new();
    if let Some(home) = home_norm {
        out.push(WorktreeCandidate {
            anchor: home.to_string(),
            below: vec![".condukt", "worktrees"],
        });
    }
    for derived in [cwd, project_dir].into_iter().flatten() {
        let Some(p) = normalize_abs(derived) else {
            continue;
        };
        let comps: Vec<&str> = p.split('/').filter(|c| !c.is_empty()).collect();
        let parent: &[&str] = match comps.iter().rposition(|c| *c == WORKTREE_DIR_NAME) {
            Some(i) => &comps[..i],
            None => &comps[..comps.len().saturating_sub(1)],
        };
        let candidate = WorktreeCandidate {
            anchor: format!("/{}", parent.join("/")),
            below: vec![WORKTREE_DIR_NAME],
        };
        if !out.contains(&candidate) {
            out.push(candidate);
        }
    }
    out
}

/// "Does `path` exist, right now, as a directory that is NOT a symlink?"
///
/// `false` for a symlink (even one pointing at a directory), a regular file, a
/// missing path, and any IO error. Injected into [`worktree_storage_roots`] so
/// the unit tests can model a filesystem without creating one.
type DirProbe = fn(&str) -> bool;

/// The real [`DirProbe`]: `lstat` (never follows the final symlink). Any IO
/// error is `false`, i.e. "not a root".
fn is_real_dir(path: &str) -> bool {
    std::fs::symlink_metadata(path)
        .map(|m| m.file_type().is_dir())
        .unwrap_or(false)
}

/// Build the worktree storage roots (module doc, "Worktree storage roots").
///
/// For each candidate: the lexical spelling must pass the derived-root rules;
/// the anchor must resolve; each component below the resolved anchor must pass
/// `probe` (real, non-symlink directory); the result must pass the derived-root
/// rules again and must be its own real path per the resolver. Any failure —
/// including no resolver or no probe — drops the candidate. There is no
/// branch that keeps a candidate it could not check.
fn worktree_storage_roots(
    cwd: Option<&str>,
    project_dir: Option<&str>,
    home_norm: Option<&str>,
    resolver: Option<RealPathResolver>,
    probe: Option<DirProbe>,
) -> Vec<String> {
    let mut roots: Vec<String> = Vec::new();
    let Some(probe) = probe else {
        return roots;
    };
    for candidate in worktree_storage_candidates(cwd, project_dir, home_norm) {
        if !is_acceptable_derived_root(&candidate.lexical(), home_norm) {
            continue;
        }
        let Some(anchor_real) = resolve_with(&candidate.anchor, resolver) else {
            continue;
        };
        if !anchor_real.starts_with('/') {
            continue;
        }
        let mut cur = anchor_real.trim_end_matches('/').to_string();
        let mut all_real_dirs = true;
        for comp in &candidate.below {
            cur.push('/');
            cur.push_str(comp);
            if !probe(&cur) {
                all_real_dirs = false;
                break;
            }
        }
        if !all_real_dirs {
            continue;
        }
        if !is_acceptable_derived_root(&cur, home_norm) {
            continue;
        }
        // Belt and braces: a root built from a canonical anchor and non-symlink
        // components is already its own real path. If the resolver disagrees,
        // something changed between the two looks — not a root.
        if resolve_with(&cur, resolver).as_deref() != Some(cur.as_str()) {
            continue;
        }
        if !roots.contains(&cur) {
            roots.push(cur);
        }
    }
    roots
}

/// The directory name the harness keeps session worktrees in, as a sibling of
/// the main checkout (CLAUDE.md §8).
const WORKTREE_DIR_NAME: &str = ".harness-worktrees";

/// Push `root` unless an equal one is already present.
fn push_root(roots: &mut Vec<String>, root: String, resolver: Option<RealPathResolver>) {
    // A root that cannot be resolved is dropped rather than kept unresolved: an
    // unresolved root would be compared against RESOLVED operands, and the two
    // spellings of a symlinked project directory would then never match — which
    // fails in the restrictive direction, but silently and confusingly.
    let Some(real) = resolve_with(&root, resolver) else {
        return;
    };
    if !roots.contains(&real) {
        roots.push(real);
    }
}

/// Apply the injected resolver, or answer "cannot resolve" when there is none.
fn resolve_with(path: &str, resolver: Option<RealPathResolver>) -> Option<String> {
    let resolver = resolver?;
    resolver(path)
}

/// Rules a root DERIVED from the session (cwd, `CLAUDE_PROJECT_DIR`) must pass.
fn is_acceptable_derived_root(root: &str, home: Option<&str>) -> bool {
    if NEVER_A_ROOT.contains(&root) {
        return false;
    }
    if Some(root) == home {
        return false;
    }
    root.split('/').filter(|c| !c.is_empty()).count() >= MIN_DERIVED_ROOT_COMPONENTS
}

/// Lexically normalise an ABSOLUTE path: collapse `//` and `.`, resolve `..`,
/// drop the trailing slash. `None` for anything that is not absolute.
///
/// Backslashes are NOT translated to `/` (unlike [`crate::exclude::normalize`],
/// which does that to catch a Windows-style spelling of a protected path):
/// there, a wrong guess over-matches into the protected set, which is the safe
/// direction; here it would over-match into the SAFE set, which is not.
/// `\` is in [`NOT_A_LITERAL_PATH`] anyway, so such an operand never reaches
/// this function.
fn normalize_abs(path: &str) -> Option<String> {
    if !path.starts_with('/') {
        return None;
    }
    let mut out: Vec<&str> = Vec::new();
    for comp in path.split('/') {
        match comp {
            "" | "." => {}
            // POSIX: `/..` is `/`. Popping past the root simply stays at the
            // root, so an absolute path can never keep a `..`.
            ".." => {
                out.pop();
            }
            c => out.push(c),
        }
    }
    if out.is_empty() {
        return Some("/".to_string());
    }
    Some(format!("/{}", out.join("/")))
}

#[cfg(test)]
// Same carve-out as `exclude`'s and `rule_id`'s test modules: a test helper that
// reaches a state it is asserting cannot happen has nothing to return, and
// `deny(clippy::panic)` is aimed at production verdict paths, not at test
// failure reporting.
#[allow(clippy::panic)]
mod tests {
    use super::*;

    /// A resolver that resolves nothing but itself — models a filesystem with
    /// no symlinks, so the tests can exercise placement logic without touching
    /// the disk.
    fn identity(p: &str) -> Option<String> {
        Some(p.to_string())
    }

    /// A resolver where `/home/yuki/proj/link` is a symlink to `/usr/lib` — the escape
    /// route a lexical-only placement check cannot see.
    fn symlinked(p: &str) -> Option<String> {
        if p == "/home/yuki/proj/link" || p.starts_with("/home/yuki/proj/link/") {
            return Some(p.replacen("/home/yuki/proj/link", "/usr/lib", 1));
        }
        Some(p.to_string())
    }

    /// A resolver that always fails — models an I/O error or a permission
    /// problem while canonicalising.
    fn unresolvable(_p: &str) -> Option<String> {
        None
    }

    fn roots() -> SafeRoots {
        SafeRoots::new(
            Some("/home/yuki/proj"),
            Some("/home/yuki/proj"),
            Some("/home/yuki"),
            None,
            Some(identity),
        )
    }

    #[track_caller]
    fn known(d: Determination<Placement>) -> Placement {
        match d {
            Determination::Known(p) => p,
            Determination::Undetermined(u) => {
                panic!("expected a placement, got Undetermined({u:?})")
            }
        }
    }

    #[track_caller]
    fn assert_undetermined(d: Determination<Placement>) {
        assert!(
            matches!(d, Determination::Undetermined(_)),
            "expected Undetermined, got {d:?}"
        );
    }

    #[test]
    fn none_classifies_everything_as_undetermined() {
        // The compatibility contract: with no model, no caller can ever relax.
        let s = SafeRoots::none();
        assert!(s.is_empty());
        assert_undetermined(s.classify("/tmp/x", Some("/home/yuki/proj")));
        assert_undetermined(s.classify("target", Some("/home/yuki/proj")));
    }

    #[test]
    fn a_strict_descendant_of_the_project_is_inside() {
        let s = roots();
        assert_eq!(
            known(s.classify("/home/yuki/proj/target", None)).inside_root(),
            Some("/home/yuki/proj")
        );
        assert_eq!(
            known(s.classify("target", Some("/home/yuki/proj"))).inside_root(),
            Some("/home/yuki/proj")
        );
        assert_eq!(
            known(s.classify("./target/debug", Some("/home/yuki/proj"))).inside_root(),
            Some("/home/yuki/proj")
        );
        assert_eq!(
            known(s.classify("/tmp/scratch", None)).inside_root(),
            Some("/tmp")
        );
    }

    #[test]
    fn the_root_itself_is_not_inside() {
        // `rm -rf <project>` must not read as "inside the project".
        let s = roots();
        assert_eq!(
            known(s.classify("/home/yuki/proj", None)),
            Placement::IsRoot {
                root: "/home/yuki/proj".to_string()
            }
        );
        assert_eq!(
            known(s.classify(".", Some("/home/yuki/proj"))),
            Placement::IsRoot {
                root: "/home/yuki/proj".to_string()
            }
        );
        assert_eq!(known(s.classify("/tmp", None)).inside_root(), None);
    }

    #[test]
    fn system_paths_are_outside() {
        let s = roots();
        for p in [
            "/usr/lib",
            "/",
            "/etc/passwd",
            "/mnt/c/Windows",
            "/home/yuki",
        ] {
            assert_eq!(
                known(s.classify(p, None)).inside_root(),
                None,
                "{p} must not be Inside"
            );
        }
    }

    #[test]
    fn escaping_the_base_with_dotdot_is_not_inside() {
        let s = roots();
        // Lexically resolved, then judged where it really lands.
        assert_eq!(
            known(s.classify("../etc/passwd", Some("/home/yuki/proj/crates"))).inside_root(),
            Some("/home/yuki/proj")
        );
        assert_eq!(
            known(s.classify("../../usr/lib", Some("/home/yuki/proj/crates"))).inside_root(),
            None
        );
        assert_eq!(
            known(s.classify("/home/yuki/proj/../usr", None)).inside_root(),
            None
        );
    }

    #[test]
    fn a_relative_operand_with_no_known_cwd_is_undetermined() {
        // `cd $VAR && rm -rf target`: the base is unknown, so the operand is
        // not assumed to sit in the session's project.
        assert_undetermined(roots().classify("target", None));
    }

    #[test]
    fn a_symlink_out_of_the_tree_is_not_inside() {
        let s = SafeRoots::new(
            Some("/home/yuki/proj"),
            None,
            Some("/home/yuki"),
            None,
            Some(symlinked),
        );
        // Lexically it is inside the project; really it is `/usr/lib`.
        assert_eq!(
            known(s.classify("/home/yuki/proj/link", None)).inside_root(),
            None
        );
        assert_eq!(
            known(s.classify("link/x", Some("/home/yuki/proj"))).inside_root(),
            None
        );
    }

    #[test]
    fn an_unresolvable_path_is_undetermined_not_inside() {
        let s = SafeRoots::new(
            Some("/home/yuki/proj"),
            None,
            Some("/home/yuki"),
            None,
            Some(unresolvable),
        );
        // Every root failed to resolve too, so there is no model at all — which
        // is itself the restrictive answer.
        assert!(s.is_empty());
        assert_undetermined(s.classify("/home/yuki/proj/target", Some("/home/yuki/proj")));
    }

    #[test]
    fn no_resolver_means_nothing_is_inside() {
        // The purity contract for library consumers: `detect` without a
        // resolver cannot relax anything.
        let s = SafeRoots::new(
            Some("/home/yuki/proj"),
            None,
            Some("/home/yuki"),
            None,
            None,
        );
        assert!(s.is_empty());
        assert_undetermined(s.classify("/home/yuki/proj/target", Some("/home/yuki/proj")));
    }

    #[test]
    fn shell_constructs_are_undetermined() {
        let s = roots();
        for operand in [
            "$HOME", "${DIR}", "~", "~/.cache", "*", "target/*", "a?b", "[abc]", "{}", "{a,b}",
            "`pwd`", "$(pwd)", "a b", "a\\ b", "'x'", "\"x\"",
        ] {
            assert_undetermined(s.classify(operand, Some("/home/yuki/proj")));
        }
    }

    #[test]
    fn find_exec_placeholder_never_classifies_against_the_cwd() {
        // The bypass this rejection exists for: `find / -exec rm -rf {} +`
        // re-analysed as `rm -rf {}` must not read `{}` as `<cwd>/{}`.
        assert_undetermined(roots().classify("{}", Some("/home/yuki/proj")));
    }

    #[test]
    fn a_too_broad_session_dir_is_not_accepted_as_a_root() {
        // cwd = `/` or a system dir must not hand out a delete-freely region.
        for broad in ["/", "/mnt", "/mnt/c", "/usr", "/home", "/mnt/c/Users"] {
            let s = SafeRoots::new(Some(broad), None, Some("/home/yuki"), None, Some(identity));
            assert_eq!(
                known(s.classify("/usr/lib", None)).inside_root(),
                None,
                "cwd={broad} must not make /usr/lib Inside"
            );
            // The temp roots are still present, so the model is not empty —
            // what must be absent is the broad root itself.
            assert!(
                !s.roots.contains(&broad.to_string()),
                "cwd={broad} must not become a root"
            );
        }
    }

    #[test]
    fn home_itself_is_never_a_root_even_when_it_is_the_cwd() {
        let s = SafeRoots::new(
            Some("/home/yuki"),
            None,
            Some("/home/yuki"),
            None,
            Some(identity),
        );
        assert_eq!(
            known(s.classify("/home/yuki/.cache", None)).inside_root(),
            None
        );
        // But it IS still a usable base for relative operands, so that
        // `cd ~ && rm -rf /tmp/x` still resolves the operand.
        assert_eq!(s.session_cwd(), Some("/home/yuki"));
    }

    #[test]
    fn a_shallow_but_named_temp_root_is_accepted() {
        // `/tmp` is one component, so it must be exempt from the
        // MIN_DERIVED_ROOT_COMPONENTS rule that rejects `/mnt`.
        let s = roots();
        assert_eq!(
            known(s.classify("/tmp/x/y", None)).inside_root(),
            Some("/tmp")
        );
    }

    #[test]
    fn tmpdir_from_the_environment_is_added_but_sanitised() {
        let ok = SafeRoots::new(
            Some("/home/yuki/proj"),
            None,
            Some("/home/yuki"),
            Some("/mnt/c/tmp"),
            Some(identity),
        );
        assert_eq!(
            known(ok.classify("/mnt/c/tmp/x", None)).inside_root(),
            Some("/mnt/c/tmp")
        );
        for bad in ["/usr", "relative/tmp", "/"] {
            let s = SafeRoots::new(
                Some("/home/yuki/proj"),
                None,
                Some("/home/yuki"),
                Some(bad),
                Some(identity),
            );
            assert!(
                !s.roots.contains(&bad.to_string()),
                "TMPDIR={bad} must not become a root"
            );
        }
    }

    #[test]
    fn the_worktree_and_the_project_dir_are_both_roots() {
        // A session working in a git worktree has cwd = the worktree and
        // CLAUDE_PROJECT_DIR = the main tree. Both are legitimate.
        let s = SafeRoots::new(
            Some("/home/yuki/wt/session-1"),
            Some("/home/yuki/proj"),
            Some("/home/yuki"),
            None,
            Some(identity),
        );
        assert_eq!(
            known(s.classify("/home/yuki/wt/session-1/target", None)).inside_root(),
            Some("/home/yuki/wt/session-1")
        );
        assert_eq!(
            known(s.classify("/home/yuki/proj/target", None)).inside_root(),
            Some("/home/yuki/proj")
        );
    }

    #[test]
    fn the_longest_matching_root_is_reported() {
        let s = SafeRoots::new(
            Some("/tmp/nested-proj"),
            None,
            Some("/home/yuki"),
            None,
            Some(identity),
        );
        assert_eq!(
            known(s.classify("/tmp/nested-proj/x", None)).inside_root(),
            Some("/tmp/nested-proj")
        );
    }

    #[test]
    fn normalize_abs_rejects_relatives_and_collapses_the_rest() {
        assert_eq!(normalize_abs("relative/x"), None);
        assert_eq!(normalize_abs("/a//b/./c"), Some("/a/b/c".to_string()));
        assert_eq!(normalize_abs("/a/b/../c"), Some("/a/c".to_string()));
        assert_eq!(normalize_abs("/.."), Some("/".to_string()));
        assert_eq!(normalize_abs("/a/"), Some("/a".to_string()));
    }

    // ---- worktree storage roots: private-helper unit tests -------------------
    // The probe models `lstat`: only the listed paths are real directories.

    fn probe_real(p: &str) -> bool {
        matches!(
            p,
            "/home/yuki/.condukt"
                | "/home/yuki/.condukt/worktrees"
                | "/home/yuki/src/.harness-worktrees"
        )
    }

    fn probe_nothing(_p: &str) -> bool {
        false
    }

    fn probe_condukt_is_symlink(p: &str) -> bool {
        // `.condukt` is a symlink (lstat says not-a-dir) even though its
        // `worktrees` child would look fine through the link.
        p == "/home/yuki/.condukt/worktrees"
    }

    #[test]
    fn worktree_roots_require_real_dirs_below_the_anchor() {
        let r = worktree_storage_roots(
            Some("/home/yuki/src/proj"),
            None,
            Some("/home/yuki"),
            Some(identity),
            Some(probe_real),
        );
        assert_eq!(
            r,
            vec![
                "/home/yuki/.condukt/worktrees".to_string(),
                "/home/yuki/src/.harness-worktrees".to_string()
            ]
        );
    }

    #[test]
    fn worktree_roots_absent_or_symlinked_or_unprobed_are_not_roots() {
        for probe in [Some(probe_nothing as DirProbe), None] {
            assert!(worktree_storage_roots(
                Some("/home/yuki/src/proj"),
                None,
                Some("/home/yuki"),
                Some(identity),
                probe,
            )
            .is_empty());
        }
        assert!(worktree_storage_roots(
            None,
            None,
            Some("/home/yuki"),
            Some(identity),
            Some(probe_condukt_is_symlink),
        )
        .is_empty());
        // No resolver: the anchor cannot be canonicalised, so no root.
        assert!(worktree_storage_roots(
            Some("/home/yuki/src/proj"),
            None,
            Some("/home/yuki"),
            None,
            Some(probe_real),
        )
        .is_empty());
    }
}
