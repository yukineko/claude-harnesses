//! The learning-shape facts of a Bash command line (spec
//! `docs/specs/blastguard-ask-learning.md`), read from the SAME evaluated
//! [`super::Script`] the detectors judge: the command words of every simple
//! command, and the write targets the evaluator resolved (file redirects and
//! the Table B B.1 target rows). Nothing here parses the command text.
//!
//! An unresolved value is [`Target::Undetermined`] / the unresolved verb —
//! never dropped, never read as "no target" (CLAUDE.md §3).

mod git;
mod interp;

use super::eval::resolve_at;
use super::judge::table_b::{extract, Row};
use super::{
    analyze, program_source, Analysis, Context, ProgramSource, RedirOp, RedirTarget, SimpleCommand,
    StdinSource, Value,
};
use crate::shape::{Facts, Target};
use git::{git_index, git_readonly, parse_git, ShellGit};
use harness_core::asklearn::UNRESOLVED_VERB;
use std::collections::{BTreeSet, HashMap};

fn target_of(v: Value) -> Target {
    match v {
        Value::Literal(s) if !s.is_empty() => Target::Path(s),
        Value::Literal(_) | Value::Undetermined(_) => Target::Undetermined,
    }
}

/// The facts of `command` run in the hook cwd `cwd`, or `None` when its
/// evaluation is `Undetermined` (the caller then uses
/// [`crate::shape::undetermined_facts`]). `cwd` lets the evaluator resolve a
/// relative `cd` (`cd ..`), so a git write's repository can be named.
pub(crate) fn shape_facts(command: &str, cwd: Option<&str>) -> Option<Facts> {
    super::with_hook_dir(cwd, || shape_facts_in(command))
}

fn shape_facts_in(command: &str) -> Option<Facts> {
    let Analysis::Known(script) = analyze(command) else {
        return None;
    };
    let mut facts = Facts::default();
    // Paths written earlier in THIS command line: `Some(body)` when a literal
    // heredoc was `cat`-ed into the path (the file's whole content is then
    // known), `None` when something else wrote it (content not pinned).
    let mut written: Written = HashMap::new();
    // A command that runs concurrently (`&`, a pipeline) with an unknown
    // writer may be rewritten under the program it starts: no pin holds.
    let concurrent = script.commands.iter().any(|c| {
        matches!(c.context, Context::Background)
            || matches!(c.context, Context::Pipeline { len, .. } if len > 1)
    });
    if concurrent && script.commands.iter().any(unknown_writer) {
        written.insert(CONCURRENT.to_string(), None);
    }
    let mut shell = ShellGit::default();
    for cmd in &script.commands {
        let before = facts.targets.len();
        let git_targets = git_index(cmd)
            .map(|i| git::targets(cmd, i, &shell))
            .unwrap_or_default();
        let git_undetermined = git_targets
            .iter()
            .any(|t| matches!(t, Target::Undetermined));
        if let Some(first) = cmd.argv.first() {
            facts.verbs.push(match &first.value {
                Value::Literal(name) if !name.is_empty() => verb_token(
                    cmd,
                    name.rsplit('/').next().unwrap_or(name),
                    &written,
                    git_undetermined,
                ),
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
                        written.insert(
                            key_of(p, &cmd.cwd),
                            whole.then(|| heredoc_body.clone()).flatten(),
                        );
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
                        written.insert(key_of(p, &cmd.cwd), None);
                    }
                    facts.targets.push(target_of(v));
                }
            }
        }
        facts.targets.extend(git_targets);
        shell.observe(cmd);
        // A shell / interpreter / wrapper runs a program whose own targets
        // this evaluation does not list.
        if program_source(cmd).is_some() {
            facts.targets.push(Target::Undetermined);
        }
        // A write to an undetermined place may have rewritten any file, and so
        // may a command whose writes are not modelled (`rsync`, `curl -o`,
        // `patch`, a git write, an interpreter run, ...): no earlier pin and
        // no on-disk read can be trusted for a later program.
        if facts.targets[before..]
            .iter()
            .any(|t| matches!(t, Target::Undetermined))
            || unknown_writer(cmd)
        {
            let concurrent = written.contains_key(CONCURRENT);
            written.clear();
            written.insert(POISON.to_string(), None);
            if concurrent {
                written.insert(CONCURRENT.to_string(), None);
            }
        }
    }
    Some(facts)
}

/// Marker key of [`Written`]: an undetermined or unmodelled write happened.
const POISON: &str = "\0undetermined-write";

