//! The learning-shape facts of a Bash command line (spec
//! `docs/specs/blastguard-ask-learning.md`), read from the SAME evaluated
//! [`super::Script`] the detectors judge: the command words of every simple
//! command, and the write targets the evaluator resolved (file redirects and
//! the Table B B.1 target rows). Nothing here parses the command text.
//!
//! An unresolved value is [`Target::Undetermined`] / the unresolved verb —
//! never dropped, never read as "no target" (CLAUDE.md §3).

use super::eval::resolve_at;
use super::judge::table_b::{extract, Row};
use super::{
    analyze, program_source, Analysis, ProgramSource, RedirOp, RedirTarget, SimpleCommand,
    StdinSource, Value,
};
use crate::shape::{Facts, Target};
use harness_core::asklearn::UNRESOLVED_VERB;
use std::collections::{BTreeSet, HashMap};

fn target_of(v: Value) -> Target {
    match v {
        Value::Literal(s) if !s.is_empty() => Target::Path(s),
        Value::Literal(_) | Value::Undetermined(_) => Target::Undetermined,
    }
}

/// The facts of `command`, or `None` when its evaluation is `Undetermined`
/// (the caller then uses [`crate::shape::undetermined_facts`]).
pub(crate) fn shape_facts(command: &str) -> Option<Facts> {
    let Analysis::Known(script) = analyze(command) else {
        return None;
    };
    let mut facts = Facts::default();
    // Paths written earlier in THIS command line: `Some(body)` when a literal
    // heredoc was `cat`-ed into the path (the file's whole content is then
    // known), `None` when something else wrote it (content not pinned).
    let mut written: Written = HashMap::new();
    for cmd in &script.commands {
        let before = facts.targets.len();
        if let Some(first) = cmd.argv.first() {
            facts.verbs.push(match &first.value {
                Value::Literal(name) if !name.is_empty() => {
                    verb_token(cmd, name.rsplit('/').next().unwrap_or(name), &written)
                }
                _ => UNRESOLVED_VERB.to_string(),
            });
        }
        let heredoc_body = match (&cmd.stdin, cmd.argv.as_slice()) {
            (StdinSource::Literal(b), [cat]) if matches!(&cat.value, Value::Literal(n) if n.rsplit('/').next() == Some("cat")) => {
                Some(b.clone())
            }
            _ => None,
        };
        for r in &cmd.redirects {
            if matches!(
                r.op,
                RedirOp::Out
                    | RedirOp::Append
                    | RedirOp::Clobber
                    | RedirOp::InOut
                    | RedirOp::OutErr
                    | RedirOp::AppendOutErr
            ) {
                if let RedirTarget::Path(v) = &r.target {
                    if let Value::Literal(p) = v {
                        let whole = matches!(r.op, RedirOp::Out | RedirOp::Clobber);
                        written.insert(p.clone(), whole.then(|| heredoc_body.clone()).flatten());
                    }
                    facts.targets.push(target_of(v.clone()));
                }
            }
        }
        match extract(cmd) {
            Row::None => {}
            Row::Undetermined(_) => facts.targets.push(Target::Undetermined),
            Row::Targets(pairs) => {
                for (_, v) in pairs {
                    let v = resolve_at(&cmd.cwd, v);
                    if let Value::Literal(p) = &v {
                        written.insert(p.clone(), None);
                    }
                    facts.targets.push(target_of(v));
                }
            }
        }
        facts.targets.extend(git_targets(cmd));
        // A shell / interpreter / wrapper runs a program whose own targets
        // this evaluation does not list.
        if program_source(cmd).is_some() {
            facts.targets.push(Target::Undetermined);
        }
        // A write to an undetermined place may have rewritten any file: no
        // earlier pin and no on-disk read can be trusted for a later program.
        if facts.targets[before..]
            .iter()
            .any(|t| matches!(t, Target::Undetermined))
        {
            written.clear();
            written.insert(POISON.to_string(), None);
        }
    }
    Some(facts)
}

/// Marker key of [`Written`]: an undetermined write happened.
const POISON: &str = "\0undetermined-write";

/// Paths written earlier on the command line (see [`shape_facts`]).
type Written = HashMap<String, Option<String>>;

