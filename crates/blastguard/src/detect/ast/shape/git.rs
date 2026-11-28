//! `git` invocations for the learning-shape facts (spec
//! `docs/specs/blastguard-ask-learning.md`, rulings of 2026-11-28 round 3/4):
//! how the global options are split from the subcommand, and which
//! repositories a git WRITE acts on.
//!
//! A git write whose repository cannot be established statically (a non-literal
//! cwd, `cd -`, `pushd`, an environment override that was not resolved, ...)
//! yields [`Target::Undetermined`]; the verb then carries `?` so the shape can
//! learn a Deny but never an Allow (CLAUDE.md §3).

use super::super::{CwdCandidates, SimpleCommand, Value, Word};
use super::{short_cluster, WRAPPERS};
use crate::shape::Target;
use std::collections::BTreeSet;

/// A `git` invocation split into its global options and the subcommand.
pub(super) struct GitCall<'a> {
    /// Normalised global flags (`-C`, `-c`, `--git-dir`, `-p`, ...); values are
    /// never part of the key.
    pub flags: BTreeSet<String>,
    /// Values of `-C <dir>`, in order.
    pub c_dirs: Vec<Value>,
    /// Values of `-c <key=value>`.
    pub config: Vec<Value>,
    pub git_dir: Vec<Value>,
    pub work_tree: Vec<Value>,
    /// Something before the subcommand could not be understood (an
    /// undetermined word, an attached `-C<path>`, `--namespace`, ...).
    pub unparsed: bool,
    /// From the subcommand on.
    pub rest: &'a [Word],
}

pub(super) fn parse_git(args: &[Word]) -> GitCall<'_> {
    let mut g = GitCall {
        flags: BTreeSet::new(),
        c_dirs: Vec::new(),
        config: Vec::new(),
        git_dir: Vec::new(),
        work_tree: Vec::new(),
        unparsed: false,
        rest: &[],
    };
    let mut i = 0;
    while i < args.len() {
        let Value::Literal(s) = &args[i].value else {
            g.unparsed = true;
            g.rest = &args[i + 1..];
            return g;
        };
        let next = |i: usize| args.get(i + 1).map(|w| w.value.clone());
        match s.as_str() {
            "-C" | "-c" => {
                g.flags.insert(s.clone());
                match next(i) {
                    Some(v) if s == "-C" => g.c_dirs.push(v),
                    Some(v) => g.config.push(v),
                    None => g.unparsed = true,
                }
                i += 2;
            }
            "--git-dir" | "--work-tree" => {
                g.flags.insert(s.clone());
                match next(i) {
                    Some(v) if s == "--git-dir" => g.git_dir.push(v),
                    Some(v) => g.work_tree.push(v),
                    None => g.unparsed = true,
                }
                i += 2;
            }
            "--namespace" | "--super-prefix" | "--config-env" | "--attr-source" => {
                g.flags.insert(s.clone());
                g.unparsed = true;
                i += 2;
            }
            o if o.starts_with("--git-dir=") => {
                g.flags.insert("--git-dir".into());
                g.git_dir
                    .push(Value::Literal(o["--git-dir=".len()..].to_string()));
                i += 1;
            }
            o if o.starts_with("--work-tree=") => {
                g.flags.insert("--work-tree".into());
                g.work_tree
                    .push(Value::Literal(o["--work-tree=".len()..].to_string()));
                i += 1;
            }
            o if o.starts_with("--namespace=")
                || o.starts_with("--super-prefix=")
                || o.starts_with("--config-env=") =>
            {
                g.flags.insert(o.split('=').next().unwrap_or(o).to_string());
                g.unparsed = true;
                i += 1;
            }
            // `-Cpath` / `-ckey=value` are not git syntax (git rejects them);
            // the value cannot be told apart from a flag cluster, so the
            // shape is never learnable as Allow.
            o if (o.starts_with("-C") || o.starts_with("-c")) && o.len() > 2 => {
                g.flags.insert(o[..2].to_string());
                g.unparsed = true;
                i += 1;
            }
            o if o.starts_with("--") => {
                g.flags.insert(o.split('=').next().unwrap_or(o).to_string());
                i += 1;
            }
            o if o.starts_with('-') && o.len() > 1 => {
                short_cluster(&o[1..], &mut g.flags);
                i += 1;
            }
            _ => break,
        }
    }
    g.rest = args.get(i..).unwrap_or(&[]);
    g
}