/// Marker key of [`Written`]: an unmodelled write runs concurrently with a
/// later command (`&`, a pipeline stage): nothing is pinned, ever.
const CONCURRENT: &str = "\0concurrent-write";

/// Paths written earlier on the command line (see [`shape_facts`]).
type Written = HashMap<String, Option<String>>;

/// Commands that write no file by themselves (their redirects are tracked
/// separately) and run no other program.
const BENIGN: &[&str] = &[
    "cd", "pushd", "popd", "echo", "printf", "true", "false", ":", "test", "[", "[[", "pwd", "ls",
    "cat", "head", "tail", "grep", "egrep", "fgrep", "rg", "wc", "sort", "uniq", "diff", "cmp",
    "stat", "file", "which", "type", "export", "set", "unset", "readonly", "local", "declare",
    "typeset", "read", "sleep", "date", "whoami", "id", "uname", "hostname", "basename", "dirname",
    "realpath", "readlink", "tr", "cut", "jq", "wait", "exit", "return", "chmod", "chown", "touch",
    "mkdir",
];

/// `cmd` may write somewhere the evaluation does not list: anything that is
/// not known to be a pure reader and is not a Table B file command whose
/// targets are extracted (`rsync`, `curl -o`, `wget -O`, `patch`, a git write
/// such as `checkout -- <path>`, an interpreter run, an undetermined command
/// word, ...). CLAUDE.md §3: not shown to be harmless is not harmless.
fn unknown_writer(cmd: &SimpleCommand) -> bool {
    let Some(first) = cmd.argv.first() else {
        return false;
    };
    let Value::Literal(name) = &first.value else {
        return true;
    };
    let base = name.rsplit('/').next().unwrap_or(name);
    if base == "git" || git_index(cmd).is_some() {
        let i = git_index(cmd).unwrap_or(0);
        let g = parse_git(&cmd.argv[i + 1..]);
        return match g.rest.first().map(|w| &w.value) {
            Some(Value::Literal(sub)) => g.unparsed || !git_readonly(sub, &g.rest[1..]),
            _ => true,
        };
    }
    if BENIGN.contains(&base) {
        return false;
    }
    if WRAPPERS.contains(&base) {
        return true;
    }
    matches!(extract(cmd), Row::None)
}

fn body_hash(bytes: &[u8]) -> String {
    format!("#{:016x}", harness_core::hash::fnv1a64(bytes))
}

/// Largest script file whose content is hashed into a verb.
const MAX_SCRIPT_BYTES: u64 = 1 << 20;

/// `p` as an absolute path: absolute as written, or relative to the cmd's cwd
/// (a literal `cd` directory or the hook's cwd; a leading `./` is dropped).
/// [`Unplaced`] when it cannot be placed or has a `.` / `..` component.
fn resolve_path(p: &str, cwd: &Value) -> Result<String, Unplaced> {
    if p.split('/').skip(1).any(|c| c == ".." || c == ".") && p.starts_with('/') {
        return Err(Unplaced);
    }
    if p.starts_with('/') {
        return Ok(p.to_string());
    }
    let mut rest = p;
    while let Some(r) = rest.strip_prefix("./") {
        rest = r;
    }
    if rest.is_empty()
        || rest
            .split('/')
            .any(|c| c == ".." || c == "." || c.is_empty())
    {
        return Err(Unplaced);
    }
    let base = match cwd {
        Value::Literal(c) => c.clone(),
        Value::Undetermined(r) if r == super::eval::HOOK_CWD => {
            super::eval::hook_dir().ok_or(Unplaced)?
        }
        Value::Undetermined(_) => return Err(Unplaced),
    };
    Ok(format!("{}/{rest}", base.trim_end_matches('/')))
}

/// A path that cannot be placed in the file system statically.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Unplaced;

/// The key of `p` in [`Written`]: its absolute spelling when it can be placed.
fn key_of(p: &str, cwd: &Value) -> String {
    match resolve_path(p, cwd) {
        Ok(abs) => abs,
        Err(Unplaced) => p.to_string(),
    }
}

