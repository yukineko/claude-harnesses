//! Bash classification (SPEC §5.1): does a command run a master-only
//! operation (push / rollout / an operation that moves main) or a
//! master-only `session-master` CLI call?
//!
//! Decided on the brush-parser AST, so strings, comments and heredoc *data*
//! are not mistaken for executed code. The answer is three-valued
//! ([`Determination`]): `Known(None)` = not master-only, `Known(Some(op))` =
//! master-only, `Undetermined` = could not tell, which the gate treats as
//! master-only (the restrictive side).
//!
//! # What "could not tell" covers (0.1.4)
//!
//! Undetermined is raised **without looking at whether the text spells
//! `push`** (verifier round 2, P1): a word that only expansion can reveal may
//! be anything. Concretely:
//!
//! * a command word that is not a literal (`$G $S`, `"$@"`, a glob), except
//!   that `$EDITOR file.txt` (>= 1 argument, all literal, none a master-only
//!   effect word or sensitive program) is allowed (0.1.7);
//! * a write to a GitHub merge/push endpoint that cannot be read statically
//!   (`gh api` / `curl`), and running a file written earlier in the same
//!   program (`cp git /tmp/g && /tmp/g push`) (0.1.7);
//! * a non-literal argument where code is expected: `eval`, `sh -c`, `ssh`'s
//!   remote command, a shell's script operand, `source`, `find -exec`;
//! * a git subcommand that is not literal, unknown, or an alias (P3);
//! * a program that is not known to be inert and whose arguments name git,
//!   a shell, `ssh`, `gh`, `condukt`, `session-master` or `rollout-plugins`
//!   — it may be a wrapper that runs them (P2: `setsid`, `xcrun`, `flock`…);
//! * a parse failure, the nesting / recursion caps, a classifier panic.
//!
//! Known limits (also in README.ja.md): the contents of script files and of
//! programs run by build tools (`make`, `npm`, `cargo` aliases, a python
//! script…) are not read, and an unknown program whose arguments do not name
//! a sensitive program is allowed.

mod rules;
mod web;

use crate::config;
use crate::group;
use brush_parser::ast;
use brush_parser::word::{WordPiece, WordPieceWithSource};
use harness_core::verdict::Determination;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};

/// Nested-program recursion cap (`-c`, `eval`, substitutions, heredocs).
pub const MAX_DEPTH: usize = 8;
/// Pre-parse caps that keep brush-parser's recursion far from the stack
/// limit (brush aborts — not panics — on stack overflow; blastguard measured
/// the first overflow at nesting 128 on a 2 MiB stack, and the classifier
/// runs on a 64 MiB thread).
const MAX_NESTING: usize = 64;
const MAX_BYTES: usize = 1 << 20;
const STACK_BYTES: usize = 64 << 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Push,
    Rollout,
    /// A merge into main, or any other operation that moves a main branch.
    Merge,
    /// A `session-master` CLI call that acts as the master or as another
    /// session (`next` / `complete` / `gate` / `hook`, a `--session` naming
    /// someone else, or a `CLAUDE_CODE_SESSION_ID` assignment). Not queued:
    /// there is nothing for the master to run on the member's behalf.
    Control,
}

impl Op {
    pub fn kind(self) -> &'static str {
        match self {
            Op::Push => "push",
            Op::Rollout => "rollout",
            Op::Merge => "merge",
            Op::Control => "control",
        }
    }
}

/// `session-master` subcommands only the master (or Claude Code itself, for
/// `gate` / `hook`) may run.
pub const CONTROL_SUBCOMMANDS: [&str; 4] = ["next", "complete", "gate", "hook"];

/// The env var the `session-master` CLI reads its identity from.
const SESSION_ENV: &str = "CLAUDE_CODE_SESSION_ID";

/// Env vars that redirect git to another repository / worktree.
const GIT_ENV: [&str; 3] = ["GIT_DIR", "GIT_WORK_TREE", "GIT_COMMON_DIR"];

/// Shell interpreters (`-c STRING`, script operand, or a program on stdin).
const SHELLS: [&str; 10] = [
    "sh", "bash", "zsh", "dash", "ksh", "mksh", "fish", "csh", "tcsh", "ash",
];

/// Programs whose mention as an argument makes an unknown program a
/// possible wrapper (P2).
const SENSITIVE: [&str; 5] = ["git", "gh", "condukt", "ssh", "eval"];

/// Programs known not to execute their arguments as commands. Anything not
/// here and not handled explicitly is "unknown" (see the module doc).
/// Deliberately absent because they can run a command given as an argument:
/// `tdd --cmd`, `trap`, `alias`, `rustup run`, `tar --to-command`,
/// `zip -TT`, `fd -x`, `watch`. `rg` is here but `rg --pre CMD` is judged
/// like an unknown program.
const INERT: &[&str] = &[
    "echo",
    "printf",
    "cat",
    "ls",
    "grep",
    "egrep",
    "fgrep",
    "rg",
    "ag",
    "head",
    "tail",
    "wc",
    "sort",
    "uniq",
    "cut",
    "tr",
    "diff",
    "cmp",
    "comm",
    "file",
    "stat",
    "du",
    "df",
    "pwd",
    "true",
    "false",
    "test",
    "[",
    "basename",
    "dirname",
    "realpath",
    "readlink",
    "which",
    "type",
    "date",
    "mkdir",
    "rmdir",
    "touch",
    "cp",
    "mv",
    "rm",
    "ln",
    "chmod",
    "chown",
    "tee",
    "less",
    "more",
    "jq",
    "yq",
    "sed",
    "tree",
    "bat",
    "cargo",
    "rustc",
    "rustfmt",
    "export",
    "unset",
    "set",
    "read",
    "wait",
    "sleep",
    "kill",
    "ps",
    "pgrep",
    "pkill",
    "curl",
    "wget",
    "unzip",
    "gzip",
    "gunzip",
    "open",
    "mktemp",
    "seq",
    "yes",
    "xxd",
    "od",
    "hexdump",
    "shasum",
    "sha256sum",
    "md5",
    "md5sum",
    "base64",
    "column",
    "paste",
    "join",
    "fold",
    "nl",
    "rev",
    "tac",
    "expr",
    "bc",
    "uname",
    "hostname",
    "id",
    "whoami",
    "printenv",
    "locale",
    "tput",
    "clear",
    "local",
    "declare",
    "typeset",
    "return",
    "exit",
    "shift",
    "break",
    "continue",
    ":",
    "umask",
    "ulimit",
    "hash",
    "history",
    "jobs",
    "fg",
    "bg",
    "disown",
    "getopts",
    "backlog",
    "lsof",
    "top",
    "nproc",
    "cal",
];

/// Script interpreters: inline code / stdin is checked for sensitive
/// programs; a script *file* operand is not read (limit).
const INTERPRETERS: &[&str] = &[
    "python",
    "python3",
    "node",
    "perl",
    "ruby",
    "deno",
    "bun",
    "osascript",
    "awk",
    "gawk",
    "php",
    "lua",
    "Rscript",
];

