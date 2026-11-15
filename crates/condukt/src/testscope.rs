//! `condukt testscope` — PreToolUse(Bash) gate that enforces the test-scope
//! rulings in code instead of in prompt prose.
//!
//! User rulings (2026-11-07 / 2026-11-08 / 2026-11-12 / 2026-11-14): a full
//! suite is nightly-only; per change only the targeted test files run, once;
//! a run does not chase other sessions' work on main until it is done. Prompt
//! text did not hold (a delegated run executed
//! `cargo test -p blastguard --no-fail-fast` and re-merged main mid-run), so
//! the rules are judged here, deterministically:
//!
//! 1. **Full suite** — `cargo test` / `cargo nextest run` with no target
//!    selector (`--test`, `--lib`, `--bin`, `--doc`, `--example`, `--bench`),
//!    or with `--workspace` / `--all` / `--tests`, is denied.
//! 2. **Mid-run main merge** — inside a condukt task worktree
//!    (`$HOME/.condukt/worktrees/…`), `git merge|rebase` onto main /
//!    origin/main and any `git pull` are denied. A run integrates once, at
//!    the end, through `condukt worktree merge` (which runs git itself, not
//!    through this hook).
//! 3. **Same-tree rerun** — an allowed test command is recorded with the
//!    identity of the tree it ran on; the same command on the same tree is
//!    denied. The key is the same whichever route runs the command: a bare
//!    Bash `cargo test`, `bash -c`, `condukt sandbox run --cmd`, `tdd
//!    red|green --cmd <c>` / `--cmd=<c>` (whose `<c>` is judged like a bare
//!    command for every rule), and `condukt state test`, which calls
//!    [`judge`] itself before running anything.
//! 4. **Task scope** (2026-11-14) — when the cwd is inside a run task's
//!    recorded worktree, each `--test <name>` must appear as `--test <name>`
//!    in that task's `reproduction_tests` or be a `tests/<name>.rs` the
//!    worktree added or changed against `default_branch` (`<base>...HEAD`,
//!    uncommitted, untracked); `--lib`/`--doc`/`--bin`/`--example`/`--bench`
//!    only when `reproduction_tests` names the same selector. Outside every
//!    recorded task worktree this rule does not apply. A cwd owned by several
//!    tasks, any run-state file that cannot be read or parsed, an unreadable
//!    decomposition or a failing git is denied.
//!
//! The hook also writes a per-session marker ([`write_session_marker`]) that
//! `condukt state set --status running` requires, so a session that never
//! loaded this hook cannot start a task.
//!
//! Every cannot-determine (unbalanced quotes around a cargo test, a tree
//! identity git cannot give, an unwritable ledger) resolves to deny
//! (CLAUDE.md §3). A command that contains no test / merge invocation is never
//! judged and is allowed by printing nothing.
//!
//! **Where a test runs** (backlog 214b39b5): a `cd <dir>` that is certain to
//! run before a test (head of a list followed by `&&` / `;`, or inside an
//! all-`&&` chain) moves the test into `<dir>`; rule 3 identifies THAT tree,
//! and rule 4 is judged both in `<dir>` and in the payload cwd. A `cd` whose
//! target is not a literal path (`$VAR`, `-`, globs, `pushd`/`popd`, `~user`,
//! a bare relative name while `CDPATH` is set) or which is conditional
//! (`||`, `|`, `&`, after a non-head `&&` member followed by `;`) leaves the
//! directory undetermined, and a test there is denied. See [`judge_pieces`].
//! When the run directory's tree differs from the payload cwd's tree, the
//! rerun key also records the payload cwd's tree, so editing the payload cwd
//! permits a rerun through the same `cd`; a test run directly in a tree and a
//! `cd` route into the same tree always collide.
//!
//! Not covered (stated, not hidden): `--manifest-path` and `git -C`-style
//! redirection inside cargo, directory changes by anything other than a
//! literal `cd` (a sourced script, `builtin cd`, `eval`), and `tdd
//! red|green` without `--cmd` (which runs tdd's configured `test_cmd`) are not
//! judged — such a test is attributed to the directory the walk last knew.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The decision for one Bash command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Deny(String),
}

const FULL_SUITE_REASON: &str = "full suite is nightly-only (user ruling 2026-11-07/11-12): \
     run only the RED file, its controls and the module's direct tests via \
     --test <file> / --lib <filter>";

const MAIN_MERGE_REASON: &str = "a condukt run integrates once at the end via `condukt worktree \
     merge`; do not merge, pull or rebase main into a task worktree mid-run, and ignore other \
     worktrees' work until this run is done (user ruling 2026-11-08/11-14)";

/// One simple command: its words after shell tokenizing.
type Words = Vec<String>;