fn body_hash(bytes: &[u8]) -> String {
    format!("#{:016x}", harness_core::hash::fnv1a64(bytes))
}

/// Largest script file whose content is hashed into a verb.
const MAX_SCRIPT_BYTES: u64 = 1 << 20;

/// The `#<hash>` token of a program FILE (user ruling 2026-11-28, R2 for
/// program files): the hash of the heredoc this command line wrote into it,
/// else of its current on-disk content. `?` (never Allow-learnable) when the
/// path is unresolved or relative to an unknown cwd, spelled with `.` / `..`,
/// written by something whose content is not pinned, an undetermined write
/// came before, or the file cannot be read (missing, too large, unreadable).
fn file_program_token(path: &Value, written: &Written) -> String {
    let unpinned = || UNRESOLVED_VERB.to_string();
    let Value::Literal(p) = path else {
        return unpinned();
    };
    if p.split('/').any(|c| c == ".." || c == ".") {
        return unpinned();
    }
    if let Some(entry) = written.get(p) {
        return match entry {
            Some(body) => body_hash(body.as_bytes()),
            None => unpinned(),
        };
    }
    if written.contains_key(POISON) || !p.starts_with('/') {
        return unpinned();
    }
    match std::fs::metadata(p) {
        Ok(m) if m.is_file() && m.len() <= MAX_SCRIPT_BYTES => match std::fs::read(p) {
            Ok(bytes) => body_hash(&bytes),
            Err(_) => unpinned(),
        },
        _ => unpinned(),
    }
}

/// Multi-command tools: the first non-option operand names what is run
/// (`git push`, `cargo test`, `docker rm`). Without it `git status` and
/// `git push --force` would share a shape (user ruling R1, 2026-11-28).
const SUBCOMMAND_TOOLS: &[&str] = &[
    "git",
    "cargo",
    "gh",
    "docker",
    "podman",
    "kubectl",
    "helm",
    "npm",
    "npx",
    "pnpm",
    "yarn",
    "bun",
    "deno",
    "pip",
    "pip3",
    "uv",
    "brew",
    "systemctl",
    "launchctl",
    "terraform",
    "go",
    "rustup",
    "apt",
    "apt-get",
    "dnf",
    "aws",
    "gcloud",
    "az",
    "condukt",
    "overwatch",
    "backlog",
];

/// Tools whose SECOND operand is part of the verb too (`gh pr merge`,
/// `docker compose down`, `npm run x`): the first operand alone is a group.
const TWO_LEVEL_TOOLS: &[&str] = &[
    "gh",
    "docker",
    "podman",
    "kubectl",
    "helm",
    "npm",
    "pnpm",
    "yarn",
    "bun",
    "aws",
    "gcloud",
    "az",
    "terraform",
    "launchctl",
    "condukt",
    "overwatch",
    "backlog",
];

/// `git` subcommands that are themselves groups (`git stash drop`).
const GIT_GROUPS: &[&str] = &[
    "remote",
    "stash",
    "worktree",
    "submodule",
    "notes",
    "reflog",
    "bisect",
    "sparse-checkout",
    "config",
];

fn short_cluster(s: &str, flags: &mut BTreeSet<String>) {
    for c in s.chars().take_while(|c| *c != '=') {
        flags.insert(format!("-{c}"));
    }
}