/// git subcommands that neither push nor move a main branch by themselves.
const GIT_SAFE: &[&str] = &[
    "status",
    "log",
    "diff",
    "show",
    "add",
    "rm",
    "mv",
    "restore",
    "stash",
    "tag",
    "rev-parse",
    "rev-list",
    "ls-files",
    "ls-tree",
    "ls-remote",
    "cat-file",
    "config",
    "blame",
    "grep",
    "describe",
    "shortlog",
    "worktree",
    "remote",
    "clone",
    "init",
    "notes",
    "reflog",
    "apply",
    "format-patch",
    "archive",
    "check-ignore",
    "check-attr",
    "show-ref",
    "symbolic-ref",
    "for-each-ref",
    "merge-base",
    "name-rev",
    "var",
    "help",
    "version",
    "clean",
    "gc",
    "fsck",
    "count-objects",
    "hash-object",
    "diff-tree",
    "diff-files",
    "diff-index",
    "range-diff",
    "sparse-checkout",
    "maintenance",
    "rerere",
    "whatchanged",
    "cherry",
    "show-branch",
    "annotate",
    "bundle",
    "prune",
    "repack",
    "verify-commit",
    "verify-tag",
    "interpret-trailers",
    "stripspace",
    "column",
    "credential",
    "mergetool",
    "difftool",
    "request-pull",
    "write-tree",
    "mktree",
    "commit",
    "checkout",
    "switch",
    "branch",
    "fetch",
    "update-ref",
    "submodule",
    "bisect",
];

/// git subcommands that move HEAD's branch: master-only when HEAD is main.
/// (`commit` is not here: SPEC §5.1 lists `git commit -m ...` as allowed.)
const GIT_MOVES_HEAD: &[&str] = &[
    "merge",
    "pull",
    "reset",
    "cherry-pick",
    "revert",
    "rebase",
    "am",
];

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    Clean,
    Found(Op),
    Undetermined(String),
}

/// Classify `command`, run from `cwd` by session `caller` (`None`: unknown,
/// so any `--session` in a `session-master` call counts as someone else).
pub fn classify(command: &str, cwd: &Path, caller: Option<&str>) -> Determination<Option<Op>> {
    let cmd = command.to_string();
    let dir = cwd.to_path_buf();
    let mains = config::main_branches();
    let me = caller.map(str::to_string);
    let spawned = std::thread::Builder::new()
        .name("session-master-detect".to_string())
        .stack_size(STACK_BYTES)
        .spawn(move || {
            catch_unwind(AssertUnwindSafe(|| {
                analyze(&cmd, &dir, &mains, me.as_deref())
            }))
        });
    let outcome = match spawned {
        Ok(handle) => match handle.join() {
            Ok(Ok(o)) => o,
            Ok(Err(_)) | Err(_) => Outcome::Undetermined("Bash 分類器が panic した".to_string()),
        },
        Err(e) => Outcome::Undetermined(format!("Bash 分類スレッドを起動できない ({e})")),
    };
    match outcome {
        Outcome::Clean => Determination::known(None),
        Outcome::Found(op) => Determination::known(Some(op)),
        Outcome::Undetermined(why) => Determination::undetermined(why),
    }
}

/// One `session-master` invocation seen in the program.
struct SmCall {
    /// `None`: the subcommand is not a literal.
    sub: Option<String>,
    /// `--session` values (`None` entry: not a literal).
    sessions: Vec<Option<String>>,
}

/// Where a HEAD-moving git command acts.
struct Target {
    /// The worktree directory (cwd + `-C`).
    dir: Result<PathBuf, String>,
    /// An explicit git dir (`--git-dir` / `GIT_DIR=`), if given.
    git_dir: Option<Result<PathBuf, String>>,
    /// The cwd at this point depends on a `cd` whose success is not certain
    /// (it ran in an earlier statement, or behind `||`).
    unsure: bool,
}

pub(crate) fn analyze(
    command: &str,
    cwd: &Path,
    mains: &[String],
    caller: Option<&str>,
) -> Outcome {
    let mut w = Walker {
        cwd: cwd.to_path_buf(),
        mains: mains.to_vec(),
        found: None,
        undet: None,
        has_cd: false,
        sets_git_env: false,
        moves: Vec::new(),
        sm_calls: Vec::new(),
        sets_session_env: false,
        cwd_unsure: false,
        written: Vec::new(),
        written_dirs: Vec::new(),
        execs: Vec::new(),
    };
    w.program(command, 0);
    if let Some(op) = w.found {
        return Outcome::Found(op);
    }
    // Running a file this same program wrote or copied is running code that
    // was never read (`cp git /tmp/g && /tmp/g push`).
    let ran_written = w.execs.iter().find(|e| w.was_written(e)).cloned();
    if let Some(e) = ran_written {
        w.set_undet(format!(
            "同じ program 内で書いた/コピーしたファイルを実行している: {e}"
        ));
    }
    for call in std::mem::take(&mut w.sm_calls) {
        let control_sub = match &call.sub {
            Some(s) => CONTROL_SUBCOMMANDS.contains(&s.as_str()),
            // A subcommand we cannot read may be any of them.
            None => true,
        };
        let other_session = call.sessions.iter().any(|s| match (s, caller) {
            (Some(v), Some(me)) => v != me,
            // Non-literal value, or the caller is unknown: cannot show it is
            // the caller's own id.
            _ => true,
        });
        if control_sub || other_session || w.sets_session_env {
            return Outcome::Found(Op::Control);
        }
    }
    for t in std::mem::take(&mut w.moves) {
        if w.has_cd || t.unsure {
            w.set_undet(
                "HEAD を動かす git 操作と同じ program 内に、成否の確定しない cd/pushd/popd/checkout/switch がある"
                    .to_string(),
            );
            continue;
        }
        if w.sets_git_env {
            w.set_undet(
                "HEAD を動かす git 操作と同じ program 内で GIT_DIR 等を設定している".to_string(),
            );
            continue;
        }
        let head = match t.git_dir {
            Some(Ok(g)) => group::head_branch_of_gitdir(&g),
            Some(Err(why)) => Err(why),
            None => match t.dir {
                Ok(d) => group::head_branch(&d),
                Err(why) => Err(why),
            },
        };
        match head {
            Ok(Some(b)) if mains.contains(&b) => return Outcome::Found(Op::Merge),
            Ok(_) => {}
            Err(e) => w.set_undet(format!("対象 worktree の HEAD を読めない: {e}")),
        }
    }
    match w.undet {
        Some(why) => Outcome::Undetermined(why),
        None => Outcome::Clean,
    }
}

/// One shell word: its raw text and, when it is a plain literal, its value.
struct Arg {
    raw: String,
    lit: Option<String>,
}

impl Arg {
    fn new(raw: &str) -> Arg {
        Arg {
            raw: raw.to_string(),
            lit: literal(raw),
        }
    }
}