/// The index of the `git` command word of `cmd`: argv[0] itself, or the first
/// `git` operand of a wrapper (`env git ..`, `timeout 10 git ..`). An
/// over-approximation (`env echo git`) only makes a shape stricter.
///
/// An unresolved command word is not a git index: its verb is `?` (never
/// Allow-learnable) and [`super::unknown_writer`] poisons the line's pins.
pub(super) fn git_index(cmd: &SimpleCommand) -> Option<usize> {
    let base = |w: &Word| match &w.value {
        Value::Literal(s) => Ok(s.rsplit('/').next().unwrap_or(s).to_string()),
        Value::Undetermined(_) => Err(Unresolved),
    };
    let first = match base(cmd.argv.first()?) {
        Ok(f) => f,
        Err(Unresolved) => return None,
    };
    if first == "git" {
        return Some(0);
    }
    if !WRAPPERS.contains(&first.as_str()) {
        return None;
    }
    cmd.argv
        .iter()
        .enumerate()
        .skip(1)
        .find(|(_, w)| matches!(base(w), Ok(b) if b == "git"))
        .map(|(i, _)| i)
}

/// A word or directory the evaluator could not determine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Unresolved;

/// Read-only git subcommands: no write target.
const GIT_READONLY: &[&str] = &[
    "status",
    "log",
    "diff",
    "show",
    "rev-parse",
    "rev-list",
    "ls-files",
    "ls-tree",
    "cat-file",
    "blame",
    "describe",
    "grep",
    "shortlog",
    "show-ref",
    "for-each-ref",
    "name-rev",
    "merge-base",
    "diff-tree",
    "diff-index",
    "ls-remote",
    "version",
    "help",
    "whatchanged",
    "count-objects",
    "check-ignore",
    "var",
];

/// `git <sub> <rest..>` that only reads. Anything not shown to be read-only
/// (including an operand the evaluator could not resolve) is a write.
pub(super) fn git_readonly(sub: &str, rest: &[Word]) -> bool {
    if GIT_READONLY.contains(&sub) {
        return true;
    }
    let lits: Vec<&str> = rest
        .iter()
        .map(|w| match &w.value {
            Value::Literal(s) => s.as_str(),
            Value::Undetermined(_) => "\0",
        })
        .collect();
    let flags: Vec<&str> = lits
        .iter()
        .copied()
        .filter(|s| s.starts_with('-'))
        .collect();
    let pos: Vec<&str> = lits
        .iter()
        .copied()
        .filter(|s| !s.starts_with('-'))
        .collect();
    let flags_within = |ok: &[&str]| flags.iter().all(|f| ok.contains(f));
    match sub {
        "branch" => {
            pos.is_empty()
                && flags_within(&[
                    "--show-current",
                    "-a",
                    "-r",
                    "-v",
                    "-vv",
                    "--list",
                    "-l",
                    "--all",
                    "--remotes",
                    "--verbose",
                ])
        }
        "tag" => pos.is_empty() && flags_within(&["-l", "--list", "-n"]),
        "stash" | "worktree" | "submodule" | "notes" | "reflog" => {
            matches!(pos.first().copied(), Some("list" | "show" | "status"))
        }
        "remote" => {
            matches!(pos.first().copied(), Some("show" | "get-url"))
                || (pos.is_empty() && flags_within(&["-v", "--verbose"]))
        }
        "config" => flags
            .iter()
            .any(|f| ["--get", "--get-all", "--get-regexp", "--list", "-l"].contains(f)),
        _ => false,
    }
}