/// Judge a Bash command line run in `cwd`. `home` is `$HOME` (for the condukt
/// worktree root); `ledger` is where same-tree runs are recorded.
pub fn judge(command: &str, cwd: &Path, home: Option<&Path>, ledger: &Path) -> Decision {
    let stripped = strip_heredoc_bodies(command);
    let Some(pieces) = split_pieces(&stripped) else {
        return if mentions_test_or_merge(&stripped) {
            Decision::Deny(format!(
                "condukt testscope cannot tokenize this command (unbalanced quote), and it \
                 mentions a cargo test / git merge, so whether it is a full suite or a \
                 mid-run main merge is undetermined: {FULL_SUITE_REASON}"
            ))
        } else {
            Decision::Allow
        };
    };
    let ctx = Ctx {
        wt_root: home.map(|h| h.join(".condukt").join("worktrees")),
        cwd_in_wt: home
            .map(|h| cwd.starts_with(h.join(".condukt").join("worktrees")))
            .unwrap_or(false),
        home: home.map(Path::to_path_buf),
    };

    let test_cmds = match judge_pieces(&pieces, &Dir::Known(cwd.to_path_buf()), &ctx, 0, "") {
        Judged::Deny(r) => return Decision::Deny(r),
        Judged::Test(t) => t,
        Judged::Other => return Decision::Allow,
    };
    // Every test must run in a directory we can name; a `cd` we could not
    // resolve leaves the tree under test undetermined (CLAUDE.md §3).
    let mut by_dir: Vec<(PathBuf, Vec<Words>)> = Vec::new();
    for (words, dir) in test_cmds {
        let d = match dir {
            Dir::Known(d) => d,
            Dir::Undetermined(why) => {
                return Decision::Deny(format!(
                    "condukt testscope cannot determine the directory `{}` runs in ({why}), so \
                     its tree, its task scope and whether it re-runs a test already run on the \
                     same tree are undetermined; run it without that `cd`, or with a leading \
                     `cd <literal absolute path> &&`",
                    words.join(" ")
                ));
            }
        };
        match by_dir.iter_mut().find(|(p, _)| *p == d) {
            Some((_, v)) => v.push(words),
            None => by_dir.push((d, vec![words])),
        }
    }
    // Rule C (task scope) is judged in the directory each test runs in AND in
    // the payload cwd: a worker inside its task worktree does not escape its
    // scope by `cd`-ing elsewhere, and a `cd` into a task worktree does not
    // escape that task's scope (backlog 214b39b5).
    for (d, tests) in &by_dir {
        if let Decision::Deny(r) = check_task_scope(tests, d) {
            return Decision::Deny(r);
        }
        if d != cwd {
            if let Decision::Deny(r) = check_task_scope(tests, cwd) {
                return Decision::Deny(r);
            }
        }
    }
    check_rerun(&by_dir, cwd, ledger)
}

/// Where a simple command runs, as far as the leading `cd`s of its command
/// line determine it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Dir {
    Known(PathBuf),
    /// A `cd` whose target or whose execution cannot be decided statically
    /// (variable, `-`, `pushd`, conditional on another command, …).
    Undetermined(String),
}

impl Dir {
    fn undetermined(why: impl Into<String>) -> Self {
        Dir::Undetermined(why.into())
    }
}

/// Per-command-line facts the segment judge needs.
struct Ctx {
    /// `$HOME/.condukt/worktrees`, when `$HOME` is known.
    wt_root: Option<PathBuf>,
    /// Whether the payload cwd is inside `wt_root`.
    cwd_in_wt: bool,
    home: Option<PathBuf>,
}

impl Ctx {
    /// Rule 2 applies when the payload cwd or the directory the command runs
    /// in is a condukt task worktree; an undetermined directory might be one.
    fn in_task_worktree(&self, dir: &Dir) -> bool {
        self.cwd_in_wt
            || match dir {
                Dir::Known(d) => self.wt_root.as_ref().is_some_and(|r| d.starts_with(r)),
                Dir::Undetermined(_) => true,
            }
    }
}

enum Judged {
    Deny(String),
    /// Scoped (allowed so far) test invocations, normalized, each with the
    /// directory it runs in.
    Test(Vec<(Words, Dir)>),
    Other,
}

/// Judge every simple command of a tokenized command line, tracking the
/// directory leading `cd`s move into. `what` names the program for messages
/// ("" for the hook's own command line).
///
/// A `cd` changes the directory of later commands only when it is certain to
/// have run before them and they are certain to run after it:
///
/// * at the head of a list (start, after `;` / newline / `(`) and followed by
///   `&&`, `;`, newline, `)` or the end — every later command of the list runs
///   in its target (if the `cd` fails, a `;`-joined successor runs in the old
///   directory, but then the target does not exist and its tree identity
///   fails, which denies);
/// * later in an all-`&&` list and followed by `&&` — its target holds only
///   while the `&&` run continues; at the first `;`, `||`, `|` or `&` the
///   directory becomes undetermined (an earlier member may have failed, so
///   the `cd` may not have run).
///
/// Any other `cd` (after `||` / `|` / `&`, or followed by them, `pushd`,
/// `popd`) makes the directory undetermined. `( … )` and `` ` … ` `` restore
/// the directory they started with.
fn judge_pieces(pieces: &[Piece], start: &Dir, ctx: &Ctx, depth: usize, what: &str) -> Judged {
    let mut dir = start.clone();
    // (dir, provisional) saved at each `(` / opening backtick.
    let mut stack: Vec<(Dir, bool)> = Vec::new();
    let mut in_tick = false;
    let mut at_head = true;
    let mut all_and = true;
    // `dir` was set by a non-head `cd` and holds only while the `&&` run lasts.
    let mut provisional = false;
    let mut found: Vec<(Words, Dir)> = Vec::new();
    let break_provisional = |dir: &mut Dir, provisional: &mut bool| {
        if *provisional {
            *dir = Dir::undetermined(
                "a `cd` that ran only if an earlier `&&` member succeeded is followed by a \
                 command that runs either way",
            );
            *provisional = false;
        }
    };
    for (idx, piece) in pieces.iter().enumerate() {
        let seg = match piece {
            Piece::Op(op) => {
                match op {
                    Op::Seq => {
                        break_provisional(&mut dir, &mut provisional);
                        at_head = true;
                        all_and = true;
                    }
                    Op::And | Op::Redirect => {}
                    Op::Or | Op::Pipe | Op::Bg => {
                        break_provisional(&mut dir, &mut provisional);
                        all_and = false;
                    }
                    Op::Open => {
                        stack.push((dir.clone(), provisional));
                        provisional = false;
                        at_head = true;
                        all_and = true;
                    }
                    Op::Close => {
                        if let Some((d, p)) = stack.pop() {
                            dir = d;
                            provisional = p;
                        }
                        at_head = false;
                    }
                    Op::Tick => {
                        if in_tick {
                            if let Some((d, p)) = stack.pop() {
                                dir = d;
                                provisional = p;
                            }
                            at_head = false;
                        } else {
                            stack.push((dir.clone(), provisional));
                            provisional = false;
                            at_head = true;
                            all_and = true;
                        }
                        in_tick = !in_tick;
                    }
                }
                continue;
            }
            Piece::Seg(s) => s,
        };
        let Some(words) = shlex::split(seg) else {
            if mentions_test_or_merge(seg) {
                return Judged::Deny(if what.is_empty() {
                    format!(
                        "condukt testscope cannot tokenize `{seg}`, and it mentions a cargo \
                         test / git merge: {FULL_SUITE_REASON}"
                    )
                } else {
                    format!(
                        "condukt testscope cannot tokenize `{seg}` in {what}: \
                         {FULL_SUITE_REASON}"
                    )
                });
            }
            at_head = false;
            continue;
        };
        let stripped_words = strip_prefixes(&words);
        if let Some(first) = stripped_words.first() {
            if matches!(first.as_str(), "cd" | "pushd" | "popd") {
                let next = pieces[idx + 1..]
                    .iter()
                    .find_map(|p| match p {
                        Piece::Op(Op::Redirect) => None,
                        Piece::Op(o) => Some(Some(*o)),
                        Piece::Seg(_) => Some(None),
                    })
                    .flatten();
                let target = resolve_cd(stripped_words, &dir, ctx.home.as_deref());
                if at_head && matches!(next, None | Some(Op::Seq | Op::And | Op::Close)) {
                    dir = target;
                    provisional = false;
                } else if all_and && next == Some(Op::And) {
                    dir = target;
                    provisional = true;
                } else {
                    dir = Dir::undetermined(format!(
                        "`{}` is not certain to have run before the commands after it",
                        words.join(" ")
                    ));
                    provisional = false;
                }
                at_head = false;
                continue;
            }
        }
        at_head = false;
        match judge_words(&words, &dir, ctx, depth) {
            Judged::Deny(r) => return Judged::Deny(r),
            Judged::Test(t) => found.extend(t),
            Judged::Other => {}
        }
    }
    if found.is_empty() {
        Judged::Other
    } else {
        Judged::Test(found)
    }
}

