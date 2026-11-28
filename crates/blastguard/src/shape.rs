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
//! [`SafeRoots`], and [`Target::GitRepo`] reads the `.git` entry of the
//! repository (real directory = main working tree; gitlink file = linked
//! worktree only when [`gitlink_kind`] vouches for it, else a main tree or
//! `Undetermined`; a symlinked `.git` is `Undetermined`); an unreadable one is
//! `Undetermined`.

use crate::scope::SafeRoots;
use harness_core::asklearn::{Shape, TargetClass, UNRESOLVED_VERB};
use harness_core::verdict::Determination;

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
        if strictly_inside_checkout(&norm, &root) {
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
            && repo_kind(&realpath(&c)) == RepoKind::Main
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

/// What kind of git working tree a directory lies in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RepoKind {
    /// `.git` is a directory: git-dir == common-dir (a MAIN working tree).
    Main,
    /// `.git` is a gitlink file pointing into `<common>/worktrees/<name>`.
    Linked,
    /// No `.git` found, or it could not be read: not determined.
    Unknown,
}

/// Walk up from `abs` to the nearest ancestor holding a `.git` entry. A
/// directory is a main working tree. A file is a gitlink, judged by
/// [`gitlink_kind`] (git's own `commondir` answer: a linked worktree; a
/// submodule's `.git/modules/..` is its own main tree). Any read error other
/// than "absent" is `Unknown`.
fn repo_kind(abs: &str) -> RepoKind {
    repo_kind_root(abs).0
}

/// [`repo_kind`] plus the checkout root (the directory holding `.git`) when
/// one was found. A path equal to that root is the checkout itself, not
/// something inside it.
fn repo_kind_root(abs: &str) -> (RepoKind, Option<std::path::PathBuf>) {
    let mut cur = std::path::Path::new(abs);
    loop {
        let dot_git = cur.join(".git");
        let root = Some(cur.to_path_buf());
        match std::fs::symlink_metadata(&dot_git) {
            Ok(m) if m.is_dir() => return (RepoKind::Main, root),
            Ok(m) if m.is_file() => return (gitlink_kind(cur, &dot_git), root),
            Ok(_) => return (RepoKind::Unknown, None),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return (RepoKind::Unknown, None),
        }
        match cur.parent() {
            Some(p) => cur = p,
            None => return (RepoKind::Unknown, None),
        }
    }
}

/// The kind of tree whose `.git` FILE (a gitlink) is `dot_git`, in `root`.
/// Decided as git decides, never by a substring of the gitlink text: the
/// `gitdir:` target must exist as a directory. A linked worktree's git dir
/// holds a `commondir` file that resolves to an existing git dir and a
/// `gitdir` back-link naming this very `.git` file; a git dir without
/// `commondir` (a submodule's `.git/modules/..`) is a main tree. A gitlink
/// that cannot be read or resolved, or whose git dir does not vouch for it, is
/// `Unknown` (restrictive).
fn gitlink_kind(root: &std::path::Path, dot_git: &std::path::Path) -> RepoKind {
    let Determination::Known(Some(text)) = harness_core::boundary::read_to_string(dot_git) else {
        return RepoKind::Unknown;
    };
    let Some(g) = text.trim().strip_prefix("gitdir:") else {
        return RepoKind::Unknown;
    };
    let g = std::path::Path::new(g.trim());
    let gitdir = match std::fs::canonicalize(root.join(g)) {
        Ok(p) if p.is_dir() => p,
        _ => return RepoKind::Unknown,
    };
    let common = match harness_core::boundary::read_to_string(&gitdir.join("commondir")) {
        Determination::Known(None) => return RepoKind::Main,
        Determination::Known(Some(c)) => c,
        _ => return RepoKind::Unknown,
    };
    match std::fs::canonicalize(gitdir.join(common.trim())) {
        Ok(p) if p.is_dir() => {}
        _ => return RepoKind::Unknown,
    }
    // The back-link: `<gitdir>/gitdir` names this checkout's `.git` file.
    let Determination::Known(Some(back)) =
        harness_core::boundary::read_to_string(&gitdir.join("gitdir"))
    else {
        return RepoKind::Unknown;
    };
    // git resolves a relative back-link (`--relative-paths`) against the git
    // dir, never against this process's cwd.
    let back = gitdir.join(back.trim());
    match (std::fs::canonicalize(back), std::fs::canonicalize(dot_git)) {
        (Ok(a), Ok(b)) if a == b => RepoKind::Linked,
        _ => RepoKind::Unknown,
    }
}

/// The checkout root of the linked worktree the session's cwd belongs to, or
/// `None` when the cwd is not inside a linked worktree (a main tree, no git
/// tree, an unreadable or unresolvable gitlink, no cwd).
fn cwd_linked_root(ctx: &Ctx<'_>) -> Option<std::path::PathBuf> {
    let c = crate::exclude::normalize(ctx.cwd?);
    if !c.starts_with('/') {
        return None;
    }
    match repo_kind_root(&realpath(&c)) {
        (RepoKind::Linked, Some(root)) => Some(root),
        _ => None,
    }
}

