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
//! Pure: the filesystem enters only through the injected resolver already held
//! by [`SafeRoots`].

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
    Shape {
        rule_id: rule_id.to_string(),
        target_class: class_of(&targets, ctx),
        verbs,
    }
}

/// The class of a set of targets: the most dangerous one wins
/// (System > ProtectedConfig > Undetermined > Home > Worktree > Scratchpad >
/// None).
pub fn class_of(targets: &[Target], ctx: &Ctx<'_>) -> TargetClass {
    fn rank(c: TargetClass) -> u8 {
        match c {
            TargetClass::System => 6,
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

/// Class of one literal path.
pub fn class_of_path(raw: &str, ctx: &Ctx<'_>) -> TargetClass {
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

/// The evaluator's facts for the Bash command `command`; an undetermined
/// evaluation yields [`undetermined_facts`].
pub fn bash_facts(command: &str) -> Facts {
    crate::detect::shape_facts(command).unwrap_or_else(undetermined_facts)
}