/// The `#<hash>` token of a program FILE (user ruling 2026-11-28, R2 for
/// program files): the hash of the heredoc this command line wrote into it,
/// else of its current on-disk content. `?` (never Allow-learnable) when the
/// path is unresolved or relative to an unknown cwd, spelled with `.` / `..`,
/// written by something whose content is not pinned (or lies under a directory
/// something wrote), an undetermined or unmodelled write came before, or the
/// file cannot be read (missing, too large, unreadable).
///
/// `shebang_only`: the file is only a program when it starts with `#!` (a
/// directly executed path); anything else is a binary, named by its basename
/// alone, and gets no token (`None`).
fn file_program_token(
    path: &Value,
    written: &Written,
    cwd: &Value,
    shebang_only: bool,
) -> Option<String> {
    let unpinned = || Some(UNRESOLVED_VERB.to_string());
    let Value::Literal(p) = path else {
        return unpinned();
    };
    let Ok(abs) = resolve_path(p, cwd) else {
        return unpinned();
    };
    if written.contains_key(CONCURRENT) {
        return unpinned();
    }
    if let Some(entry) = written.get(&abs) {
        return match entry {
            Some(body) => Some(body_hash(body.as_bytes())),
            None => unpinned(),
        };
    }
    let under_written = written
        .keys()
        .any(|k| k != POISON && abs.starts_with(&format!("{}/", k.trim_end_matches('/'))));
    if under_written || written.contains_key(POISON) {
        return unpinned();
    }
    match std::fs::metadata(&abs) {
        Ok(m) if m.is_file() => {
            if shebang_only {
                use std::io::Read;
                let mut head = [0u8; 2];
                let read = std::fs::File::open(&abs).and_then(|mut f| f.read_exact(&mut head));
                match read {
                    Ok(()) if &head == b"#!" => {}
                    Ok(()) => return None,
                    // Shorter than two bytes: not a script, not a binary.
                    Err(_) => return unpinned(),
                }
            }
            if m.len() > MAX_SCRIPT_BYTES {
                return unpinned();
            }
            match std::fs::read(&abs) {
                Ok(bytes) => Some(body_hash(&bytes)),
                Err(_) => unpinned(),
            }
        }
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
///   call. An option's VALUE is never in the verb (`git -C /p`, `git -c k=v`;
///   before the subcommand, an operand shaped like a path / `k=v` is a value).
/// * an inline interpreter program (`-c` / `-e` body, a literal heredoc or
///   here-string on stdin) adds `#<fnv1a64 of the body>`; a program the
///   evaluator cannot pin (wrapper, `eval`, piped from a command) adds the
///   unresolved token `?`, which makes the shape never learnable as Allow.
/// * a program FILE (`python3 a.py`, `awk -f`, `make -f`, `deno run`, a
///   directly executed script `./run.sh`) adds the hash of its content (see
///   [`file_program_token`]; heredoc written earlier on the line wins), a
///   wrapper keeps the wrapped tool's subcommand, and `xargs` / `$(..)`
///   operands add `?` (schema `s3-`).
/// * a git write whose repository is not determined adds `?`.
fn verb_token(
    cmd: &SimpleCommand,
    base: &str,
    written: &Written,
    git_undetermined: bool,
) -> String {
    let args = cmd.argv.get(1..).unwrap_or(&[]);
    let all = parse_args(args);
    let mut parts: Vec<String> = vec![base.to_string()];
    let mut unpinned = git_undetermined;
    let flags;
    if WRAPPERS.contains(&base) {
        // A wrapper runs the command named in its operands: keep THAT
        // command's tool and subcommand (`env git branch -D` must not become
        // `env -D`). Every option of the line is in the verb.
        let inner = args.iter().position(|w| {
            matches!(&w.value, Value::Literal(s) if !s.starts_with('-')
                && SUBCOMMAND_TOOLS.contains(&s.rsplit('/').next().unwrap_or(s)))
        });
        match inner {
            Some(j) => {
                let inner_base = match &args[j].value {
                    Value::Literal(s) => s.rsplit('/').next().unwrap_or(s),
                    Value::Undetermined(_) => "?",
                };
                parts.push(inner_base.to_string());
                let view = tool_view(inner_base, &args[j + 1..]);
                parts.extend(view.subs);
                unpinned |= view.unpinned;
                let mut f = parse_args(&args[..j]).flags;
                f.extend(view.flags);
                flags = f;
            }
            None => flags = all.flags.clone(),
        }
        // A wrapped interpreter this module models runs a program nothing
        // here pins.
        unpinned |= args.iter().enumerate().any(|(i, w)| match &w.value {
            Value::Literal(s) if !s.starts_with('-') => {
                let b = s.rsplit('/').next().unwrap_or(s);
                interp::EXTRA.contains(&b) && !interp::extra_programs(b, &args[i + 1..]).is_empty()
            }
            _ => false,
        });
    } else {
        let view = tool_view(base, args);
        parts.extend(view.subs);
        unpinned |= view.unpinned;
        flags = view.flags;
    }
    parts.extend(flags);
    match program_source(cmd) {
        Some(ProgramSource::InlineLiteral(body))
        | Some(ProgramSource::Stdin(StdinSource::Literal(body))) => {
            parts.push(body_hash(body.as_bytes()));
        }
        Some(ProgramSource::File(f)) | Some(ProgramSource::Stdin(StdinSource::File(f))) => {
            parts.extend(file_program_token(&f, written, &cmd.cwd, false));
        }
        None => {
            for prog in interp::extra_programs(base, args) {
                match prog {
                    interp::Prog::Inline(body) => parts.push(body_hash(body.as_bytes())),
                    interp::Prog::File(f) => {
                        parts.extend(file_program_token(&f, written, &cmd.cwd, false))
                    }
                    interp::Prog::Unpinned => unpinned = true,
                }
            }
            // A directly executed script: `./run.sh`, `/abs/run.sh`.
            if let Some(Value::Literal(name)) = cmd.argv.first().map(|w| &w.value) {
                if name.contains('/') {
                    parts.extend(file_program_token(
                        &Value::Literal(name.clone()),
                        written,
                        &cmd.cwd,
                        true,
                    ));
                }
            }
        }
        Some(_) => unpinned = true,
    }
    // Operands the key cannot pin: `xargs` reads its command's arguments (and
    // flags) from stdin, and a `$(..)` / backquote substitution supplies words
    // at run time. Neither is in the key, so the shape must never be Allow.
    if base == "xargs" || all.substituted {
        unpinned = true;
    }
    if unpinned {
        parts.push(UNRESOLVED_VERB.to_string());
    }
    parts.join(" ")
}

/// What the key takes from one tool's arguments.
struct ToolView {
    subs: Vec<String>,
    flags: BTreeSet<String>,
    /// The arguments cannot be pinned (`?`).
    unpinned: bool,
}

/// The subcommand(s) and options of `tool args..`. `git` has global options
/// with values (`-C dir`, `-c k=v`) that are split off first; a `git push`
/// `+refspec` is a force push and a `:refspec` a deletion (R1).
fn tool_view(tool: &str, args: &[super::Word]) -> ToolView {
    if tool == "git" {
        let g = parse_git(args);
        let p = parse_args(g.rest);
        let subs = subcommand_parts("git", &p);
        let mut flags = g.flags;
        flags.extend(p.flags.iter().cloned());
        if subs.first().map(String::as_str) == Some("push") {
            for r in p.positionals.iter().skip(1) {
                if r.starts_with('+') {
                    flags.insert("--force".to_string());
                } else if r.starts_with(':') {
                    flags.insert("--delete".to_string());
                }
            }
        }
        return ToolView {
            subs,
            flags,
            unpinned: g.unparsed,
        };
    }
    let p = parse_args(args);
    ToolView {
        subs: subcommand_parts(tool, &p),
        flags: p.flags,
        unpinned: false,
    }
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
    /// An operand before the first subcommand that follows an option and is
    /// shaped like a path / `k=v` was taken as that option's value and left
    /// out of `positionals`.
    value_skipped: bool,
    /// An operand the evaluator could not resolve was a `$(..)` / backquote
    /// substitution (a plain `$var` is not: it is the pinned `rm "$f"` shape).
    substituted: bool,
}

/// An operand that is an option's value rather than a subcommand: a subcommand
/// name has none of these characters.
fn looks_like_value(s: &str) -> bool {
    s.contains(['/', '=', '.', ':', '~'])
}

fn parse_args(args: &[super::Word]) -> ParsedArgs<'_> {
    let mut p = ParsedArgs {
        flags: BTreeSet::new(),
        positionals: Vec::new(),
        option_before_first_operand: false,
        value_skipped: false,
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
        } else if p.positionals.is_empty() && p.option_before_first_operand && looks_like_value(s) {
            p.value_skipped = true;
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
    if (p.option_before_first_operand && !p.value_skipped) || first.starts_with('+') {
        depth += 1;
    }
    if TWO_LEVEL_TOOLS.contains(&base) || (base == "git" && GIT_GROUPS.contains(&first)) {
        depth += 1;
    }
    // A path / `k=v` operand is a value or a program file (`bun a.ts`), never
    // a subcommand name: it stays out of the key.
    p.positionals
        .iter()
        .take(depth)
        .take_while(|s| !looks_like_value(s))
        .map(|s| s.to_string())
        .collect()
}
