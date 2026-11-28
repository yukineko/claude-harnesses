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
//! repository (directory = main working tree, gitlink file = linked
//! worktree); an unreadable one is `Undetermined`.

use crate::scope::{Placement, SafeRoots};
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
    Shape {
        rule_id: rule_id.to_string(),
        target_class: class_of(&targets, ctx),
        verbs,
    }
}

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
    if physical == lexical {
        lexical
    } else {
        TargetClass::Undetermined
    }
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
    if norm.contains("/.condukt/worktrees/") || norm.contains("/.harness-worktrees/") {
        return TargetClass::Worktree;
    }
    if matches!(
        ctx.scope.classify_worktree(&norm, ctx.cwd),
        Determination::Known(Placement::Inside { .. } | Placement::IsRoot { .. })
    ) {
        return TargetClass::Worktree;
    }
    // The session's own tree (the payload cwd), unless the cwd is `$HOME` or
    // one of its ancestors, which is not a worktree.
    if let Some(c) = ctx.cwd.map(crate::exclude::normalize) {
        let home = ctx.home.map(|h| h.trim_end_matches('/'));
        let cwd_is_home_or_above = c == "/"
            || home
                .is_some_and(|h| h == c || h.starts_with(&format!("{}/", c.trim_end_matches('/'))));
        if !cwd_is_home_or_above && (norm == c || norm.starts_with(&format!("{c}/"))) {
            return TargetClass::Worktree;
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
/// directory is a main working tree. A file is a gitlink: `.../worktrees/..`
/// is a linked worktree; anything else (a submodule's `.git/modules/..`) is its
/// own main tree. Any read error other than "absent" is `Unknown`.
fn repo_kind(abs: &str) -> RepoKind {
    let mut cur = std::path::Path::new(abs);
    loop {
        let dot_git = cur.join(".git");
        match std::fs::symlink_metadata(&dot_git) {
            Ok(m) if m.is_dir() => return RepoKind::Main,
            Ok(m) if m.is_file() => {
                return match harness_core::boundary::read_to_string(&dot_git) {
                    Determination::Known(Some(t)) => match t.trim().strip_prefix("gitdir:") {
                        Some(g) if g.contains("/worktrees/") => RepoKind::Linked,
                        Some(_) => RepoKind::Main,
                        None => RepoKind::Unknown,
                    },
                    _ => RepoKind::Unknown,
                };
            }
            Ok(_) => return RepoKind::Unknown,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return RepoKind::Unknown,
        }
        match cur.parent() {
            Some(p) => cur = p,
            None => return RepoKind::Unknown,
        }
    }
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
/// config and scratchpad locations keep their own class; a worktree storage
/// path or a linked worktree is `Worktree`; the main working tree is
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
    if lexical == TargetClass::Scratchpad && physical == TargetClass::Scratchpad {
        return TargetClass::Scratchpad;
    }
    if real.contains("/.condukt/worktrees/") || real.contains("/.harness-worktrees/") {
        return TargetClass::Worktree;
    }
    match repo_kind(&real) {
        RepoKind::Linked => TargetClass::Worktree,
        RepoKind::Main => TargetClass::MainTree,
        RepoKind::Unknown => TargetClass::Undetermined,
    }
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

/// The evaluator's facts for the Bash command `command`; an undetermined
/// evaluation yields [`undetermined_facts`].
pub fn bash_facts(command: &str, cwd: Option<&str>) -> Facts {
    crate::detect::shape_facts(command, cwd).unwrap_or_else(undetermined_facts)
}