/// The verb of one simple command (spec R1/R2, user rulings 2026-11-28):
///
/// `basename [subcommand..] [option..] [#body-hash]`, space separated.
///
/// * every OPTION token is part of the verb, normalised: a short cluster is
///   split (`-rf` -> `-f -r`), a long option loses its `=value`, the set is
///   sorted. A finer key is always safe; a coarser one would let an approval
///   of `git branch -d` cover `git branch -D`. Operands that are not a
///   subcommand (paths, refs) are NOT in the verb, so `rm a` and `rm b` still
///   share a shape. An operand the evaluator could not resolve adds nothing
///   (`rm "$f"` is `rm`): that is the pre-existing, pinned shape of such a
///   call.
/// * an inline interpreter program (`-c` / `-e` body, a literal heredoc or
///   here-string on stdin) adds `#<fnv1a64 of the body>`; a program the
///   evaluator cannot pin (wrapper, `eval`, piped from a command) adds the
///   unresolved token `?`, which makes the shape never learnable as Allow.
/// * a program FILE (`python3 a.py`) adds the hash of its content (see
///   [`file_program_token`]; heredoc written earlier on the line wins), a
///   wrapper keeps the wrapped tool's subcommand, and `xargs` / `$(..)`
///   operands add `?` (schema `s3-`).
fn verb_token(cmd: &SimpleCommand, base: &str, written: &Written) -> String {
    let all = parse_args(cmd.argv.get(1..).unwrap_or(&[]));
    let mut parts: Vec<String> = vec![base.to_string()];
    if WRAPPERS.contains(&base) {
        // A wrapper runs the command named in its operands: keep THAT
        // command's tool and subcommand (`env git branch -D` must not become
        // `env -D`). Its flags are already in `all.flags` (every option of the
        // line is in the verb).
        let args = cmd.argv.get(1..).unwrap_or(&[]);
        let inner = args.iter().position(|w| {
            matches!(&w.value, Value::Literal(s) if !s.starts_with('-')
                && SUBCOMMAND_TOOLS.contains(&s.rsplit('/').next().unwrap_or(s)))
        });
        if let Some(j) = inner {
            if let Value::Literal(s) = &args[j].value {
                let inner_base = s.rsplit('/').next().unwrap_or(s);
                parts.push(inner_base.to_string());
                parts.extend(subcommand_parts(inner_base, &parse_args(&args[j + 1..])));
            }
        }
    } else {
        parts.extend(subcommand_parts(base, &all));
    }
    parts.extend(all.flags);
    match program_source(cmd) {
        Some(ProgramSource::InlineLiteral(body))
        | Some(ProgramSource::Stdin(StdinSource::Literal(body))) => {
            parts.push(body_hash(body.as_bytes()));
        }
        Some(ProgramSource::File(f)) | Some(ProgramSource::Stdin(StdinSource::File(f))) => {
            parts.push(file_program_token(&f, written));
        }
        None => {}
        Some(_) => parts.push(UNRESOLVED_VERB.to_string()),
    }
    // Operands the key cannot pin: `xargs` reads its command's arguments (and
    // flags) from stdin, and a `$(..)` / backquote substitution supplies words
    // at run time. Neither is in the key, so the shape must never be Allow.
    if base == "xargs" || all.substituted {
        parts.push(UNRESOLVED_VERB.to_string());
    }
    parts.join(" ")
}

/// Commands that run another command named in their operands (the same list
/// `program_source` treats as wrappers).
const WRAPPERS: &[&str] = &[
    "env",
    "sudo",
    "doas",
    "su",
    "xargs",
    "exec",
    "command",
    "builtin",
    "nohup",
    "nice",
    "ionice",
    "timeout",
    "time",
    "stdbuf",
    "chrt",
    "taskset",
    "setsid",
    "caffeinate",
    "arch",
    "unbuffer",
    "flock",
];

struct ParsedArgs<'a> {
    flags: BTreeSet<String>,
    positionals: Vec<&'a str>,
    option_before_first_operand: bool,
    /// An operand the evaluator could not resolve was a `$(..)` / backquote
    /// substitution (a plain `$var` is not: it is the pinned `rm "$f"` shape).
    substituted: bool,
}

fn parse_args(args: &[super::Word]) -> ParsedArgs<'_> {
    let mut p = ParsedArgs {
        flags: BTreeSet::new(),
        positionals: Vec::new(),
        option_before_first_operand: false,
        substituted: false,
    };
    let mut options_ended = false;
    for a in args {
        let Value::Literal(s) = &a.value else {
            p.substituted |= a.raw.contains("$(") || a.raw.contains('`');
            continue;
        };
        if !options_ended && s == "--" {
            options_ended = true;
        } else if !options_ended && s.starts_with("--") {
            p.flags.insert(s.split('=').next().unwrap_or(s).to_string());
            p.option_before_first_operand |= p.positionals.is_empty();
        } else if !options_ended && s.starts_with('-') && s.len() > 1 {
            short_cluster(&s[1..], &mut p.flags);
            p.option_before_first_operand |= p.positionals.is_empty();
        } else {
            p.positionals.push(s.as_str());
        }
    }
    p
}

