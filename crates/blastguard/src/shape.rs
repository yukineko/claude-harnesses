//! The learning shape of a tool call (spec
//! `docs/specs/blastguard-ask-learning.md`, user ruling 2026-11-28).
//!
//! A shape is `rule_id` + the statically resolved TARGET CLASS + the verbs
//! (basenames of the command words of every simple command; the tool name for
//! a non-Bash tool). This module turns the facts blastguard's own evaluator
//! resolved ([`Facts`], produced from the same `detect::ast` script the
//! detectors judge) into a [`harness_core::asklearn::Shape`]. It never parses
//! the raw command text with string heuristics: a target the evaluator could
//! not resolve is [`Target::Undetermined`], and an undetermined target makes
//! the class `Undetermined` — never `None`, never a placed class.
//!
//! Not pure: path classification uses the injected resolver held by
//! [`SafeRoots`]. Which git repository / work tree a directory belongs to is
//! never guessed from `.git` entries or config text: it is git's own answer
//! (`git rev-parse --show-toplevel --git-dir --git-common-dir`, [`GitLayout`]),
//! asked once per directory with a timeout. A failed, timed-out or
//! unparsable probe, or a hook-process environment that redirects git
//! ([`git_env_redirected`]), is "not determined": no git class, never Allow.
//! A git command whose repository lies in a session scratchpad is
//! `Undetermined` without asking git: never Allow-eligible.

use crate::scope::SafeRoots;
use harness_core::asklearn::{Shape, TargetClass, UNRESOLVED_VERB};
use harness_core::verdict::Determination;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Environment variables that make git act on a repository, work tree, index,
/// object store or configuration other than the one found from the working
/// directory. If the hook process has any of them (set at all, even empty),
/// the git class is not determined: the session's Bash tool inherits the same
/// environment, so what git would really use cannot be read from the cwd.
pub const GIT_REDIRECT_ENV: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_NAMESPACE",
    "GIT_PREFIX",
    "GIT_QUARANTINE_PATH",
    "GIT_CONFIG",
    "GIT_CONFIG_GLOBAL",
    "GIT_CONFIG_SYSTEM",
    "GIT_CONFIG_NOSYSTEM",
    "GIT_CONFIG_COUNT",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CEILING_DIRECTORIES",
    "GIT_DISCOVERY_ACROSS_FILESYSTEM",
];

/// Whether any [`GIT_REDIRECT_ENV`] variable is present, read through `get`
/// (the process environment in production, a map in tests).
pub fn git_env_redirected(get: impl Fn(&str) -> Option<std::ffi::OsString>) -> bool {
    GIT_REDIRECT_ENV.iter().any(|k| get(k).is_some())
}

/// How long one `git rev-parse` may take before the answer is "not determined".
const GIT_PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// What git itself says about the directory it was asked from (all canonical).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitLayout {
    /// `--show-toplevel`: the work tree git uses.
    pub toplevel: PathBuf,
    /// `--git-dir`.
    pub git_dir: PathBuf,
    /// `--git-common-dir`.
    pub common_dir: PathBuf,
}

impl GitLayout {
    /// A main working tree: the git dir is the common dir.
    fn is_main(&self) -> bool {
        self.git_dir == self.common_dir
    }

    /// The checkout root of a LINKED worktree, only when git's layout and the
    /// git dir's own registry agree: the git dir is `<common>/worktrees/<name>`
    /// and its `gitdir` back-link names `<toplevel>/.git`, a real file. A work
    /// tree redirected elsewhere (`core.worktree` in `config.worktree`) has a
    /// toplevel whose `.git` is not that back-link, so it is not a linked root.
    fn linked_root(&self) -> Option<PathBuf> {
        if self.is_main() || self.git_dir.parent() != Some(&self.common_dir.join("worktrees")) {
            return None;
        }
        let Determination::Known(Some(back)) =
            harness_core::boundary::read_to_string(&self.git_dir.join("gitdir"))
        else {
            return None;
        };
        // A relative back-link (`--relative-paths`) is read against the git dir.
        let back = std::fs::canonicalize(self.git_dir.join(back.trim())).ok()?;
        let dot_git = self.toplevel.join(".git");
        // Unreadable `.git` = not shown to be a linked checkout.
        let Ok(meta) = std::fs::symlink_metadata(&dot_git) else {
            return None;
        };
        (meta.is_file() && std::fs::canonicalize(&dot_git).ok()? == back)
            .then(|| self.toplevel.clone())
    }
}