/// `GIT_*` variables the shell state carries from earlier commands of the
/// line (`export GIT_DIR=..`, `GIT_DIR=..;`).
#[derive(Debug, Default)]
pub(super) struct ShellGit {
    vars: Vec<(String, Value)>,
    /// Something that may have set any variable ran (`eval`, `source`, an
    /// undetermined `export` operand): the git environment is unknown.
    taint: bool,
}

const DECLARERS: &[&str] = &["export", "declare", "typeset", "readonly", "local"];

impl ShellGit {
    fn set(&mut self, k: &str, v: Value) {
        self.vars.retain(|(n, _)| n != k);
        self.vars.push((k.to_string(), v));
    }

    /// Record the effect of `cmd` on the git environment of later commands.
    pub(super) fn observe(&mut self, cmd: &SimpleCommand) {
        let Some(first) = cmd.argv.first() else {
            for (k, w) in &cmd.assignments {
                if k.starts_with("GIT_") {
                    self.set(k, w.value.clone());
                }
            }
            return;
        };
        let Value::Literal(name) = &first.value else {
            self.taint = true;
            return;
        };
        let base = name.rsplit('/').next().unwrap_or(name);
        if DECLARERS.contains(&base) {
            for a in &cmd.argv[1..] {
                match &a.value {
                    Value::Literal(s) => {
                        if let Some((k, v)) = s.split_once('=') {
                            if k.starts_with("GIT_") {
                                self.set(k, Value::Literal(v.to_string()));
                            }
                        }
                    }
                    Value::Undetermined(_) => self.taint = true,
                }
            }
        } else if base == "unset" {
            for a in &cmd.argv[1..] {
                match &a.value {
                    Value::Literal(s) => self.vars.retain(|(n, _)| n != s),
                    Value::Undetermined(_) => self.taint = true,
                }
            }
        } else if matches!(base, "eval" | "source" | ".")
            && !super::super::is_rustup_env_source(cmd)
        {
            self.taint = true;
        }
    }
}

/// `base` joined with the directory operand `d` (`.` = the payload cwd).
fn join(base: &str, d: &str) -> String {
    if d.starts_with('/') || d.starts_with('~') || base == "." {
        d.to_string()
    } else {
        format!("{}/{d}", base.trim_end_matches('/'))
    }
}

/// The directories the git process may start in (`.` = the payload cwd), or
/// [`Unresolved`] when they are not known.
fn bases(cmd: &SimpleCommand) -> Result<Vec<String>, Unresolved> {
    if let CwdCandidates::OneOf(list) = &cmd.cwd_candidates {
        return Ok(list
            .iter()
            .map(|c| c.clone().unwrap_or_else(|| ".".to_string()))
            .collect());
    }
    match &cmd.cwd {
        Value::Literal(c) => Ok(vec![c.clone()]),
        Value::Undetermined(r) if r == super::super::eval::HOOK_CWD => Ok(vec![".".to_string()]),
        Value::Undetermined(_) => Err(Unresolved),
    }
}

/// The repository (working tree) a `GIT_DIR` / `--git-dir` names: the parent of
/// a literal `.../.git`; anything else is not resolved.
fn repo_of_git_dir(base: &str, v: &Value) -> Option<String> {
    let Value::Literal(g) = v else { return None };
    let abs = join(base, g);
    let parent = abs.strip_suffix("/.git")?;
    Some(if parent.is_empty() {
        "/".into()
    } else {
        parent.to_string()
    })
}