/// The subcommand operands of a multi-command tool (empty for any other).
fn subcommand_parts(base: &str, p: &ParsedArgs<'_>) -> Vec<String> {
    if !SUBCOMMAND_TOOLS.contains(&base) {
        return Vec::new();
    }
    let first = p.positionals.first().copied().unwrap_or("");
    let mut depth = 1;
    // A global option before the subcommand (`git -C dir push`) may have
    // consumed an operand: take one more so the subcommand is included.
    if p.option_before_first_operand || first.starts_with('+') {
        depth += 1;
    }
    if TWO_LEVEL_TOOLS.contains(&base) || (base == "git" && GIT_GROUPS.contains(&first)) {
        depth += 1;
    }
    p.positionals
        .iter()
        .take(depth)
        .map(|s| s.to_string())
        .collect()
}

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
fn git_readonly(sub: &str, rest: &[super::Word]) -> bool {
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

/// The repository a git WRITE acts on, as a [`Target::GitRepo`] (spec rule 5):
/// the effective cwd (after a literal `cd`, or `-C`), or, for `git worktree
/// remove <path>`, the path it deletes. Read-only subcommands have no target;
/// a write whose repository cannot be determined is [`Target::Undetermined`].
fn git_targets(cmd: &SimpleCommand) -> Vec<Target> {
    let undet = || vec![Target::Undetermined];
    let Some(first) = cmd.argv.first() else {
        return Vec::new();
    };
    match &first.value {
        Value::Literal(n) if n.rsplit('/').next() == Some("git") => {}
        _ => return Vec::new(),
    }
    if cmd.assignments.iter().any(|(k, _)| k.starts_with("GIT_")) {
        return undet();
    }
    // The directory the git process starts in.
    let mut base = match &cmd.cwd {
        Value::Literal(c) => c.clone(),
        Value::Undetermined(r) if r == super::eval::HOOK_CWD => ".".to_string(),
        Value::Undetermined(_) => return undet(),
    };
    let join = |base: &str, d: &str| -> String {
        // An absolute / `~` path, or a relative one under the hook's cwd
        // (`.`, joined with the payload cwd when classified), stands alone.
        if d.starts_with('/') || d.starts_with('~') || base == "." {
            d.to_string()
        } else {
            format!("{}/{d}", base.trim_end_matches('/'))
        }
    };
    let mut i = 1;
    while let Some(w) = cmd.argv.get(i) {
        let Value::Literal(s) = &w.value else {
            return undet();
        };
        match s.as_str() {
            "-C" => match cmd.argv.get(i + 1).map(|w| &w.value) {
                Some(Value::Literal(d)) => {
                    base = join(&base, d);
                    i += 2;
                }
                _ => return undet(),
            },
            "-c" => i += 2,
            o if o.starts_with("--git-dir")
                || o.starts_with("--work-tree")
                || o.starts_with("--namespace")
                || o.starts_with("--super-prefix") =>
            {
                return undet()
            }
            o if o.starts_with('-') => i += 1,
            _ => break,
        }
    }
    let Some(Value::Literal(sub)) = cmd.argv.get(i).map(|w| &w.value) else {
        // `git` alone prints usage; a subcommand that is not a literal is a
        // write to an undetermined place.
        return if cmd.argv.get(i).is_some() {
            undet()
        } else {
            Vec::new()
        };
    };
    let rest = cmd.argv.get(i + 1..).unwrap_or(&[]);
    if git_readonly(sub, rest) {
        return Vec::new();
    }
    match sub.as_str() {
        // The destination of a new repository is an operand, not the cwd.
        "clone" | "init" => undet(),
        "worktree" if matches!(rest.first().map(|w| &w.value), Some(Value::Literal(s)) if s == "remove") => {
            match rest[1..]
                .iter()
                .find(|w| !matches!(&w.value, Value::Literal(s) if s.starts_with('-')))
                .map(|w| &w.value)
            {
                Some(Value::Literal(p)) => vec![Target::GitRepo(join(&base, p))],
                _ => undet(),
            }
        }
        _ => vec![Target::GitRepo(base)],
    }
}