/// The directory `cd <args>` moves into from `dir`, resolved statically:
/// a literal absolute path, a literal relative path (lexically against
/// `dir`), `~` / `~/…` against `$HOME`, or no argument (`$HOME`). Anything
/// else — `-`, a variable, a glob, command substitution, several operands,
/// `pushd` / `popd`, a bare relative name while `CDPATH` is set — is
/// undetermined.
fn resolve_cd(words: &[String], dir: &Dir, home: Option<&Path>) -> Dir {
    let shown = words.join(" ");
    if words[0] != "cd" {
        return Dir::undetermined(format!("`{shown}` changes the directory stack"));
    }
    let mut ops: Vec<&str> = Vec::new();
    let mut opts_done = false;
    for a in &words[1..] {
        if !opts_done && a == "--" {
            opts_done = true;
        } else if !opts_done && a.starts_with('-') && a != "-" {
            if !matches!(a.as_str(), "-L" | "-P" | "-e" | "-LP" | "-PL") {
                return Dir::undetermined(format!("`{shown}`: unknown cd option `{a}`"));
            }
        } else {
            ops.push(a.as_str());
        }
    }
    let home_dir = || match home {
        Some(h) => Dir::Known(h.to_path_buf()),
        None => Dir::undetermined(format!("`{shown}` moves to $HOME, which is unknown")),
    };
    let arg = match ops.as_slice() {
        [] => return home_dir(),
        [a] => *a,
        _ => return Dir::undetermined(format!("`{shown}` has several operands")),
    };
    if arg.is_empty() || arg == "-" || arg.contains(['$', '`', '*', '?', '[', '{']) {
        return Dir::undetermined(format!("`{shown}`: the target is not a literal path"));
    }
    let target = if arg == "~" {
        match home_dir() {
            Dir::Known(h) => h,
            u => return u,
        }
    } else if let Some(rest) = arg.strip_prefix("~/") {
        match home_dir() {
            Dir::Known(h) => h.join(rest),
            u => return u,
        }
    } else if arg.starts_with('~') {
        return Dir::undetermined(format!("`{shown}`: `~user` is not resolved"));
    } else if Path::new(arg).is_absolute() {
        PathBuf::from(arg)
    } else {
        let dotted = arg == "." || arg == ".." || arg.starts_with("./") || arg.starts_with("../");
        if !dotted && std::env::var_os("CDPATH").is_some_and(|v| !v.is_empty()) {
            return Dir::undetermined(format!("`{shown}`: CDPATH is set"));
        }
        match dir {
            Dir::Known(d) => d.join(arg),
            Dir::Undetermined(why) => return Dir::Undetermined(why.clone()),
        }
    };
    Dir::Known(normalize_lexically(&target))
}