/// The repositories a git WRITE acts on, as [`Target::GitRepo`]s (spec rule 5
/// and the round-4 rulings): the effective cwd (after a literal `cd`, or
/// `-C`), the tree a `GIT_WORK_TREE` / `--work-tree` / `-c core.worktree=`
/// names, the repository a `GIT_DIR` / `--git-dir` names, or, for `git
/// worktree remove <path>`, the path it deletes. Read-only subcommands have no
/// target; whatever cannot be resolved is [`Target::Undetermined`].
pub(super) fn targets(cmd: &SimpleCommand, git_idx: usize, shell: &ShellGit) -> Vec<Target> {
    let undet = || vec![Target::Undetermined];
    let g = parse_git(&cmd.argv[git_idx + 1..]);
    let Some(sub_w) = g.rest.first() else {
        // `git` alone prints usage; an unparsed prefix may hide the subcommand.
        return if g.unparsed { undet() } else { Vec::new() };
    };
    let Value::Literal(sub) = &sub_w.value else {
        return undet();
    };
    let rest = &g.rest[1..];
    if git_readonly(sub, rest) {
        return Vec::new();
    }
    if g.unparsed || shell.taint {
        return undet();
    }
    // Environment: the shell's, the command's prefix assignments and the
    // `NAME=value` operands of a wrapper (`env GIT_DIR=.. git ..`).
    let mut vars: Vec<(String, Value)> = shell.vars.clone();
    for (k, w) in &cmd.assignments {
        vars.push((k.clone(), w.value.clone()));
    }
    for w in cmd.argv.get(1..git_idx).unwrap_or(&[]) {
        if let Value::Literal(s) = &w.value {
            if let Some((k, v)) = s.split_once('=') {
                vars.push((k.to_string(), Value::Literal(v.to_string())));
            }
        }
    }
    let mut env_git_dir: Option<Value> = None;
    let mut env_work_tree: Option<Value> = None;
    for (k, v) in &vars {
        match k.as_str() {
            "GIT_DIR" => env_git_dir = Some(v.clone()),
            "GIT_WORK_TREE" => env_work_tree = Some(v.clone()),
            k if k.starts_with("GIT_AUTHOR_")
                || k.starts_with("GIT_COMMITTER_")
                || k == "GIT_TERMINAL_PROMPT" => {}
            k if k.starts_with("GIT_") => return undet(),
            _ => {}
        }
    }
    let Ok(bases) = bases(cmd) else {
        return undet();
    };
    let mut out = Vec::new();
    for base in bases {
        let mut dir = base;
        for c in &g.c_dirs {
            match c {
                Value::Literal(d) => dir = join(&dir, d),
                Value::Undetermined(_) => return undet(),
            }
        }
        // `worktree remove <path>` deletes that tree.
        if sub == "worktree"
            && matches!(rest.first().map(|w| &w.value), Some(Value::Literal(s)) if s == "remove")
        {
            match rest[1..]
                .iter()
                .find(|w| !matches!(&w.value, Value::Literal(s) if s.starts_with('-')))
                .map(|w| &w.value)
            {
                Some(Value::Literal(p)) => out.push(Target::GitRepo(join(&dir, p))),
                _ => return undet(),
            }
            continue;
        }
        // The destination of a new repository is an operand, not the cwd.
        if matches!(sub.as_str(), "clone" | "init") {
            return undet();
        }
        out.push(Target::GitRepo(dir.clone()));
        for v in g.git_dir.iter().chain(env_git_dir.iter()) {
            match repo_of_git_dir(&dir, v) {
                Some(r) => out.push(Target::GitRepo(r)),
                None => return undet(),
            }
        }
        for v in g.work_tree.iter().chain(env_work_tree.iter()) {
            match v {
                Value::Literal(w) if !w.is_empty() => out.push(Target::GitRepo(join(&dir, w))),
                _ => return undet(),
            }
        }
        for v in &g.config {
            let Value::Literal(kv) = v else {
                return undet();
            };
            let (k, val) = kv.split_once('=').unwrap_or((kv, ""));
            if k.eq_ignore_ascii_case("core.worktree") {
                // Relative to the git dir, which is not resolved here.
                if val.starts_with('/') || val.starts_with('~') {
                    out.push(Target::GitRepo(val.to_string()));
                } else {
                    return undet();
                }
            }
        }
    }
    out
}