/// Per-process memo of git probes (a directory is asked once per hook call).
#[derive(Debug, Default)]
pub struct GitCache(RefCell<HashMap<PathBuf, Option<GitLayout>>>);

/// Ask git for the layout of directory `dir`; `None` for every way of not
/// getting a trustworthy answer (not a directory, spawn failure, timeout,
/// non-zero exit -- including "not a repository" and a bare repository --,
/// wrong line count, a path that does not canonicalise).
fn probe_git(dir: &Path) -> Option<GitLayout> {
    if !dir.is_dir() {
        return None;
    }
    let mut cmd = std::process::Command::new("git");
    cmd.current_dir(dir)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(std::process::Stdio::null())
        .args([
            "rev-parse",
            "--path-format=absolute",
            "--show-toplevel",
            "--git-dir",
            "--git-common-dir",
        ]);
    let Determination::Known(out) =
        harness_core::boundary::run_with_timeout(&mut cmd, GIT_PROBE_TIMEOUT)
    else {
        return None;
    };
    let Determination::Known(stdout) = out.stdout_allowing(&[0]) else {
        return None;
    };
    let lines: Vec<&str> = stdout
        .strip_suffix('\n')
        .unwrap_or(&stdout)
        .split('\n')
        .collect();
    let [top, gd, cd] = lines.as_slice() else {
        return None;
    };
    let canon = |s: &str| {
        let p = Path::new(s);
        if p.is_absolute() {
            std::fs::canonicalize(p).ok()
        } else {
            None
        }
    };
    Some(GitLayout {
        toplevel: canon(top)?,
        git_dir: canon(gd)?,
        common_dir: canon(cd)?,
    })
}

/// The deepest existing directory at or above `p` (what `git -C` can start in).
fn existing_dir(p: &str) -> Option<PathBuf> {
    let mut cur = Path::new(p);
    loop {
        if cur.is_dir() {
            return Some(cur.to_path_buf());
        }
        cur = cur.parent()?;
    }
}

/// git's layout for `abs` (its deepest existing directory), memoised; `None`
/// when the hook environment redirects git or the probe was not determined.
fn git_layout(abs: &str, ctx: &Ctx<'_>) -> Option<GitLayout> {
    if ctx.git_env_redirected {
        return None;
    }
    let dir = existing_dir(abs)?;
    if let Some(hit) = ctx.git.0.borrow().get(&dir) {
        return hit.clone();
    }
    let got = probe_git(&dir);
    ctx.git.0.borrow_mut().insert(dir, got.clone());
    got
}

/// One write target as the evaluator resolved it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// A literal path (absolute after a literal `cd`, else as written).
    Path(String),
    /// The evaluator could not determine the target.
    Undetermined,
    /// A git write acts on the REPOSITORY containing this path (the effective
    /// cwd, a `-C` directory, or the tree a `git worktree remove` deletes). A
    /// relative path is read against the payload cwd; `.` is the payload cwd.
    /// Its class is the kind of tree the path lies in (linked worktree /
    /// scratchpad / MAIN working tree), not the path's own location.
    GitRepo(String),
}

/// What the evaluator established about a Bash command line.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Facts {
    /// Basenames of every simple command's literal command word;
    /// [`UNRESOLVED_VERB`] for a command word it could not resolve.
    pub verbs: Vec<String>,
    pub targets: Vec<Target>,
}