#[derive(Default)]
struct Stdin {
    /// Literal program texts fed on stdin (heredoc bodies / here-strings).
    bodies: Vec<String>,
    /// Files redirected to stdin (`< f`; `None`: not a literal).
    files: Vec<Option<String>>,
    /// stdin comes from an earlier pipeline stage (contents unknown).
    piped: bool,
}

/// Prefix assignments of one simple command (`NAME=value cmd`).
type Assigns = Vec<(String, Option<String>)>;

struct Walker {
    cwd: PathBuf,
    mains: Vec<String>,
    found: Option<Op>,
    undet: Option<String>,
    has_cd: bool,
    /// `GIT_DIR` & co. are set somewhere other than a command prefix.
    sets_git_env: bool,
    /// HEAD-moving git commands, judged against HEAD after the walk.
    moves: Vec<Target>,
    /// `session-master` CLI calls (judged after the whole program is read).
    sm_calls: Vec<SmCall>,
    /// Somewhere in the program `CLAUDE_CODE_SESSION_ID` is assigned (prefix
    /// assignment, `env`, `export`, ...): a `session-master` call may then run
    /// under another session's identity.
    sets_session_env: bool,
    /// A statement-level `cd /abs && ...` ended: later statements may run in
    /// either directory.
    cwd_unsure: bool,
    /// Literal paths this program writes (cp/mv/ln/tee/dd of=/redirects/
    /// curl -o), raw-normalised; a destination directory also covers files in it.
    written: Vec<String>,
    /// Directories this program filled recursively / from unreadable sources:
    /// anything under them counts as written.
    written_dirs: Vec<String>,
    /// Files this program runs (command word with a `/`, script operand).
    execs: Vec<String>,
}

/// `cd /literal/abs/path` as a whole pipeline stage (no options, redirects,
/// assignments or pipe): the directory it changes to.
fn simple_abs_cd(p: &ast::Pipeline) -> Option<String> {
    if p.seq.len() != 1 || p.bang {
        return None;
    }
    let ast::Command::Simple(sc) = &p.seq[0] else {
        return None;
    };
    if sc.prefix.is_some() {
        return None;
    }
    if literal(&sc.word_or_name.as_ref()?.value)? != "cd" {
        return None;
    }
    let items = &sc.suffix.as_ref()?.0;
    if items.len() != 1 {
        return None;
    }
    let ast::CommandPrefixOrSuffixItem::Word(w) = &items[0] else {
        return None;
    };
    let dir = literal(&w.value)?;
    dir.starts_with('/').then_some(dir)
}

fn opts() -> brush_parser::ParserOptions {
    brush_parser::ParserOptions::default()
}

fn nesting(text: &str) -> usize {
    let mut depth = 0usize;
    let mut max = 0usize;
    for c in text.bytes() {
        match c {
            b'(' | b'{' | b'[' => {
                depth += 1;
                max = max.max(depth);
            }
            b')' | b'}' | b']' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    let keywords = text
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| matches!(*w, "if" | "while" | "until" | "for" | "case" | "select"))
        .count();
    max.max(keywords)
}

fn parse_word(raw: &str) -> Option<Vec<WordPieceWithSource>> {
    match catch_unwind(AssertUnwindSafe(|| brush_parser::word::parse(raw, &opts()))) {
        Ok(Ok(p)) => Some(p),
        _ => None,
    }
}

/// The literal value of a word, or `None` when expansion is involved.
fn literal(raw: &str) -> Option<String> {
    let pieces = parse_word(raw)?;
    let mut out = String::new();
    if literal_pieces(&pieces, &mut out, false) {
        Some(out)
    } else {
        None
    }
}

fn literal_pieces(pieces: &[WordPieceWithSource], out: &mut String, quoted: bool) -> bool {
    for p in pieces {
        match &p.piece {
            WordPiece::Text(t) => {
                // Unquoted glob / brace-expansion characters may rewrite the word.
                let expands = !quoted
                    && (t.contains(['*', '?', '['])
                        || (t.contains('{') && (t.contains(',') || t.contains(".."))));
                if expands {
                    return false;
                }
                out.push_str(t);
            }
            WordPiece::SingleQuotedText(t) => out.push_str(t),
            WordPiece::AnsiCQuotedText(t) => {
                if t.contains('\\') {
                    return false;
                }
                out.push_str(t);
            }
            WordPiece::EscapeSequence(s) => {
                let body = s.strip_prefix('\\').unwrap_or(s);
                if body != "\n" {
                    out.push_str(body);
                }
            }
            WordPiece::DoubleQuotedSequence(seq) | WordPiece::GettextDoubleQuotedSequence(seq) => {
                if !literal_pieces(seq, out, true) {
                    return false;
                }
            }
            WordPiece::TildeExpansion(_)
            | WordPiece::ParameterExpansion(_)
            | WordPiece::CommandSubstitution(_)
            | WordPiece::BackquotedCommandSubstitution(_)
            | WordPiece::ArithmeticExpression(_) => return false,
        }
    }
    true
}

fn basename(s: &str) -> &str {
    s.rsplit('/').next().unwrap_or(s)
}

fn unescape_backquote(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\\' {
            if let Some(&n) = it.peek() {
                if matches!(n, '\\' | '`' | '$') {
                    out.push(n);
                    it.next();
                    continue;
                }
            }
        }
        out.push(c);
    }
    out
}

/// Does `text` name a sensitive program (as a token)?
fn mentions_sensitive(text: &str) -> Option<String> {
    for tok in text.split(|c: char| !(c.is_ascii_alphanumeric() || "_.-/+".contains(c))) {
        if tok.is_empty() {
            continue;
        }
        let b = basename(tok);
        if SENSITIVE.contains(&b)
            || SHELLS.contains(&b)
            || b.starts_with("session-master")
            || tok.contains("rollout-plugins")
        {
            return Some(tok.to_string());
        }
    }
    None
}