/// Resolve `.` and `..` components without touching the filesystem (the
/// logical `cd -L` view).
fn normalize_lexically(p: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn judge_words(words: &[String], dir: &Dir, ctx: &Ctx, depth: usize) -> Judged {
    let in_task_worktree = ctx.in_task_worktree(dir);
    let w = strip_prefixes(words);
    let Some(first) = w.first() else {
        return Judged::Other;
    };
    let base = Path::new(first.as_str())
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(first.as_str());
    match base {
        "bash" | "sh" | "zsh" if depth < 4 => {
            if let Some(pos) = w.iter().position(|a| a == "-c" || a == "-lc") {
                if let Some(payload) = w.get(pos + 1) {
                    return judge_payload(payload, dir, ctx, depth + 1);
                }
            }
            Judged::Other
        }
        // `condukt sandbox run --cmd '<program>'` runs <program> in a container;
        // the program is judged like a `-c` payload.
        "condukt" if depth < 4 && w.get(1).map(String::as_str) == Some("sandbox") => {
            match cmd_flag(w) {
                Some(p) => judge_payload(&p, dir, ctx, depth + 1),
                None => Judged::Other,
            }
        }
        // `tdd red|green … --cmd '<c>'` runs <c> as the test command; it is
        // judged exactly like the bare command (full suite, task scope and the
        // same rerun-ledger key), so routing a test through tdd changes nothing.
        "tdd" if depth < 4 && matches!(w.get(1).map(String::as_str), Some("red" | "green")) => {
            match cmd_flag(w) {
                Some(p) => judge_payload(&p, dir, ctx, depth + 1),
                None => Judged::Other,
            }
        }
        "cargo" => judge_cargo(&w[1..], dir),
        "git" if in_task_worktree => judge_git(&w[1..]),
        _ => Judged::Other,
    }
}

/// A nested program (`bash -c`, `--cmd`) starts in the directory of the
/// command that runs it; its own `cd`s do not leak out.
fn judge_payload(payload: &str, dir: &Dir, ctx: &Ctx, depth: usize) -> Judged {
    let stripped = strip_heredoc_bodies(payload);
    let Some(pieces) = split_pieces(&stripped) else {
        return if mentions_test_or_merge(&stripped) {
            Judged::Deny(format!(
                "condukt testscope cannot tokenize the -c program: {FULL_SUITE_REASON}"
            ))
        } else {
            Judged::Other
        };
    };
    judge_pieces(&pieces, dir, ctx, depth, "the -c program")
}

/// The value of `--cmd <v>` / `--cmd=<v>` in `w`, if any.
fn cmd_flag(w: &[String]) -> Option<String> {
    w.iter().enumerate().find_map(|(i, a)| {
        if a == "--cmd" {
            w.get(i + 1).cloned()
        } else {
            a.strip_prefix("--cmd=").map(str::to_string)
        }
    })
}

/// Drop leading `VAR=val` assignments and the `env` / `timeout N` / `nice`
/// wrappers so the real command word comes first.
fn strip_prefixes(words: &[String]) -> &[String] {
    let mut i = 0;
    loop {
        let Some(w) = words.get(i) else {
            return &words[i..];
        };
        if is_assignment(w) {
            i += 1;
        } else if w == "env" || w == "nice" || w == "command" || w == "exec" {
            i += 1;
            while words.get(i).is_some_and(|a| a.starts_with('-')) {
                i += 1;
            }
        } else if w == "timeout" {
            i += 1;
            while words.get(i).is_some_and(|a| a.starts_with('-')) {
                i += 1;
            }
            i += 1; // the duration
        } else {
            return &words[i..];
        }
    }
}

fn is_assignment(w: &str) -> bool {
    match w.split_once('=') {
        Some((name, _)) => {
            !name.is_empty()
                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                && !name.starts_with(|c: char| c.is_ascii_digit())
        }
        None => false,
    }
}

fn judge_cargo(args: &[String], dir: &Dir) -> Judged {
    let mut i = 0;
    // `cargo +toolchain …` and global flags before the subcommand.
    while args
        .get(i)
        .is_some_and(|a| a.starts_with('+') || a.starts_with('-'))
    {
        i += 1;
    }
    let rest: &[String] = match args.get(i).map(String::as_str) {
        Some("test") | Some("t") => &args[i + 1..],
        Some("nextest") if args.get(i + 1).map(String::as_str) == Some("run") => &args[i + 2..],
        _ => return Judged::Other,
    };
    let opts: Vec<&String> = rest.iter().take_while(|a| a.as_str() != "--").collect();
    let flag = |name: &str| {
        opts.iter()
            .any(|a| a.as_str() == name || a.starts_with(&format!("{name}=")))
    };
    if flag("--workspace") || flag("--all") || flag("--tests") {
        return Judged::Deny(format!(
            "`cargo {}` runs every test target: {FULL_SUITE_REASON}",
            args.join(" ")
        ));
    }
    let selected = ["--test", "--lib", "--bin", "--doc", "--example", "--bench"]
        .iter()
        .any(|s| flag(s));
    if !selected {
        return Judged::Deny(format!(
            "`cargo {}` names no test target, so it builds and runs the whole suite: \
             {FULL_SUITE_REASON}",
            args.join(" ")
        ));
    }
    let mut norm = vec!["cargo".to_string()];
    norm.extend(args.iter().cloned());
    Judged::Test(vec![(norm, dir.clone())])
}

fn judge_git(args: &[String]) -> Judged {
    let mut i = 0;
    // `git -C dir …` / `git -c k=v …`
    while let Some(a) = args.get(i) {
        if a == "-C" || a == "-c" {
            i += 2;
        } else if a.starts_with('-') {
            i += 1;
        } else {
            break;
        }
    }
    let Some(sub) = args.get(i) else {
        return Judged::Other;
    };
    let operands: Vec<&str> = args[i + 1..]
        .iter()
        .filter(|a| !a.starts_with('-'))
        .map(String::as_str)
        .collect();
    let is_main = |s: &str| {
        matches!(
            s,
            "main" | "master" | "origin/main" | "origin/master" | "@{u}" | "@{upstream}"
        )
    };
    let deny = match sub.as_str() {
        "pull" => true,
        "merge" | "rebase" => operands.iter().any(|o| is_main(o)),
        _ => false,
    };
    if deny {
        Judged::Deny(format!("`git {}`: {MAIN_MERGE_REASON}", args.join(" ")))
    } else {
        Judged::Other
    }
}

fn mentions_test_or_merge(s: &str) -> bool {
    (s.contains("cargo") && s.contains("test"))
        || (s.contains("git")
            && (s.contains("merge") || s.contains("pull") || s.contains("rebase")))
}

/// Remove here-document bodies: after a line holding `<<DELIM` / `<<-DELIM`
/// (optionally quoted), drop the following lines up to the delimiter line.
/// Body text is data, never a command.
fn strip_heredoc_bodies(cmd: &str) -> String {
    let mut out = String::new();
    let mut pending: Vec<(String, bool)> = Vec::new();
    let mut lines = cmd.split('\n').peekable();
    while let Some(line) = lines.next() {
        if let Some((delim, dash)) = pending.first().cloned() {
            let probe = if dash {
                line.trim_start_matches('\t')
            } else {
                line
            };
            if probe == delim {
                pending.remove(0);
            }
            continue;
        }
        out.push_str(line);
        if lines.peek().is_some() {
            out.push('\n');
        }
        pending.extend(heredoc_delims(line));
    }
    out
}

fn heredoc_delims(line: &str) -> Vec<(String, bool)> {
    let mut found = Vec::new();
    let b = line.as_bytes();
    let mut i = 0;
    let mut quote: Option<u8> = None;
    while i < b.len() {
        let c = b[i];
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                } else if c == b'\\' && q == b'"' {
                    i += 1;
                }
            }
            None => {
                if c == b'\'' || c == b'"' {
                    quote = Some(c);
                } else if c == b'\\' {
                    i += 1;
                } else if c == b'<' && b.get(i + 1) == Some(&b'<') && b.get(i + 2) != Some(&b'<') {
                    let mut j = i + 2;
                    let dash = b.get(j) == Some(&b'-');
                    if dash {
                        j += 1;
                    }
                    while b.get(j) == Some(&b' ') {
                        j += 1;
                    }
                    let mut delim = String::new();
                    while let Some(&ch) = b.get(j) {
                        if ch == b'\'' || ch == b'"' || ch == b'\\' {
                            j += 1;
                            continue;
                        }
                        if ch.is_ascii_alphanumeric() || ch == b'_' || ch == b'-' || ch == b'.' {
                            delim.push(ch as char);
                            j += 1;
                        } else {
                            break;
                        }
                    }
                    if !delim.is_empty() {
                        found.push((delim, dash));
                    }
                    i = j;
                    continue;
                }
            }
        }
        i += 1;
    }
    found
}