/// Whether `abs` lies STRICTLY inside the checkout `root`: below it, not the
/// root itself, and its own nearest `.git` is that checkout's (a submodule's
/// or another checkout nested below is a different tree).
fn strictly_inside_checkout(abs: &str, root: &std::path::Path) -> bool {
    let p = std::path::Path::new(abs.trim_end_matches('/'));
    p != root && p.starts_with(root) && repo_kind_root(abs).1.as_deref() == Some(root)
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
/// config keep their own class. A scratchpad repository is `Scratchpad` only
/// when `.git` is a real (non-symlink) directory whose config has no
/// `core.worktree` (or there is no repository); a symlinked `.git`, a
/// `core.worktree`, an `include`d config or an unreadable config is
/// `Undetermined`, and a gitlink file is judged by [`gitlink_kind`] like any
/// other tree. Only the linked worktree the session's cwd belongs to is
/// `Worktree` (a worktree storage path is not); the main working tree is
/// `MainTree` (never Allow-learnable, CLAUDE.md §8); anything not shown to be
/// one of those is `Undetermined`.
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
        // Round 11: the class is the tree git really uses, not the cwd's
        // spelling. Scratchpad only for no repository at all or a plain one
        // (`.git` a real directory, no `core.worktree`); a gitlink file falls
        // through to the git-dir judgement below, anything else is unknown.
        match scratch_repo_state(&real) {
            ScratchRepo::NoRepo | ScratchRepo::Plain => return TargetClass::Scratchpad,
            ScratchRepo::Gitlink => {}
            ScratchRepo::Unsafe => return TargetClass::Undetermined,
        }
    }
    // Round 9: the repository's kind is git's answer, never a substring of
    // the spelling (the `.harness-worktrees/` container is no repository).
    // Round 10: only the linked worktree the session's cwd belongs to is
    // `Worktree` (the repository IS that checkout, root included, unlike a path
    // target); another linked worktree is `Undetermined`.
    let placed = match repo_kind_root(&real) {
        (RepoKind::Linked, Some(root)) if cwd_linked_root(ctx).as_deref() == Some(&root) => {
            TargetClass::Worktree
        }
        (RepoKind::Linked, _) => TargetClass::Undetermined,
        (RepoKind::Main, _) => TargetClass::MainTree,
        (RepoKind::Unknown, _) => TargetClass::Undetermined,
    };
    // Only the Allow-eligible class needs the spelling to be the real path.
    if placed == TargetClass::Worktree && !real_spelling {
        return TargetClass::Undetermined;
    }
    placed
}

/// What git would use for a repository found at or above a scratchpad path.
enum ScratchRepo {
    /// No `.git` anywhere above (every probe was "absent").
    NoRepo,
    /// `.git` is a real directory whose config has no `core.worktree`.
    Plain,
    /// `.git` is a gitlink file (judged by [`gitlink_kind`]).
    Gitlink,
    /// A symlinked `.git`, a `core.worktree`, an `include`d config, or
    /// anything unreadable: not shown to be plain (restrictive).
    Unsafe,
}

/// Same upward search as [`repo_kind_root`], reporting how git's git dir and
/// work tree are determined for the nearest `.git`.
fn scratch_repo_state(abs: &str) -> ScratchRepo {
    let mut cur = std::path::Path::new(abs);
    loop {
        let dot_git = cur.join(".git");
        match std::fs::symlink_metadata(&dot_git) {
            Ok(m) if m.is_dir() => {
                return match git_config_redirects(&dot_git.join("config")) {
                    Some(false) => ScratchRepo::Plain,
                    _ => ScratchRepo::Unsafe,
                };
            }
            Ok(m) if m.is_file() => return ScratchRepo::Gitlink,
            Ok(_) => return ScratchRepo::Unsafe,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return ScratchRepo::Unsafe,
        }
        match cur.parent() {
            Some(p) => cur = p,
            None => return ScratchRepo::NoRepo,
        }
    }
}

/// Whether the git config at `path` redirects the work tree: `Some(true)` when
/// a `[core]` section sets `worktree` or any `[include*]` section could, and
/// `None` when it cannot be read (absent is `Some(false)`).
fn git_config_redirects(path: &std::path::Path) -> Option<bool> {
    let text = match harness_core::boundary::read_to_string(path) {
        Determination::Known(Some(t)) => t,
        Determination::Known(None) => return Some(false),
        _ => return None,
    };
    let mut section = String::new();
    for raw in text.lines() {
        let mut rest = raw.trim();
        if let Some(inner) = rest.strip_prefix('[') {
            let end = inner.find(']')?;
            let head = inner[..end].trim().to_ascii_lowercase();
            section = head.split_whitespace().next().unwrap_or("").to_string();
            if section.starts_with("include") {
                return Some(true);
            }
            rest = inner[end + 1..].trim();
        }
        if section == "core" {
            let key = rest
                .split(['=', ' ', '\t'])
                .next()
                .unwrap_or("")
                .to_ascii_lowercase();
            if key == "worktree" {
                return Some(true);
            }
        }
    }
    Some(false)
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