fn joined(args: &[Arg]) -> String {
    args.iter()
        .map(|a| a.raw.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}

impl Walker {
    fn set_undet(&mut self, why: String) {
        if self.undet.is_none() {
            self.undet = Some(why);
        }
    }

    /// Undetermined, unconditionally (P1: never gated on the text).
    fn undet(&mut self, text: &str, why: &str) {
        let shown: String = text.chars().take(120).collect();
        self.set_undet(format!("{why}: {shown}"));
    }

    fn found(&mut self, op: Op) {
        if self.found.is_none() {
            self.found = Some(op);
        }
    }

    fn is_main_ref(&self, r: &str) -> bool {
        let r = r.trim_start_matches('+');
        let r = r.strip_prefix("refs/heads/").unwrap_or(r);
        self.mains.iter().any(|m| m == r)
    }

    fn program(&mut self, src: &str, depth: usize) {
        if depth > MAX_DEPTH {
            self.undet(src, "再帰深さ上限を超えた");
            return;
        }
        if src.len() > MAX_BYTES || nesting(src) > MAX_NESTING {
            self.undet(src, "構文が大きすぎる/深すぎるので解析しない");
            return;
        }
        let parsed = catch_unwind(AssertUnwindSafe(|| {
            brush_parser::Parser::new(src.as_bytes(), &opts()).parse_program()
        }));
        let prog = match parsed {
            Ok(Ok(p)) => p,
            Ok(Err(_)) | Err(_) => {
                self.undet(src, "構文解析に失敗した");
                return;
            }
        };
        for cc in &prog.complete_commands {
            self.list(cc, src, depth);
        }
    }

    fn list(&mut self, l: &ast::CompoundList, src: &str, depth: usize) {
        for item in &l.0 {
            self.and_or(&item.0, src, depth);
        }
    }

    /// One `a && b || c ...` chain. A statement `cd /abs/path` that is
    /// followed by `&&` (and reached only through `&&`) definitely changes the
    /// directory for the `&&`-connected commands after it, so a HEAD-moving
    /// git there is judged against that directory. Everything else about
    /// `cd` (relative / `-` / bare / piped / after `||` / `;`) stays
    /// `has_cd` or `cwd_unsure`, i.e. undetermined.
    fn and_or(&mut self, aol: &ast::AndOrList, src: &str, depth: usize) {
        let saved = self.cwd.clone();
        let mut elems: Vec<&ast::Pipeline> = vec![&aol.first];
        let mut ors: Vec<bool> = vec![false];
        for a in &aol.additional {
            match a {
                ast::AndOr::And(p) => {
                    elems.push(p);
                    ors.push(false);
                }
                ast::AndOr::Or(p) => {
                    elems.push(p);
                    ors.push(true);
                }
            }
        }
        let mut tracked = false;
        let mut cd_seen = false;
        for (k, p) in elems.iter().enumerate() {
            // `ors[k]`: this element is reached through `||`.
            let reached_by_and_only = !ors[..=k].iter().any(|o| *o);
            let followed_by_and = ors.get(k + 1).is_some_and(|o| !*o);
            if let Some(dir) = simple_abs_cd(p) {
                if reached_by_and_only && followed_by_and {
                    self.cwd = PathBuf::from(dir);
                    tracked = true;
                    cd_seen = true;
                    continue;
                }
            }
            if tracked && ors[k] {
                // Reached through `||` after a tracked cd: either directory.
                self.cwd = saved.clone();
                tracked = false;
                self.cwd_unsure = true;
            }
            self.pipeline(p, src, depth);
        }
        self.cwd = saved;
        if cd_seen {
            self.cwd_unsure = true;
        }
    }

    fn pipeline(&mut self, p: &ast::Pipeline, src: &str, depth: usize) {
        for (i, c) in p.seq.iter().enumerate() {
            self.command(c, src, depth, i > 0);
        }
    }

    fn command(&mut self, c: &ast::Command, src: &str, depth: usize, piped: bool) {
        match c {
            ast::Command::Simple(sc) => self.simple(sc, src, depth, piped),
            ast::Command::Compound(cc, redirs) => {
                self.compound(cc, src, depth);
                self.redirect_list(redirs.as_ref(), depth);
            }
            ast::Command::Function(fd) => {
                self.compound(&fd.body.0, src, depth);
                self.redirect_list(fd.body.1.as_ref(), depth);
            }
            ast::Command::ExtendedTest(e, redirs) => {
                self.ext_test(&e.expr, depth);
                self.redirect_list(redirs.as_ref(), depth);
            }
        }
    }

    fn ext_test(&mut self, e: &ast::ExtendedTestExpr, depth: usize) {
        match e {
            ast::ExtendedTestExpr::And(a, b) | ast::ExtendedTestExpr::Or(a, b) => {
                self.ext_test(a, depth);
                self.ext_test(b, depth);
            }
            ast::ExtendedTestExpr::Not(a) | ast::ExtendedTestExpr::Parenthesized(a) => {
                self.ext_test(a, depth)
            }
            ast::ExtendedTestExpr::UnaryTest(_, w) => self.scan_word(&w.value, depth),
            ast::ExtendedTestExpr::BinaryTest(_, a, b) => {
                self.scan_word(&a.value, depth);
                self.scan_word(&b.value, depth);
            }
        }
    }

    fn compound(&mut self, cc: &ast::CompoundCommand, src: &str, depth: usize) {
        use ast::CompoundCommand as C;
        match cc {
            C::Arithmetic(a) => self.scan_word(&a.expr.value, depth),
            C::ArithmeticForClause(f) => {
                for e in [&f.initializer, &f.condition, &f.updater]
                    .into_iter()
                    .flatten()
                {
                    self.scan_word(&e.value, depth);
                }
                self.list(&f.body.list, src, depth);
            }
            C::BraceGroup(b) => self.list(&b.list, src, depth),
            C::Subshell(s) => self.list(&s.list, src, depth),
            C::ForClause(f) => {
                for w in f.values.iter().flatten() {
                    self.scan_word(&w.value, depth);
                }
                self.list(&f.body.list, src, depth);
            }
            C::CaseClause(c) => {
                self.scan_word(&c.value.value, depth);
                for item in &c.cases {
                    for p in &item.patterns {
                        self.scan_word(&p.value, depth);
                    }
                    if let Some(l) = &item.cmd {
                        self.list(l, src, depth);
                    }
                }
            }
            C::IfClause(i) => {
                self.list(&i.condition, src, depth);
                self.list(&i.then, src, depth);
                for e in i.elses.iter().flatten() {
                    if let Some(c) = &e.condition {
                        self.list(c, src, depth);
                    }
                    self.list(&e.body, src, depth);
                }
            }
            C::WhileClause(w) | C::UntilClause(w) => {
                self.list(&w.0, src, depth);
                self.list(&w.1.list, src, depth);
            }
            C::Coprocess(c) => self.command(&c.body, src, depth, false),
        }
    }

    fn redirect_list(&mut self, r: Option<&ast::RedirectList>, depth: usize) {
        let mut sink = Stdin::default();
        for io in r.iter().flat_map(|l| l.0.iter()) {
            self.redirect(io, depth, &mut sink);
        }
    }

    fn redirect(&mut self, io: &ast::IoRedirect, depth: usize, stdin: &mut Stdin) {
        match io {
            ast::IoRedirect::File(fd, kind, target) => match target {
                ast::IoFileRedirectTarget::Filename(w) => {
                    self.scan_word(&w.value, depth);
                    let to_stdin = matches!(fd, None | Some(0))
                        && matches!(
                            kind,
                            ast::IoFileRedirectKind::Read | ast::IoFileRedirectKind::ReadAndWrite
                        );
                    if to_stdin {
                        stdin.files.push(literal(&w.value));
                    }
                    if matches!(
                        kind,
                        ast::IoFileRedirectKind::Write
                            | ast::IoFileRedirectKind::Append
                            | ast::IoFileRedirectKind::Clobber
                            | ast::IoFileRedirectKind::ReadAndWrite
                    ) {
                        let t = literal(&w.value).unwrap_or_else(|| w.value.clone());
                        let p = self.np(&t);
                        self.written.push(p);
                    }
                }
                ast::IoFileRedirectTarget::Duplicate(w) => self.scan_word(&w.value, depth),
                ast::IoFileRedirectTarget::ProcessSubstitution(_, sub) => {
                    self.list(&sub.list, "", depth)
                }
                ast::IoFileRedirectTarget::Fd(_) => {}
            },
            ast::IoRedirect::HereDocument(_, hd) => {
                if hd.requires_expansion {
                    self.scan_heredoc(&hd.doc.value, depth);
                }
                stdin.bodies.push(hd.doc.value.clone());
            }
            ast::IoRedirect::HereString(_, w) => {
                self.scan_word(&w.value, depth);
                match literal(&w.value) {
                    Some(s) => stdin.bodies.push(s),
                    None => stdin.bodies.push(w.value.clone()),
                }
            }
            ast::IoRedirect::OutputAndError(w, _) => self.scan_word(&w.value, depth),
        }
    }

    /// Walk command substitutions hidden inside a word.
    fn scan_word(&mut self, raw: &str, depth: usize) {
        if !raw.contains("$(") && !raw.contains('`') && !raw.contains("<(") && !raw.contains(">(") {
            return;
        }
        match parse_word(raw) {
            Some(pieces) => self.walk_pieces(&pieces, raw, depth),
            None => self.undet(raw, "word を解析できない"),
        }
    }

    fn scan_heredoc(&mut self, body: &str, depth: usize) {
        if !body.contains("$(") && !body.contains('`') {
            return;
        }
        let parsed = catch_unwind(AssertUnwindSafe(|| {
            brush_parser::word::parse_heredoc(body, &opts())
        }));
        match parsed {
            Ok(Ok(pieces)) => self.walk_pieces(&pieces, body, depth),
            _ => self.undet(body, "heredoc 本文を解析できない"),
        }
    }

    fn walk_pieces(&mut self, pieces: &[WordPieceWithSource], raw: &str, depth: usize) {
        for p in pieces {
            match &p.piece {
                WordPiece::CommandSubstitution(s) => self.program(s, depth + 1),
                WordPiece::BackquotedCommandSubstitution(s) => {
                    self.program(&unescape_backquote(s), depth + 1)
                }
                WordPiece::DoubleQuotedSequence(seq)
                | WordPiece::GettextDoubleQuotedSequence(seq) => self.walk_pieces(seq, raw, depth),
                WordPiece::ParameterExpansion(_) => {
                    let slice = raw.get(p.start_index..p.end_index).unwrap_or(raw);
                    if slice.contains("$(") || slice.contains('`') {
                        self.undet(slice, "パラメータ展開の中のコマンド置換は解析しない");
                    }
                }
                WordPiece::ArithmeticExpression(e) => {
                    if e.value != raw {
                        self.scan_word(&e.value, depth);
                    }
                }
                WordPiece::Text(_)
                | WordPiece::SingleQuotedText(_)
                | WordPiece::AnsiCQuotedText(_)
                | WordPiece::EscapeSequence(_)
                | WordPiece::TildeExpansion(_) => {}
            }
        }
    }

    fn simple(&mut self, sc: &ast::SimpleCommand, src: &str, depth: usize, piped: bool) {
        let mut words: Vec<&ast::Word> = Vec::new();
        let mut assigns: Assigns = Vec::new();
        let mut stdin = Stdin {
            piped,
            ..Stdin::default()
        };
        let items = sc
            .prefix
            .iter()
            .flat_map(|p| p.0.iter())
            .map(|i| (i, true))
            .chain(
                sc.suffix
                    .iter()
                    .flat_map(|s| s.0.iter())
                    .map(|i| (i, false)),
            );
        let mut name_pushed = false;
        for (item, is_prefix) in items {
            if !is_prefix && !name_pushed {
                if let Some(w) = &sc.word_or_name {
                    words.push(w);
                }
                name_pushed = true;
            }
            match item {
                ast::CommandPrefixOrSuffixItem::IoRedirect(r) => {
                    self.redirect(r, depth, &mut stdin)
                }
                ast::CommandPrefixOrSuffixItem::Word(w) => words.push(w),
                ast::CommandPrefixOrSuffixItem::AssignmentWord(_, w) => {
                    if is_prefix {
                        self.scan_word(&w.value, depth);
                        let name = w.value.split('=').next().unwrap_or("").to_string();
                        if name == SESSION_ENV {
                            self.sets_session_env = true;
                        }
                        let value = literal(&w.value)
                            .and_then(|s| s.split_once('=').map(|(_, v)| v.to_string()));
                        self.exec_env(&name, value.as_deref(), depth);
                        assigns.push((name, value));
                    } else {
                        words.push(w);
                    }
                }
                ast::CommandPrefixOrSuffixItem::ProcessSubstitution(_, sub) => {
                    self.list(&sub.list, src, depth)
                }
            }
        }
        if !name_pushed {
            if let Some(w) = &sc.word_or_name {
                words.push(w);
            }
        }
        let sets_env = words
            .first()
            .and_then(|w| literal(&w.value))
            .is_some_and(|c| {
                matches!(
                    basename(&c),
                    "export" | "declare" | "typeset" | "local" | "readonly" | "env"
                )
            });
        for w in &words {
            if sets_env {
                self.exec_env_word(&w.value, depth);
            }
            // `env X=..`, `export X=..`, `declare X=..`: restrictive textual
            // checks; they only matter when a session-master / HEAD-moving
            // git call exists in the same program.
            if w.value.contains(&format!("{SESSION_ENV}=")) {
                self.sets_session_env = true;
            }
            if GIT_ENV.iter().any(|g| w.value.contains(&format!("{g}="))) {
                self.sets_git_env = true;
            }
            self.scan_word(&w.value, depth);
        }
        let argv: Vec<Arg> = words.iter().map(|w| Arg::new(&w.value)).collect();
        self.exec(&argv, &stdin, &assigns, src, depth);
    }

    /// Strip wrappers, then judge the command that actually runs.
    fn exec(&mut self, argv: &[Arg], stdin: &Stdin, assigns: &Assigns, src: &str, depth: usize) {
        let mut i = 0usize;
        let mut via_xargs = false;
        loop {
            let Some(a) = argv.get(i) else {
                return;
            };
            let Some(name) = a.lit.as_deref() else {
                self.non_literal_command(&argv[i..], depth);
                return;
            };
            let next = match basename(name) {
                "env" => match self.skip_env(argv, i + 1, depth) {
                    Some(n) => n,
                    None => return,
                },
                "command" => {
                    let mut j = i + 1;
                    while let Some(o) = argv.get(j).and_then(|x| x.lit.as_deref()) {
                        match o {
                            "-v" | "-V" => return,
                            "-p" => j += 1,
                            "--" => {
                                j += 1;
                                break;
                            }
                            _ => break,
                        }
                    }
                    j
                }
                "exec" => skip_opts(argv, i + 1, &["-a"]),
                "builtin" | "nohup" | "busybox" => i + 1,
                "time" => skip_opts(argv, i + 1, &["-f", "-o"]),
                "nice" => {
                    let mut j = i + 1;
                    while let Some(o) = argv.get(j).and_then(|x| x.lit.as_deref()) {
                        if o == "-n" || o == "--adjustment" {
                            j += 2;
                        } else if o.starts_with('-') && o.len() > 1 {
                            j += 1;
                        } else {
                            break;
                        }
                    }
                    j
                }
                "timeout" => {
                    let j = skip_opts(argv, i + 1, &["-s", "-k", "--signal", "--kill-after"]);
                    j + 1
                }
                "sudo" => skip_opts(
                    argv,
                    i + 1,
                    &[
                        "-u", "-g", "-h", "-p", "-C", "-D", "-r", "-t", "-U", "-T", "-R",
                    ],
                ),
                "xargs" => {
                    via_xargs = true;
                    skip_opts(
                        argv,
                        i + 1,
                        &[
                            "-I", "-L", "-n", "-P", "-s", "-d", "-E", "-a", "-J", "-R", "-S",
                        ],
                    )
                }
                _ => break,
            };
            if argv[i + 1..next.min(argv.len())]
                .iter()
                .any(|x| x.lit.is_none())
            {
                self.undet(&joined(&argv[i..]), "wrapper の option が literal でない");
            }
            i = next;
        }
        let rest = &argv[i..];
        let Some(name) = rest.first().and_then(|a| a.lit.as_deref()) else {
            return;
        };
        let args = &rest[1..];
        let base = basename(name);
        if name.contains('/') {
            let p = self.np(name);
            self.execs.push(p);
        }
        self.note_writer(base, args);
        match base {
            "git" => self.git(args, via_xargs, assigns, src, depth),
            "cd" | "pushd" | "popd" => self.has_cd = true,
            "rollout-plugins.sh" => self.found(Op::Rollout),
            b if SHELLS.contains(&b) => self.shell(args, stdin, depth),
            "source" | "." => self.script_operand(args.first()),
            "eval" => match literal_all(args) {
                Some(text) => self.program(&text, depth + 1),
                None => self.undet(&joined(rest), "eval の引数が literal でない"),
            },
            "condukt" => {
                if positionals(args, &[]).starts_with(&["worktree", "merge"]) {
                    self.found(Op::Merge)
                }
            }
            "gh" => self.gh(args, rest),
            "man" => self.man(args, depth),
            "docker" | "podman" => self.docker(base, args, rest, src, depth),
            "curl" | "wget" | "http" | "https" | "xh" | "xhs" => self.web_client(base, args, rest),
            "ssh" => self.ssh(args, depth),
            "find" => self.find(args, src, depth),
            b if b.starts_with("session-master") => self.sm_call(args),
            "rg" if args
                .iter()
                .any(|a| a.lit.as_deref().is_none_or(|s| s.starts_with("--pre"))) =>
            {
                self.undet(
                    &joined(rest),
                    "rg --pre はコマンドを実行する（または引数が literal でない）",
                )
            }
            b if INERT.contains(&b) => {}
            b if INTERPRETERS.contains(&b) => self.interpreter(args, stdin),
            _ => self.unknown_program(base, args, rest),
        }
    }

    /// `env [opts] [NAME=VAL...] cmd`. `-S STRING` is a nested program.
    fn skip_env(&mut self, argv: &[Arg], mut j: usize, depth: usize) -> Option<usize> {
        while let Some(a) = argv.get(j) {
            let Some(s) = a.lit.as_deref() else {
                return Some(j);
            };
            if s == "--" {
                return Some(j + 1);
            }
            if s == "-S" || s == "--split-string" {
                if let Some(n) = argv.get(j + 1) {
                    match &n.lit {
                        Some(body) => {
                            let mut text = body.clone();
                            for extra in &argv[j + 2..] {
                                text.push(' ');
                                text.push_str(&extra.raw);
                            }
                            self.program(&text, depth + 1);
                        }
                        None => self.undet(&joined(&argv[j..]), "env -S の引数が literal でない"),
                    }
                }
                return None;
            }
            if let Some(body) = s
                .strip_prefix("--split-string=")
                .or_else(|| s.strip_prefix("-S"))
            {
                let mut text = body.to_string();
                for extra in &argv[j + 1..] {
                    text.push(' ');
                    text.push_str(&extra.raw);
                }
                self.program(&text, depth + 1);
                return None;
            }
            if matches!(s, "-u" | "-C" | "--unset" | "--chdir" | "-P") {
                j += 2;
            } else if s.starts_with('-') || (s.contains('=') && !s.starts_with('=')) {
                j += 1;
            } else {
                return Some(j);
            }
        }
        Some(j)
    }

    /// A script file operand (`source f`, `bash f`, `sh < f`). Only its name
    /// is judged; its contents are not read (limit).
    fn script_operand(&mut self, a: Option<&Arg>) {
        let Some(a) = a else {
            return;
        };
        let name = match a.lit.as_deref() {
            Some(s) => Some(basename(s).to_string()),
            None => literal_tail(&a.raw),
        };
        let p = self.np(a.lit.as_deref().unwrap_or(&a.raw));
        self.execs.push(p);
        match name.as_deref() {
            Some("rollout-plugins.sh") => self.found(Op::Rollout),
            Some(_) => {}
            None => self.undet(
                &a.raw,
                "script の指定が literal でない（展開・glob を含む）",
            ),
        }
    }

    /// `ssh [opts] host [command...]`: the remote command is a nested program
    /// (`ssh host git push` pushes from the remote side).
    fn ssh(&mut self, args: &[Arg], depth: usize) {
        const WITH_ARG: [&str; 19] = [
            "-b", "-c", "-D", "-E", "-e", "-F", "-I", "-i", "-J", "-L", "-l", "-m", "-O", "-o",
            "-p", "-Q", "-R", "-S", "-W",
        ];
        let mut j = skip_opts(args, 0, &WITH_ARG);
        if args.get(j).is_none() {
            return;
        }
        j += 1; // host
        let remote = &args[j.min(args.len())..];
        if remote.is_empty() {
            return;
        }
        match literal_all(remote) {
            Some(text) => self.program(&text, depth + 1),
            None => self.undet(&joined(remote), "ssh のリモートコマンドが literal でない"),
        }
    }

    /// `find ... -exec CMD ... ;` runs CMD.
    fn find(&mut self, args: &[Arg], src: &str, depth: usize) {
        let mut j = 0usize;
        while j < args.len() {
            let is_exec = matches!(
                args[j].lit.as_deref(),
                Some("-exec" | "-execdir" | "-ok" | "-okdir")
            );
            if !is_exec {
                j += 1;
                continue;
            }
            let start = j + 1;
            let mut end = start;
            while end < args.len() && !matches!(args[end].lit.as_deref(), Some(";" | "+")) {
                end += 1;
            }
            if depth + 1 > MAX_DEPTH {
                self.undet(&joined(&args[start..end]), "再帰深さ上限を超えた");
                return;
            }
            self.exec(
                &args[start..end],
                &Stdin::default(),
                &Vec::new(),
                src,
                depth + 1,
            );
            j = end + 1;
        }
    }

    /// Inline code (`-c` / `-e`, stdin bodies) of a script interpreter is
    /// checked for sensitive programs; script files are not read (limit).
    fn interpreter(&mut self, args: &[Arg], stdin: &Stdin) {
        let mut code_next = false;
        let mut k = 0usize;
        while let Some(a) = args.get(k) {
            match a.lit.as_deref() {
                Some(s) if s.starts_with('-') => {
                    k += if matches!(s, "-c" | "-e" | "-E" | "--eval" | "-p" | "-m") {
                        2
                    } else {
                        1
                    };
                }
                _ => {
                    let p = self.np(a.lit.as_deref().unwrap_or(&a.raw));
                    self.execs.push(p);
                    break;
                }
            }
        }
        for a in args {
            match a.lit.as_deref() {
                Some(s) => {
                    if let Some(tok) = mentions_sensitive(s) {
                        self.undet(s, &format!("インタプリタのコードが {tok} を呼ぶ可能性"));
                        return;
                    }
                    code_next = matches!(s, "-c" | "-e" | "-E" | "--eval" | "-p");
                }
                None => {
                    if code_next {
                        self.undet(&a.raw, "インタプリタに渡すコードが literal でない");
                        return;
                    }
                }
            }
        }
        for b in &stdin.bodies {
            if let Some(tok) = mentions_sensitive(b) {
                self.undet(b, &format!("インタプリタの stdin が {tok} を呼ぶ可能性"));
                return;
            }
        }
    }

    /// Record a `session-master` CLI call (judged in [`analyze`]).
    fn sm_call(&mut self, args: &[Arg]) {
        let sub = match args.first() {
            Some(a) => a.lit.clone(),
            None => Some(String::new()),
        };
        let mut sessions = Vec::new();
        for (k, a) in args.iter().enumerate() {
            if a.lit.as_deref() == Some("--session") {
                sessions.push(args.get(k + 1).and_then(|v| v.lit.clone()));
            }
        }
        self.sm_calls.push(SmCall { sub, sessions });
    }

    fn git(&mut self, args: &[Arg], via_xargs: bool, assigns: &Assigns, src: &str, depth: usize) {
        let mut j = 0usize;
        let mut dir: Result<PathBuf, String> = Ok(self.cwd.clone());
        let mut git_dir: Option<Result<String, String>> = None;
        for (name, value) in assigns {
            if name == "GIT_DIR" {
                git_dir = Some(match value {
                    Some(v) => Ok(v.clone()),
                    None => Err("GIT_DIR が literal でない".to_string()),
                });
            } else if GIT_ENV.contains(&name.as_str()) {
                self.sets_git_env = true;
            }
        }
        while let Some(a) = args.get(j) {
            let Some(s) = a.lit.as_deref() else {
                // The program is git, so a non-literal word here may expand
                // to anything (`X=push; git $X`, `git "$@"`).
                self.undet(&a.raw, "git の subcommand / option が literal でない");
                return;
            };
            if let Some(v) = s.strip_prefix("--git-dir=") {
                git_dir = Some(Ok(v.to_string()));
                j += 1;
                continue;
            }
            match s {
                "-C" => {
                    match args.get(j + 1).map(|x| x.lit.clone()) {
                        Some(Some(p)) => {
                            if let Ok(d) = &dir {
                                dir = Ok(d.join(p));
                            }
                        }
                        Some(None) => dir = Err("git -C の引数が literal でない".to_string()),
                        None => {}
                    }
                    j += 2;
                }
                "-c" => {
                    match args.get(j + 1).and_then(|v| v.lit.as_deref()) {
                        Some(v) if !v.starts_with("alias.") => {}
                        _ => {
                            let raw = args.get(j + 1).map_or("", |v| v.raw.as_str());
                            self.undet(raw, "git -c の alias / 非 literal 設定");
                            return;
                        }
                    }
                    j += 2;
                }
                "--git-dir" => {
                    git_dir = Some(match args.get(j + 1).and_then(|x| x.lit.clone()) {
                        Some(v) => Ok(v),
                        None => Err("--git-dir の引数が literal でない".to_string()),
                    });
                    j += 2;
                }
                "--work-tree" | "--namespace" | "--config-env" | "--super-prefix"
                | "--list-cmds" => j += 2,
                _ if s.starts_with('-') => j += 1,
                _ => break,
            }
        }
        let Some(sub) = args.get(j).and_then(|a| a.lit.as_deref()) else {
            if via_xargs {
                self.undet(src, "xargs から git の subcommand が渡る");
            }
            return;
        };
        let rest = &args[j + 1..];
        match sub {
            "push" | "send-pack" | "http-push" => self.found(Op::Push),
            s if GIT_MOVES_HEAD.contains(&s) => {
                let gd = git_dir.map(|g| {
                    g.map(|p| match &dir {
                        Ok(d) => d.join(p),
                        Err(_) => PathBuf::from(p),
                    })
                });
                self.moves.push(Target {
                    dir,
                    git_dir: gd,
                    unsure: self.cwd_unsure,
                });
            }
            "checkout" | "switch" => self.has_cd = true,
            "update-ref" => self.git_update_ref(rest),
            "branch" => self.git_branch(rest),
            "fetch" => self.git_fetch(rest),
            "submodule" | "bisect" => self.git_runs_command(sub, rest, depth),
            s if GIT_SAFE.contains(&s) => {}
            other => self.undet(other, "git の未知の subcommand（alias の可能性）"),
        }
    }

    fn git_update_ref(&mut self, rest: &[Arg]) {
        for a in rest {
            match a.lit.as_deref() {
                Some("--stdin") => {
                    self.undet("--stdin", "git update-ref --stdin の対象 ref は読めない");
                    return;
                }
                Some("-m") => {}
                Some(s) if s.starts_with('-') => {}
                Some(s) => {
                    if self.is_main_ref(s) {
                        self.found(Op::Merge);
                    }
                    return;
                }
                None => {
                    self.undet(&a.raw, "git update-ref の ref が literal でない");
                    return;
                }
            }
        }
    }

    fn git_branch(&mut self, rest: &[Arg]) {
        let moves = rest.iter().any(|a| {
            matches!(
                a.lit.as_deref(),
                Some(
                    "-f" | "--force"
                        | "-m"
                        | "-M"
                        | "--move"
                        | "-d"
                        | "-D"
                        | "--delete"
                        | "-c"
                        | "-C"
                        | "--copy"
                )
            )
        });
        if !moves {
            return;
        }
        for a in rest {
            match a.lit.as_deref() {
                Some(s) if !s.starts_with('-') && self.is_main_ref(s) => {
                    self.found(Op::Merge);
                    return;
                }
                Some(_) => {}
                None => {
                    self.undet(&a.raw, "git branch -f/-m/-d の対象が literal でない");
                    return;
                }
            }
        }
    }

    fn git_fetch(&mut self, rest: &[Arg]) {
        for a in rest {
            match a.lit.as_deref() {
                Some(s) => {
                    if let Some((_, dst)) = s.split_once(':') {
                        if self.is_main_ref(dst) {
                            self.found(Op::Merge);
                            return;
                        }
                    }
                }
                None => {
                    self.undet(
                        &a.raw,
                        "git fetch の引数が literal でない（refspec が main を指す可能性）",
                    );
                    return;
                }
            }
        }
    }

    /// `git submodule foreach CMD` / `git bisect run CMD` run CMD.
    fn git_runs_command(&mut self, sub: &str, rest: &[Arg], depth: usize) {
        let marker = if sub == "submodule" { "foreach" } else { "run" };
        let Some(k) = rest.iter().position(|a| a.lit.as_deref() == Some(marker)) else {
            return;
        };
        let cmd = &rest[k + 1..];
        let start = cmd
            .iter()
            .position(|a| !matches!(a.lit.as_deref(), Some(s) if s.starts_with('-')))
            .unwrap_or(cmd.len());
        match literal_all(&cmd[start..]) {
            Some(text) => self.program(&text, depth + 1),
            None => self.undet(
                &joined(cmd),
                "git submodule foreach / bisect run の引数が literal でない",
            ),
        }
    }

    fn shell(&mut self, args: &[Arg], stdin: &Stdin, depth: usize) {
        let mut j = 0usize;
        let mut has_c = false;
        let mut reads_stdin = false;
        while let Some(a) = args.get(j) {
            let Some(s) = a.lit.as_deref() else {
                break;
            };
            if s == "--" || s == "-" {
                j += 1;
                break;
            }
            if matches!(
                s,
                "-o" | "+o" | "-O" | "+O" | "--rcfile" | "--init-file" | "-C" | "--command"
            ) {
                if s == "-C" || s == "--command" {
                    // fish: -C / --command take the code.
                    has_c = true;
                    j += 1;
                    break;
                }
                j += 2;
                continue;
            }
            if s.starts_with("--") {
                j += 1;
                continue;
            }
            if (s.starts_with('-') || s.starts_with('+')) && s.len() > 1 {
                if s.starts_with('-') && s[1..].contains('c') {
                    has_c = true;
                }
                if s.starts_with('-') && s[1..].contains('s') {
                    reads_stdin = true;
                }
                j += 1;
                continue;
            }
            break;
        }
        if has_c {
            if let Some(a) = args.get(j) {
                match &a.lit {
                    Some(body) => self.program(body, depth + 1),
                    None => self.undet(&a.raw, "shell -c の引数が literal でない"),
                }
            }
            return;
        }
        if !reads_stdin {
            if let Some(a) = args.get(j) {
                self.script_operand(Some(a));
                return;
            }
        }
        // The program comes from stdin.
        for f in &stdin.files {
            match f {
                Some(name) => self.script_operand(Some(&Arg {
                    raw: name.clone(),
                    lit: Some(name.clone()),
                })),
                None => self.undet("<", "shell の stdin のファイル名が literal でない"),
            }
        }
        for b in &stdin.bodies {
            self.program(b, depth + 1);
        }
        if stdin.bodies.is_empty() && stdin.files.is_empty() && stdin.piped {
            self.undet("|", "pipeline から shell に渡る stdin の中身は読めない");
        }
    }
}

/// The file name of a path word whose *last component* is literal even
/// though an earlier part expands (`"$HOME/.cargo/env"` -> `env`). `None`
/// when the last component itself may expand (`scripts/roll*`, `$f`).
fn literal_tail(raw: &str) -> Option<String> {
    let (_, tail) = raw.rsplit_once('/')?;
    let tail: String = tail.chars().filter(|c| !matches!(c, '"' | '\'')).collect();
    let expands = tail.is_empty()
        || tail.contains(['$', '`', '\\', '*', '?', '[', ']', '{', '}', '(', ')', '~']);
    if expands {
        None
    } else {
        Some(tail)
    }
}

/// All args joined with spaces, when every one is a literal.
fn literal_all(args: &[Arg]) -> Option<String> {
    let mut parts = Vec::with_capacity(args.len());
    for a in args {
        parts.push(a.lit.clone()?);
    }
    Some(parts.join(" "))
}

/// Skip `-x` options (those in `with_arg` consume the next word) and a
/// trailing `--`; return the index of the first non-option word.
fn skip_opts(argv: &[Arg], mut j: usize, with_arg: &[&str]) -> usize {
    while let Some(s) = argv.get(j).and_then(|a| a.lit.as_deref()) {
        if s == "--" {
            return j + 1;
        }
        if !s.starts_with('-') || s.len() < 2 {
            break;
        }
        if with_arg.contains(&s) {
            j += 2;
        } else {
            j += 1;
        }
    }
    j
}

/// Literal positional words (options skipped; `with_arg` consume a value).
fn positionals<'a>(args: &'a [Arg], with_arg: &[&str]) -> Vec<&'a str> {
    let mut out = Vec::new();
    let mut j = 0usize;
    while let Some(a) = args.get(j) {
        match a.lit.as_deref() {
            Some(s) if with_arg.contains(&s) => j += 2,
            Some(s) if s.starts_with('-') => j += 1,
            Some(s) => {
                out.push(s);
                j += 1;
            }
            None => {
                out.push("\u{0}");
                j += 1;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(cmd: &str) -> Outcome {
        analyze(
            cmd,
            Path::new("/nonexistent-dir-for-test"),
            &["main".to_string()],
            Some("B"),
        )
    }

    #[test]
    fn plain_and_hidden_push() {
        for c in [
            "git push",
            "FOO=1 git -C . push",
            "bash -c 'git push'",
            "echo $(git push)",
            "echo `git push`",
            "env A=1 nice -n 5 git push",
            "sudo -u x git push",
            "timeout 5 git push",
            "f() { git push; }",
            "if true; then git push; fi",
            "bash <<EOF\ngit push\nEOF",
            "eval git push",
            "cat <(git push)",
            "env -S 'git push'",
            "find . -exec git push \\;",
        ] {
            assert_eq!(run(c), Outcome::Found(Op::Push), "{c}");
        }
    }

    #[test]
    fn data_is_not_code() {
        for c in [
            "echo git push",
            "git commit -m \"git push する\"",
            "cat <<'EOF'\ngit push\nEOF",
            "git log --grep push",
            "git status",
            "# git push",
            "cargo test -p x",
            "grep -rn push .",
        ] {
            assert_eq!(run(c), Outcome::Clean, "{c}");
        }
    }

    #[test]
    fn undetermined_regardless_of_text() {
        for c in [
            "$G status",
            "git $x",
            "eval \"$c\"",
            "git p",
            "setsid git status",
        ] {
            assert!(matches!(run(c), Outcome::Undetermined(_)), "{c}");
        }
    }
}