/// The session facts classification needs.
#[derive(Debug)]
pub struct Ctx<'a> {
    pub home: Option<&'a str>,
    /// The payload `cwd`.
    pub cwd: Option<&'a str>,
    pub tmpdir: Option<&'a str>,
    pub scope: &'a SafeRoots,
    /// The hook process environment redirects git ([`git_env_redirected`]).
    pub git_env_redirected: bool,
    /// Memo of the git probes of this call.
    pub git: &'a GitCache,
}

/// Facts for a Bash command whose evaluation failed: the verb is unresolved
/// and the target is undetermined. Such a shape can learn a Deny but never an
/// Allow ([`Shape::allow_permitted`]).
pub fn undetermined_facts() -> Facts {
    Facts {
        verbs: vec![UNRESOLVED_VERB.to_string()],
        targets: vec![Target::Undetermined],
    }
}

/// The shape of a call. `bash` is the evaluator's facts for a Bash command
/// (`None` = the evaluation was undetermined).
pub fn shape_of(
    tool: &str,
    tool_input: Option<&serde_json::Value>,
    bash: Option<Facts>,
    rule_id: &str,
    ctx: &Ctx<'_>,
) -> Shape {
    let (verbs, targets) = match tool {
        "Bash" => {
            let f = bash.unwrap_or_else(undetermined_facts);
            (f.verbs, f.targets)
        }
        _ => {
            let path = ["file_path", "notebook_path"]
                .iter()
                .find_map(|k| tool_input?.get(*k)?.as_str());
            let targets = match path {
                Some(p) if !p.trim().is_empty() => vec![Target::Path(p.to_string())],
                // A file tool whose path cannot be read has an undetermined
                // target; a tool with no path at all has none.
                _ if matches!(tool, "Write" | "Edit" | "MultiEdit" | "NotebookEdit") => {
                    vec![Target::Undetermined]
                }
                _ => Vec::new(),
            };
            (vec![tool.to_string()], targets)
        }
    };
    // A git write whose repository was named but is not shown to be a main or
    // a linked working tree (a nonexistent / non-repo directory, `-C ../`)
    // is not determined either: the verb carries `?`, so the shape can learn
    // a Deny but never an Allow (CLAUDE.md §3).
    let mut verbs = verbs;
    if targets.iter().any(
        |t| matches!(t, Target::GitRepo(p) if class_of_repo(p, ctx) == TargetClass::Undetermined),
    ) {
        verbs.push(UNRESOLVED_VERB.to_string());
    }
    let mut target_class = class_of(&targets, ctx);
    // Round 9: a destination-writing tool the Allow gate does not model
    // (`cp` / `mv` / `install` / `ln` / `rsync` / `ditto` / `mkdir` / `touch`)
    // does not place its write: `cp f DIR` writes `DIR/<f>` through a symlink
    // child and `mkdir` / `touch` carry no target row. Its class is never an
    // Allow-eligible one; System / MainTree / ProtectedConfig / Home keep
    // their own (more restrictive or equal) class.
    if matches!(
        target_class,
        TargetClass::None | TargetClass::Scratchpad | TargetClass::Worktree
    ) && verbs
        .iter()
        .any(|v| UNPLACED_WRITERS.contains(&v.split(' ').next().unwrap_or("")))
    {
        target_class = TargetClass::Undetermined;
    }
    Shape {
        rule_id: rule_id.to_string(),
        target_class,
        verbs,
    }
}

/// Tools whose write destination the shape cannot place (see [`shape_of`]).
const UNPLACED_WRITERS: &[&str] = &[
    "cp", "gcp", "mv", "gmv", "install", "ginstall", "ln", "gln", "rsync", "ditto", "mkdir",
    "touch",
];