/// Split a command line into simple-command segments at `;`, `&`, `|`,
/// newlines and parentheses outside quotes. `None` when a quote is unbalanced.
fn split_segments(cmd: &str) -> Option<Vec<String>> {
    Some(
        split_pieces(cmd)?
            .into_iter()
            .filter_map(|p| match p {
                Piece::Seg(s) => Some(s),
                Piece::Op(_) => None,
            })
            .collect(),
    )
}

/// The control operator that separated two segments.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    /// `;` or newline.
    Seq,
    And,
    Or,
    /// `|` or `|&`.
    Pipe,
    /// A lone `&` (background).
    Bg,
    Open,
    Close,
    Tick,
    /// The `&` of a redirection (`2>&1`, `<&0`, `&>file`): not a separator,
    /// but the segment is still split there as before.
    Redirect,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Piece {
    Seg(String),
    Op(Op),
}

/// [`split_segments`] with the operators kept, so the walk can tell which
/// commands a `cd` is certain to precede. `None` when a quote is unbalanced.
fn split_pieces(cmd: &str) -> Option<Vec<Piece>> {
    let mut segs: Vec<Piece> = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut chars = cmd.chars().peekable();
    while let Some(c) = chars.next() {
        match quote {
            Some(q) => {
                cur.push(c);
                if c == q {
                    quote = None;
                } else if c == '\\' && q == '"' {
                    if let Some(n) = chars.next() {
                        cur.push(n);
                    }
                }
            }
            None => match c {
                '\'' | '"' => {
                    quote = Some(c);
                    cur.push(c);
                }
                '\\' => {
                    match chars.next() {
                        Some('\n') => {} // line continuation
                        Some(n) => {
                            cur.push('\\');
                            cur.push(n);
                        }
                        None => cur.push('\\'),
                    }
                }
                ';' | '&' | '|' | '\n' | '(' | ')' | '`' => {
                    let op = match c {
                        ';' | '\n' => Op::Seq,
                        '&' if cur.ends_with(['>', '<']) => Op::Redirect,
                        '&' if chars.peek() == Some(&'>') => Op::Redirect,
                        '&' if chars.peek() == Some(&'&') => {
                            chars.next();
                            Op::And
                        }
                        '&' => Op::Bg,
                        '|' if chars.peek() == Some(&'|') => {
                            chars.next();
                            Op::Or
                        }
                        '|' => {
                            if chars.peek() == Some(&'&') {
                                chars.next();
                            }
                            Op::Pipe
                        }
                        '(' => Op::Open,
                        ')' => Op::Close,
                        _ => Op::Tick,
                    };
                    if !cur.trim().is_empty() {
                        segs.push(Piece::Seg(std::mem::take(&mut cur)));
                    }
                    cur.clear();
                    segs.push(Piece::Op(op));
                }
                _ => cur.push(c),
            },
        }
    }
    if quote.is_some() {
        return None;
    }
    if !cur.trim().is_empty() {
        segs.push(Piece::Seg(cur));
    }
    Some(segs)
}

