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
//! Not covered (stated, not hidden): the tree and the task scope are taken
//! from the payload `cwd`, so a leading `cd <dir> &&` or `--manifest-path`
//! pointing elsewhere is judged against the payload cwd; `tdd red|green`
//! without `--cmd` (which runs tdd's configured `test_cmd`) is not judged.

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
    let segments = match split_segments(&stripped) {
        Some(s) => s,
        None => {
            return if mentions_test_or_merge(&stripped) {
                Decision::Deny(format!(
                    "condukt testscope cannot tokenize this command (unbalanced quote), and it \
                     mentions a cargo test / git merge, so whether it is a full suite or a \
                     mid-run main merge is undetermined: {FULL_SUITE_REASON}"
                ))
            } else {
                Decision::Allow
            };
        }
    };
    let in_task_worktree = home
        .map(|h| cwd.starts_with(h.join(".condukt").join("worktrees")))
        .unwrap_or(false);

    let mut test_cmds: Vec<Words> = Vec::new();
    for seg in &segments {
        let words = match shlex::split(seg) {
            Some(w) => w,
            None => {
                if mentions_test_or_merge(seg) {
                    return Decision::Deny(format!(
                        "condukt testscope cannot tokenize `{seg}`, and it mentions a cargo \
                         test / git merge: {FULL_SUITE_REASON}"
                    ));
                }
                continue;
            }
        };
        match judge_words(&words, in_task_worktree, 0) {
            Judged::Deny(r) => return Decision::Deny(r),
            Judged::Test(w) => test_cmds.extend(w),
            Judged::Other => {}
        }
    }
    if test_cmds.is_empty() {
        return Decision::Allow;
    }
    if let Decision::Deny(r) = check_task_scope(&test_cmds, cwd) {
        return Decision::Deny(r);
    }
    check_rerun(&test_cmds, cwd, ledger)
}

enum Judged {
    Deny(String),
    /// Scoped (allowed so far) test invocations, normalized.
    Test(Vec<Words>),
    Other,
}

fn judge_words(words: &[String], in_task_worktree: bool, depth: usize) -> Judged {
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
                    return judge_payload(payload, in_task_worktree, depth + 1);
                }
            }
            Judged::Other
        }
        // `condukt sandbox run --cmd '<program>'` runs <program> in a container;
        // the program is judged like a `-c` payload.
        "condukt" if depth < 4 && w.get(1).map(String::as_str) == Some("sandbox") => {
            match cmd_flag(w) {
                Some(p) => judge_payload(&p, in_task_worktree, depth + 1),
                None => Judged::Other,
            }
        }
        // `tdd red|green … --cmd '<c>'` runs <c> as the test command; it is
        // judged exactly like the bare command (full suite, task scope and the
        // same rerun-ledger key), so routing a test through tdd changes nothing.
        "tdd" if depth < 4 && matches!(w.get(1).map(String::as_str), Some("red" | "green")) => {
            match cmd_flag(w) {
                Some(p) => judge_payload(&p, in_task_worktree, depth + 1),
                None => Judged::Other,
            }
        }
        "cargo" => judge_cargo(&w[1..]),
        "git" if in_task_worktree => judge_git(&w[1..]),
        _ => Judged::Other,
    }
}

fn judge_payload(payload: &str, in_task_worktree: bool, depth: usize) -> Judged {
    let stripped = strip_heredoc_bodies(payload);
    let Some(segs) = split_segments(&stripped) else {
        return if mentions_test_or_merge(&stripped) {
            Judged::Deny(format!(
                "condukt testscope cannot tokenize the -c program: {FULL_SUITE_REASON}"
            ))
        } else {
            Judged::Other
        };
    };
    let mut found: Vec<Words> = Vec::new();
    for seg in segs {
        let Some(words) = shlex::split(&seg) else {
            if mentions_test_or_merge(&seg) {
                return Judged::Deny(format!(
                    "condukt testscope cannot tokenize `{seg}` in the -c program: \
                     {FULL_SUITE_REASON}"
                ));
            }
            continue;
        };
        match judge_words(&words, in_task_worktree, depth) {
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

fn judge_cargo(args: &[String]) -> Judged {
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
    Judged::Test(vec![norm])
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
    let mut segs = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut chars = cmd.chars();
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
                    if !cur.trim().is_empty() {
                        segs.push(std::mem::take(&mut cur));
                    }
                    cur.clear();
                }
                _ => cur.push(c),
            },
        }
    }
    if quote.is_some() {
        return None;
    }
    if !cur.trim().is_empty() {
        segs.push(cur);
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

fn check_rerun(tests: &[Words], cwd: &Path, ledger: &Path) -> Decision {
    let (top, tree) = match tree_identity(cwd) {
        Ok(t) => t,
        Err(e) => {
            return Decision::Deny(format!(
                "condukt testscope cannot determine the tree this test runs against ({e}), so \
                 whether it re-runs a test already run on the same tree is undetermined"
            ));
        }
    };
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
    let keys: Vec<String> = tests
        .iter()
        .map(|w| format!("{top}\t{tree}\t{}", w.join(" ")))
        .collect();
    for k in &keys {
        if existing.lines().any(|l| l == k) {
            let cmd = k.rsplit('\t').next().unwrap_or_default();
            return Decision::Deny(format!(
                "rerun refused: `{cmd}` already ran on this exact tree ({tree}); running the same \
                 test twice is forbidden (user ruling 2026-11-07). Change the code first, or \
                 move on"
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
            writeln!(f, "{k}")?;
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
        [] => return Decision::Allow,
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
        assert!(matches!(
            judge_words(&words("cargo test"), false, 0),
            Judged::Deny(_)
        ));
        assert!(matches!(
            judge_words(&words("cargo test -p x --test y"), false, 0),
            Judged::Test(_)
        ));
    }
}