/// The class of a set of targets: the most dangerous one wins
/// (System > MainTree > ProtectedConfig > Undetermined > Home > Worktree >
/// Scratchpad > None).
pub fn class_of(targets: &[Target], ctx: &Ctx<'_>) -> TargetClass {
    fn rank(c: TargetClass) -> u8 {
        match c {
            TargetClass::System => 7,
            TargetClass::MainTree => 6,
            TargetClass::ProtectedConfig => 5,
            TargetClass::Undetermined => 4,
            TargetClass::Home => 3,
            TargetClass::Worktree => 2,
            TargetClass::Scratchpad => 1,
            TargetClass::None => 0,
        }
    }
    targets
        .iter()
        .map(|t| match t {
            Target::Undetermined => TargetClass::Undetermined,
            Target::Path(p) => class_of_path(p, ctx),
            Target::GitRepo(p) => class_of_repo(p, ctx),
        })
        .max_by_key(|c| rank(*c))
        .unwrap_or(TargetClass::None)
}

fn has_component(path: &str, name: &str) -> bool {
    path.split('/').any(|c| c == name)
}

fn is_claude_settings(path: &str) -> bool {
    let mut it = path.rsplit('/');
    let base = it.next().unwrap_or("");
    let parent = it.next().unwrap_or("");
    parent == ".claude" && base.starts_with("settings") && base.ends_with(".json")
}

/// Class of one literal path, decided by the path it REALLY is, not by its
/// spelling: the canonical path of the longest existing prefix (plus the
/// not-yet-existing remainder) is classed too. System / protected config on
/// either wins; a class the spelling and the real path disagree on, or a path
/// whose real location cannot be determined (nothing of it canonicalises), is
/// `Undetermined` (never Allow-learnable).
pub fn class_of_path(raw: &str, ctx: &Ctx<'_>) -> TargetClass {
    let lexical = class_of_path_spelled(raw, ctx);
    if matches!(
        lexical,
        TargetClass::System | TargetClass::ProtectedConfig | TargetClass::Undetermined
    ) {
        return lexical;
    }
    let Some(abs) = absolute_of(raw, ctx) else {
        return TargetClass::Undetermined;
    };
    let Some(real) = realpath_opt(&abs) else {
        return TargetClass::Undetermined;
    };
    // The session roots are compared against the real path, so they are
    // canonicalised too (a symlinked `$TMPDIR` / cwd / home is not a mismatch).
    let canon_tmp = ctx.tmpdir.and_then(realpath_existing);
    let canon_cwd = ctx.cwd.and_then(realpath_existing);
    let canon_home = ctx.home.and_then(realpath_existing);
    let real_ctx = Ctx {
        tmpdir: canon_tmp.as_deref().or(ctx.tmpdir),
        cwd: canon_cwd.as_deref().or(ctx.cwd),
        home: canon_home.as_deref().or(ctx.home),
        ..*ctx
    };
    let physical = class_of_path_spelled(&real, &real_ctx);
    if matches!(physical, TargetClass::System | TargetClass::ProtectedConfig) {
        return physical;
    }
    // An Allow-eligible class needs a spelling that IS the real path: no `..`
    // (the kernel resolves it physically, the spelling lexically), no symlink
    // on the way, and the same case the file system holds (macOS folds case).
    if physical == lexical && spelling_is_real(raw, &abs, &real) {
        lexical
    } else {
        TargetClass::Undetermined
    }
}

/// Whether the spelling `raw` of the absolute path `abs` (real path `real`)
/// names that very path: no `..` component, and the canonical path of the
/// longest existing prefix equals the spelling byte for byte (so no symlink on
/// the way and no case difference).
fn spelling_is_real(raw: &str, abs: &str, real: &str) -> bool {
    // Compared component by component and byte for byte (`.` and repeated
    // slashes are no difference; a case or symlink difference is).
    !raw.split('/').any(|c| c == "..")
        && std::path::Path::new(real)
            .components()
            .eq(std::path::Path::new(abs).components())
        && no_symlink_component(abs)
}