/// The identity of the tree a test runs against: the repo top level, HEAD and
/// a hash of everything that differs from HEAD (tracked diff + untracked list).
fn tree_identity(cwd: &Path) -> Result<(String, String), String> {
    let git = |args: &[&str]| -> Result<String, String> {
        let out = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .map_err(|e| format!("git {}: {e}", args.join(" ")))?;
        if !out.status.success() {
            return Err(format!(
                "git {} exited {}: {}",
                args.join(" "),
                out.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    };
    let top = git(&["rev-parse", "--show-toplevel"])?.trim().to_string();
    let head = git(&["rev-parse", "HEAD"])?.trim().to_string();
    let diff = git(&["diff", "HEAD", "--binary"])?;
    let untracked = git(&["ls-files", "--others", "--exclude-standard"])?;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    std::hash::Hash::hash(&diff, &mut h);
    std::hash::Hash::hash(&untracked, &mut h);
    let dirty = std::hash::Hasher::finish(&h);
    Ok((top, format!("{head}:{dirty:016x}")))
}

/// One rerun-ledger record: the tree a test ran in, the command, and — for a
/// test run through a leading `cd` from a payload cwd whose tree differs — that
/// payload cwd's tree.
///
/// Line format: `<top>\t<head:dirty>\t<cmd>` (no context; the format written
/// before backlog 214b39b5) or `<top>\t<head:dirty>\t@from=<top>:<head:dirty>\t<cmd>`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct LedgerKey {
    top: String,
    tree: String,
    from: Option<String>,
    cmd: String,
}

impl LedgerKey {
    fn line(&self) -> String {
        match &self.from {
            None => format!("{}\t{}\t{}", self.top, self.tree, self.cmd),
            Some(f) => format!("{}\t{}\t@from={f}\t{}", self.top, self.tree, self.cmd),
        }
    }

    fn parse(line: &str) -> Option<Self> {
        let mut it = line.splitn(3, '\t');
        let top = it.next()?.to_string();
        let tree = it.next()?.to_string();
        let rest = it.next()?;
        let (from, cmd) = match rest.strip_prefix("@from=") {
            Some(r) => {
                let (f, c) = r.split_once('\t')?;
                (Some(f.to_string()), c.to_string())
            }
            None => (None, rest.to_string()),
        };
        Some(LedgerKey {
            top,
            tree,
            from,
            cmd,
        })
    }

    /// Same command on the same tree, and not provably launched from two
    /// different payload trees. A record without context (a test run in its
    /// own cwd) collides with every `cd` route into that tree and vice versa,
    /// so `cd`-ing in from elsewhere never re-runs a test already run there.
    fn collides(&self, other: &LedgerKey) -> bool {
        self.top == other.top
            && self.tree == other.tree
            && self.cmd == other.cmd
            && (self.from.is_none() || other.from.is_none() || self.from == other.from)
    }
}

/// Rule 3. `by_dir` maps each directory a test runs in (the payload cwd, or
/// the target of a leading `cd`) to its tests. The identity is taken of the
/// tree each test actually runs in (backlog 214b39b5: it used to be the
/// payload cwd's tree whatever `cd` preceded the test).
///
/// When the run directory's tree differs from the payload cwd's tree, the key
/// also records the payload cwd's tree (`from`): editing the payload cwd's
/// tree then permits a rerun through the same `cd`, as it did before the run
/// directory was keyed (pinned by
/// `backlog_20261114_testscope_cd_tree_identity.rs`
/// `control_editing_payload_cwd_tree_allows_rerun`). A payload cwd whose tree
/// cannot be identified contributes no context, which only makes collisions
/// more likely (never fewer).
fn check_rerun(by_dir: &[(PathBuf, Vec<Words>)], cwd: &Path, ledger: &Path) -> Decision {
    let mut cwd_identity: Option<Option<(String, String)>> = None;
    let mut keys: Vec<LedgerKey> = Vec::new();
    for (dir, tests) in by_dir {
        let (top, tree) = match tree_identity(dir) {
            Ok(t) => t,
            Err(e) => {
                return Decision::Deny(format!(
                    "condukt testscope cannot determine the tree this test runs against in {} \
                     ({e}), so whether it re-runs a test already run on the same tree is \
                     undetermined",
                    dir.display()
                ));
            }
        };
        let from = if dir.as_path() == cwd {
            None
        } else {
            let c = cwd_identity.get_or_insert_with(|| tree_identity(cwd).ok());
            match c.as_ref() {
                Some((ctop, ctree)) if (ctop, ctree) != (&top, &tree) => {
                    Some(format!("{ctop}:{ctree}"))
                }
                _ => None,
            }
        };
        for w in tests {
            keys.push(LedgerKey {
                top: top.clone(),
                tree: tree.clone(),
                from: from.clone(),
                cmd: w.join(" "),
            });
        }
    }
    let existing = match std::fs::read_to_string(ledger) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => {
            return Decision::Deny(format!(
                "condukt testscope cannot read its rerun ledger {}: {e}",
                ledger.display()
            ));
        }
    };
    // A line that does not parse as a key never matched a key before either
    // (the old check was exact line equality), so skipping it changes nothing.
    let recorded: Vec<LedgerKey> = existing.lines().filter_map(LedgerKey::parse).collect();
    for k in &keys {
        if recorded.iter().any(|r| r.collides(k)) {
            return Decision::Deny(format!(
                "rerun refused: `{}` already ran on this exact tree ({} at {}); running the same \
                 test twice is forbidden (user ruling 2026-11-07). Change the code first, or \
                 move on",
                k.cmd, k.tree, k.top
            ));
        }
    }
    let write = || -> std::io::Result<()> {
        if let Some(dir) = ledger.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(ledger)?;
        for k in &keys {
            writeln!(f, "{}", k.line())?;
        }
        Ok(())
    };
    match write() {
        Ok(()) => Decision::Allow,
        Err(e) => Decision::Deny(format!(
            "condukt testscope cannot record this test run in {} ({e}), so a later rerun could \
             not be detected",
            ledger.display()
        )),
    }
}

/// The ledger path: `$CONDUKT_TESTSCOPE_LEDGER`, else
/// `$HOME/.condukt/testscope/ledger.tsv`.
pub fn ledger_path(home: Option<&Path>) -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("CONDUKT_TESTSCOPE_LEDGER") {
        return Some(PathBuf::from(p));
    }
    home.map(|h| h.join(".condukt").join("testscope").join("ledger.tsv"))
}

// ── session marker (rule A) ─────────────────────────────────────────────────

/// `<ledger parent>/sessions/<safe session id>`: the file whose existence says
/// "the testscope hook ran in this session". `None` when the ledger path has no
/// parent or the session id is empty.
pub fn session_marker_path(ledger: &Path, session_id: &str) -> Option<PathBuf> {
    if session_id.trim().is_empty() {
        return None;
    }
    let dir = ledger.parent()?;
    Some(
        dir.join("sessions")
            .join(harness_core::store::safe_session(session_id)),
    )
}

/// Record that the testscope hook is loaded in `session_id`. Called by the hook
/// on every payload that carries a session id; `condukt state set --status
/// running` refuses a session that has no marker ([`session_marker_present`]).
pub fn write_session_marker(ledger: &Path, session_id: &str) -> std::io::Result<()> {
    let Some(p) = session_marker_path(ledger, session_id) else {
        return Ok(());
    };
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&p, b"")
}

/// Whether the marker of `session_id` exists, as an observation: `Err` (never
/// `false`, never `true`) when existence cannot be told.
pub fn session_marker_present(ledger: &Path, session_id: &str) -> Result<bool, String> {
    let Some(p) = session_marker_path(ledger, session_id) else {
        return Ok(false);
    };
    p.try_exists()
        .map_err(|e| format!("cannot tell whether {} exists: {e}", p.display()))
}

// ── task scope (rule C) ─────────────────────────────────────────────────────

/// One target selector of a `cargo test` / `cargo nextest run` invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Selector {
    /// `--test <name>` / `--test=<name>`.
    Test(String),
    /// `--lib`, `--doc`, or `--bin|--example|--bench [<name>]`.
    Kind(String, Option<String>),
}