/// Whether no component of `abs` is a symlink (dangling or not): each prefix
/// is `lstat`ed, so a write through a link whose target does not exist yet is
/// seen. A component that does not exist ends the walk (nothing below it can
/// be a link); any other lstat failure is "cannot tell" = `false`.
fn no_symlink_component(abs: &str) -> bool {
    let mut cur = std::path::PathBuf::new();
    for comp in std::path::Path::new(abs).components() {
        cur.push(comp);
        match std::fs::symlink_metadata(&cur) {
            Ok(m) if m.file_type().is_symlink() => return false,
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return true,
            Err(_) => return false,
        }
    }
    true
}

/// `p` with every symlink resolved ([`realpath`]), or `None` when no prefix of
/// it canonicalises.
fn realpath_opt(p: &str) -> Option<String> {
    let mut cur = std::path::Path::new(p);
    loop {
        if std::fs::canonicalize(cur).is_ok() {
            return Some(realpath(p));
        }
        cur = cur.parent()?;
    }
}

/// Class of one literal path by its spelling alone.
fn class_of_path_spelled(raw: &str, ctx: &Ctx<'_>) -> TargetClass {
    let raw = raw.trim();
    if raw.is_empty() || raw.contains(['$', '`', '*', '?', '[', '{', '\n']) {
        return TargetClass::Undetermined;
    }
    // Protected config is judged on the spelling as written too: a relative
    // `.githooks/x` is protected whatever the cwd is.
    let protected = |p: &str| {
        crate::exclude::touches_protected(p)
            || crate::exclude::is_workflow_path(p)
            || is_claude_settings(&crate::exclude::normalize(p))
    };
    if protected(raw) {
        return TargetClass::ProtectedConfig;
    }
    let expanded = match (raw.strip_prefix("~/"), raw == "~", ctx.home) {
        (Some(rest), _, Some(h)) => format!("{}/{rest}", h.trim_end_matches('/')),
        (_, true, Some(h)) => h.to_string(),
        (Some(_), _, None) | (_, true, None) => return TargetClass::Undetermined,
        _ => raw.to_string(),
    };
    let abs = if expanded.starts_with('/') {
        expanded
    } else if raw.starts_with('~') {
        return TargetClass::Undetermined;
    } else {
        match ctx.cwd {
            Some(c) if c.starts_with('/') => format!("{}/{expanded}", c.trim_end_matches('/')),
            _ => return TargetClass::Undetermined,
        }
    };
    let norm = crate::exclude::normalize(&abs);
    if !norm.starts_with('/') {
        return TargetClass::Undetermined;
    }
    if protected(&norm) {
        return TargetClass::ProtectedConfig;
    }
    // Scratchpad before System: a session scratchpad under `$TMPDIR` lives in
    // a system-listed `/private/var/folders`.
    if has_component(&norm, "scratchpad") {
        let scratch_root = ["/tmp/claude-", "/private/tmp/claude-"]
            .iter()
            .any(|p| norm.starts_with(p))
            || ctx
                .tmpdir
                .map(|t| t.trim_end_matches('/'))
                .is_some_and(|t| !t.is_empty() && norm.starts_with(&format!("{t}/")));
        if scratch_root {
            return TargetClass::Scratchpad;
        }
    }
    if crate::deletion::is_system_path(&norm) || crate::scope::is_inside_system_dir(&norm) {
        return TargetClass::System;
    }
    // Round 10: Worktree is "strictly inside THE linked-worktree checkout this
    // session's cwd belongs to" and nothing else: another session's linked
    // worktree, a checkout's own root and the storage container
    // (`.harness-worktrees/`, `~/.condukt/worktrees/<run>`) are not Worktree
    // (they fall to Home / Undetermined below, never Allow-eligible).
    if let Some(root) = cwd_linked_root(ctx) {
        if strictly_inside_checkout(&norm, &root, ctx) {
            return TargetClass::Worktree;
        }
    }
    // The session's own tree (the payload cwd). Being under the cwd places
    // nothing: the class is the KIND of tree the cwd is (spec Components 1):
    // a main working tree is MainTree (never Allow), and a cwd that is no git
    // tree (`~/.claude`, `~/src`, `$HOME`) gives no class here, so the path
    // falls to Home / Undetermined below.
    if let Some(c) = ctx.cwd.map(crate::exclude::normalize) {
        if c.starts_with('/')
            && (norm == c || norm.starts_with(&format!("{}/", c.trim_end_matches('/'))))
            && git_layout(&realpath(&c), ctx).is_some_and(|l| l.is_main())
        {
            return TargetClass::MainTree;
        }
    }
    if let Some(h) = ctx.home.map(|h| h.trim_end_matches('/')) {
        if !h.is_empty() && norm.starts_with(&format!("{h}/")) {
            return TargetClass::Home;
        }
    }
    TargetClass::Undetermined
}