/// The selectors among `words` (options before `--` only). `Err` for a
/// `--test` with no name (it lists targets instead of naming one, so which test
/// file runs is undetermined).
fn selectors(words: &[String]) -> Result<Vec<Selector>, String> {
    let opts: Vec<&String> = words.iter().take_while(|a| a.as_str() != "--").collect();
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(a) = opts.get(i) {
        let a = a.as_str();
        let value_after = |i: usize| {
            opts.get(i + 1)
                .filter(|v| !v.starts_with('-'))
                .map(|v| v.to_string())
        };
        if a == "--test" {
            match value_after(i) {
                Some(v) => {
                    out.push(Selector::Test(v));
                    i += 1;
                }
                None => {
                    return Err("`--test` names no test target".to_string());
                }
            }
        } else if let Some(v) = a.strip_prefix("--test=") {
            out.push(Selector::Test(v.to_string()));
        } else if a == "--lib" || a == "--doc" {
            out.push(Selector::Kind(a.to_string(), None));
        } else if a == "--bin" || a == "--example" || a == "--bench" {
            let v = value_after(i);
            if v.is_some() {
                i += 1;
            }
            out.push(Selector::Kind(a.to_string(), v));
        } else if let Some((flag, v)) = a.split_once('=') {
            if matches!(flag, "--bin" | "--example" | "--bench") {
                out.push(Selector::Kind(flag.to_string(), Some(v.to_string())));
            }
        }
        i += 1;
    }
    Ok(out)
}

/// Every selector named anywhere in a task's `reproduction_tests` text.
/// Unparseable text is `Err` (what it allows is undetermined).
fn reproduction_selectors(text: &str) -> Result<Vec<Selector>, String> {
    let stripped = strip_heredoc_bodies(text);
    let segs = split_segments(&stripped)
        .ok_or_else(|| "reproduction_tests has an unbalanced quote".to_string())?;
    let mut out = Vec::new();
    for seg in segs {
        let words = shlex::split(&seg)
            .ok_or_else(|| format!("reproduction_tests: cannot tokenize `{seg}`"))?;
        out.extend(selectors(&words)?);
    }
    Ok(out)
}