/// The checkout root of the linked worktree the session's cwd belongs to, or
/// `None` when git does not say the cwd is inside a linked worktree (a main
/// tree, no git tree, a redirected work tree, a failed probe, no cwd).
fn cwd_linked_root(ctx: &Ctx<'_>) -> Option<PathBuf> {
    let c = crate::exclude::normalize(ctx.cwd?);
    if !c.starts_with('/') {
        return None;
    }
    git_layout(&realpath(&c), ctx)?.linked_root()
}

/// Whether `abs` lies STRICTLY inside the checkout `root`: below it, not the
/// root itself, and git says the directory it is in belongs to that very
/// linked checkout (a submodule's or another checkout nested below is a
/// different tree).
fn strictly_inside_checkout(abs: &str, root: &Path, ctx: &Ctx<'_>) -> bool {
    let p = Path::new(abs.trim_end_matches('/'));
    p != root
        && p.starts_with(root)
        && git_layout(abs, ctx)
            .and_then(|l| l.linked_root())
            .as_deref()
            == Some(root)
}

/// The absolute, normalised spelling of `raw` (as [`class_of_path`] reads it),
/// or `None` when it cannot be determined.
fn absolute_of(raw: &str, ctx: &Ctx<'_>) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() || raw.contains(['$', '`', '*', '?', '[', '{', '\n']) {
        return None;
    }
    let expanded = match (raw.strip_prefix("~/"), raw == "~", ctx.home) {
        (Some(rest), _, Some(h)) => format!("{}/{rest}", h.trim_end_matches('/')),
        (_, true, Some(h)) => h.to_string(),
        (Some(_), _, None) | (_, true, None) => return None,
        _ => raw.to_string(),
    };
    let abs = if expanded.starts_with('/') {
        expanded
    } else if raw.starts_with('~') {
        return None;
    } else {
        match ctx.cwd {
            Some(c) if c.starts_with('/') => format!("{}/{expanded}", c.trim_end_matches('/')),
            _ => return None,
        }
    };
    let norm = crate::exclude::normalize(&abs);
    norm.starts_with('/').then_some(norm)
}

/// Class of the REPOSITORY a git write acts on (spec rule: a git write takes
/// its class from the tree it modifies, not from `none`). System, protected
/// config keep their own class. A path that is a session scratchpad both
/// spelled and really is `Undetermined` WITHOUT asking git (a git command
/// there is never Allow-eligible: the repository git uses from a scratchpad
/// may be any other one; `rm` / file tools there stay `Scratchpad`, which
/// [`class_of_path`] judges from the path alone). Every other tree is git's own answer
/// ([`GitLayout`]): only the linked worktree the session's cwd belongs to is
/// `Worktree` (a worktree storage path is not); the main working tree is
/// `MainTree` (never Allow-learnable, CLAUDE.md §8); anything git did not
/// answer, or answered as something else, is `Undetermined`.
pub fn class_of_repo(raw: &str, ctx: &Ctx<'_>) -> TargetClass {
    let Some(abs) = absolute_of(raw, ctx) else {
        return TargetClass::Undetermined;
    };
    // The class of the tree the path REALLY is: a symlink under a scratchpad
    // that points at the main working tree is the main tree.
    let real = realpath(&abs);
    let canon_tmp = ctx.tmpdir.and_then(realpath_existing);
    let real_ctx = Ctx {
        tmpdir: canon_tmp.as_deref().or(ctx.tmpdir),
        ..*ctx
    };
    let lexical = class_of_path_spelled(raw, ctx);
    let physical = class_of_path_spelled(&real, &real_ctx);
    for c in [lexical, physical] {
        if matches!(c, TargetClass::System | TargetClass::ProtectedConfig) {
            return c;
        }
    }
    let real_spelling = spelling_is_real(raw, &abs, &real);
    if lexical == TargetClass::Scratchpad && physical == TargetClass::Scratchpad {
        if !real_spelling {
            return TargetClass::Undetermined;
        }
        // Round 13: no git class for a scratchpad (what git uses from there
        // is the repository's own business: hooks, `core.fsmonitor`,
        // `core.worktree`, a repository above it). `Undetermined` also marks
        // the verb `?` ([`shape_of`]), so the shape is never Allow-eligible.
        return TargetClass::Undetermined;
    }
    // The repository's kind is git's answer, never a substring of the
    // spelling (the `.harness-worktrees/` container is no repository). Only
    // the linked worktree the session's cwd belongs to is `Worktree` (the
    // repository IS that checkout, root included, unlike a path target);
    // another linked worktree is `Undetermined`.
    let placed = match git_layout(&real, ctx) {
        None => TargetClass::Undetermined,
        Some(l) if l.is_main() => TargetClass::MainTree,
        Some(l) => match (l.linked_root(), cwd_linked_root(ctx)) {
            (Some(root), Some(own)) if root == own => TargetClass::Worktree,
            _ => TargetClass::Undetermined,
        },
    };
    // Only the Allow-eligible class needs the spelling to be the real path.
    if placed == TargetClass::Worktree && !real_spelling {
        return TargetClass::Undetermined;
    }
    placed
}

/// `p` with every symlink resolved: the canonical path of its nearest existing
/// ancestor plus the (not yet existing) remainder. A path whose ancestors
/// cannot be resolved is returned as written (its repository kind is then read
/// from the spelling and is `Unknown` unless it is a real tree).
fn realpath(p: &str) -> String {
    let mut rest: Vec<&str> = Vec::new();
    let mut cur = std::path::Path::new(p);
    loop {
        if let Ok(c) = std::fs::canonicalize(cur) {
            let mut out = c;
            for part in rest.iter().rev() {
                out.push(part);
            }
            return out
                .to_str()
                .map(str::to_string)
                .unwrap_or_else(|| p.to_string());
        }
        match (cur.file_name().and_then(|n| n.to_str()), cur.parent()) {
            (Some(name), Some(parent)) => {
                rest.push(name);
                cur = parent;
            }
            _ => return p.to_string(),
        }
    }
}

fn realpath_existing(p: &str) -> Option<String> {
    std::fs::canonicalize(p)
        .ok()
        .and_then(|c| c.to_str().map(str::to_string))
}

/// The quote-stripped words the shell would pass, joined by spaces
/// (`Undetermined` when they cannot be determined statically).
pub fn static_words(command: &str) -> Determination<String> {
    crate::detect::ast::static_words(command)
}

/// Whether `command` is one simple command of static literal words (the
/// structural half of Allow-eligibility; see `detect::ast::static_simple`).
pub fn is_single_static_command(command: &str) -> bool {
    crate::detect::ast::is_single_static_command(command)
}

/// The evaluator's facts for the Bash command `command`; an undetermined
/// evaluation yields [`undetermined_facts`].
pub fn bash_facts(command: &str, cwd: Option<&str>) -> Facts {
    crate::detect::shape_facts(command, cwd).unwrap_or_else(undetermined_facts)
}