/// Run git in `dir` and return stdout; a spawn failure or non-zero exit is an
/// `Err` naming it (a checker that died is not a checker that passed).
fn git_stdout(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = harness_core::git_env::scrub(&mut Command::new("git"))
        .args(args)
        .current_dir(dir)
        .output()
        .map_err(|e| format!("git {}: {e}", args.join(" ")))?;
    if !out.status.success() {
        return Err(format!(
            "git {} exited {}: {}",
            args.join(" "),
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The `tests/<name>.rs` stems the worktree at `wt` added or changed against
/// `base`: committed (`git diff --name-only <base>...HEAD`), uncommitted
/// (`git diff --name-only HEAD`) and untracked files.
fn worktree_changed_tests(wt: &Path, base: &str) -> Result<Vec<String>, String> {
    let range = format!("{base}...HEAD");
    let mut paths = String::new();
    paths.push_str(&git_stdout(wt, &["diff", "--name-only", &range])?);
    paths.push('\n');
    paths.push_str(&git_stdout(wt, &["diff", "--name-only", "HEAD"])?);
    paths.push('\n');
    paths.push_str(&git_stdout(
        wt,
        &["ls-files", "--others", "--exclude-standard"],
    )?);
    let mut out: Vec<String> = Vec::new();
    for line in paths.lines() {
        let p = Path::new(line.trim());
        let in_tests = p
            .parent()
            .and_then(|d| d.file_name())
            .and_then(|n| n.to_str())
            == Some("tests");
        if in_tests && p.extension().and_then(|e| e.to_str()) == Some("rs") {
            if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                if !out.iter().any(|s| s == stem) {
                    out.push(stem.to_string());
                }
            }
        }
    }
    Ok(out)
}

const TASK_SCOPE_REASON: &str = "inside a condukt task worktree only the task's own tests may \
     run: a `--test <name>` named in the task's reproduction_tests, or a tests/<name>.rs this \
     worktree added or changed; `--lib`/`--bin`/`--doc` only when reproduction_tests names that \
     selector (user ruling 2026-11-14)";

/// `state::all_runs` for a GATE: the same dirs and the same canonical-first
/// precedence, but any run-state file that cannot be read or parsed (and any
/// state dir that exists but cannot be listed) is an `Err` naming it instead of
/// a stderr warning. A gate that asks "which task owns this path" cannot answer
/// from a partial list — the unread file may be the owner (CLAUDE.md §3). A
/// missing dir still contributes no runs (ENOENT is an observed absence).
fn all_runs_strict(
    cfg: &crate::config::Config,
    cwd: &Path,
) -> anyhow::Result<Vec<crate::state::RunState>> {
    use crate::state::{is_run_state_sidecar, project_state_dirs, RunState};
    use anyhow::{bail, Context};
    let mut runs: Vec<RunState> = Vec::new();
    for dir in project_state_dirs(cfg, cwd)? {
        let rd = match std::fs::read_dir(&dir) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => bail!("cannot list run state dir {}: {e}", dir.display()),
        };
        for entry in rd {
            let entry = entry.with_context(|| format!("unreadable entry in {}", dir.display()))?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let fname = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if is_run_state_sidecar(fname) {
                continue;
            }
            let txt = std::fs::read_to_string(&path)
                .with_context(|| format!("cannot read run state {}", path.display()))?;
            let rs: RunState = serde_json::from_str(&txt)
                .with_context(|| format!("unparseable run state {}", path.display()))?;
            if !runs.iter().any(|r| r.run_id == rs.run_id) {
                runs.push(rs);
            }
        }
    }
    runs.sort_by(|a, b| a.run_id.cmp(&b.run_id));
    Ok(runs)
}

/// Rule C: when `cwd` is inside a run task's recorded worktree, every selector
/// of every test command must be in that task's scope. Outside every recorded
/// task worktree this allows. Undetermined (config / run state / decomposition
/// / git unreadable, cwd owned by several tasks) denies.
fn check_task_scope(tests: &[Words], cwd: &Path) -> Decision {
    let undetermined = |why: String| {
        Decision::Deny(format!(
            "condukt testscope cannot determine whether this test is in a condukt task's scope \
             ({why}), so it is refused: {TASK_SCOPE_REASON}"
        ))
    };
    let cfg = match crate::config::Config::load() {
        Ok(c) => c,
        Err(e) => return undetermined(format!("condukt config unreadable: {e}")),
    };
    let runs = match all_runs_strict(&cfg, cwd) {
        Ok(r) => r,
        Err(e) => return undetermined(format!("run state unreadable: {e:#}")),
    };
    let owners = crate::subagent_stop::tasks_owning_cwd(cwd, &runs);
    let (rs, task) = match owners.as_slice() {
        [] => {
            // A task whose recorded worktree is empty or relative names no
            // directory, so it owns no cwd (backlog b4a2780b: `""` used to own
            // every cwd). But while such a task is still live, a cwd under the
            // condukt worktree base could be its worktree, so ownership there
            // is undetermined. Outside the worktree base, and for terminal
            // tasks, it can be no-one's.
            let live_unusable: Vec<String> =
                crate::subagent_stop::tasks_with_unusable_worktree(&runs)
                    .iter()
                    .filter(|(_, t)| {
                        matches!(
                            t.status,
                            crate::state::Status::Pending
                                | crate::state::Status::Running
                                | crate::state::Status::Done
                                | crate::state::Status::Failed
                        )
                    })
                    .map(|(rs, t)| {
                        format!(
                            "{}/{} (worktree {:?})",
                            rs.run_id,
                            t.id,
                            t.worktree.as_deref().unwrap_or_default()
                        )
                    })
                    .collect();
            if !live_unusable.is_empty() && cwd.starts_with(&cfg.worktree_base) {
                return undetermined(format!(
                    "cwd {} is under the condukt worktree base {} and non-terminal task(s) {} \
                     record a worktree that is not an absolute path, so whether one of them \
                     owns this cwd is undetermined",
                    cwd.display(),
                    cfg.worktree_base.display(),
                    live_unusable.join(", ")
                ));
            }
            return Decision::Allow;
        }
        [one] => *one,
        many => {
            let ids: Vec<String> = many
                .iter()
                .map(|(rs, t)| format!("{}/{}", rs.run_id, t.id))
                .collect();
            return undetermined(format!(
                "cwd {} is inside the recorded worktree of {} tasks ({}); whose scope applies \
                 is undetermined",
                cwd.display(),
                ids.len(),
                ids.join(", ")
            ));
        }
    };
    use harness_core::verdict::Required;
    let repro_text =
        match crate::state::load_decomposition_determined(&cfg, cwd, &rs.run_id).require() {
            Required::Blocked(v) => {
                return undetermined(format!(
                    "decomposition of run '{}' unreadable: {}",
                    rs.run_id,
                    v.as_str()
                ))
            }
            Required::Determined(None) => None,
            Required::Determined(Some(raw)) => {
                let dec: crate::model::Decomposition = match serde_json::from_str(&raw) {
                    Ok(d) => d,
                    Err(e) => {
                        return undetermined(format!(
                            "decomposition of run '{}' unparseable: {e}",
                            rs.run_id
                        ))
                    }
                };
                match dec.tasks.iter().find(|t| t.id == task.id) {
                    Some(t) => t.reproduction_tests.clone(),
                    None => {
                        return undetermined(format!(
                            "task '{}' is not in run '{}''s decomposition",
                            task.id, rs.run_id
                        ))
                    }
                }
            }
        };
    let repro = match repro_text.as_deref().map(reproduction_selectors) {
        None => Vec::new(),
        Some(Ok(s)) => s,
        Some(Err(e)) => return undetermined(e),
    };
    let wt = PathBuf::from(task.worktree.clone().unwrap_or_default());
    let mut changed: Option<Vec<String>> = None;
    for words in tests {
        let sels = match selectors(words) {
            Ok(s) => s,
            Err(e) => return undetermined(format!("`{}`: {e}", words.join(" "))),
        };
        for sel in sels {
            if repro.contains(&sel) {
                continue;
            }
            let allowed = match &sel {
                Selector::Test(name) => {
                    if changed.is_none() {
                        match worktree_changed_tests(&wt, &cfg.default_branch) {
                            Ok(c) => changed = Some(c),
                            Err(e) => {
                                return undetermined(format!(
                                    "which tests worktree {} changed is unknown: {e}",
                                    wt.display()
                                ))
                            }
                        }
                    }
                    changed.as_ref().is_some_and(|c| c.contains(name))
                }
                Selector::Kind(..) => false,
            };
            if !allowed {
                let what = match &sel {
                    Selector::Test(n) => format!("--test {n}"),
                    Selector::Kind(k, Some(n)) => format!("{k} {n}"),
                    Selector::Kind(k, None) => k.clone(),
                };
                return Decision::Deny(format!(
                    "`{}` selects `{what}`, which is outside task '{}' of run '{}' (worktree {}): \
                     {TASK_SCOPE_REASON}",
                    words.join(" "),
                    task.id,
                    rs.run_id,
                    wt.display()
                ));
            }
        }
    }
    Decision::Allow
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(cmd: &str) -> Vec<String> {
        shlex::split(cmd).unwrap_or_default()
    }

    /// Rule 1 alone (no ledger, no task scope): what `full_suite_reason`
    /// asserted before `state test` moved onto [`judge`].
    #[test]
    fn rule1_flags_bare_cargo_test() {
        let ctx = Ctx {
            wt_root: None,
            cwd_in_wt: false,
            home: None,
        };
        let dir = Dir::Known(PathBuf::from("/r"));
        assert!(matches!(
            judge_words(&words("cargo test"), &dir, &ctx, 0),
            Judged::Deny(_)
        ));
        assert!(matches!(
            judge_words(&words("cargo test -p x --test y"), &dir, &ctx, 0),
            Judged::Test(_)
        ));
    }
}
