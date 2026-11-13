//! `condukt testscope` — PreToolUse(Bash) gate that enforces the test-scope
//! rulings in code instead of in prompt prose.
//!
//! User rulings (2026-11-07 / 2026-11-08 / 2026-11-12 / 2026-11-14): a full
//! suite is nightly-only; per change only the targeted test files run, once;
//! a run does not chase other sessions' work on main until it is done. Prompt
//! text did not hold (a delegated run executed
//! `cargo test -p blastguard --no-fail-fast` and re-merged main mid-run), so
//! the three rules are judged here, deterministically:
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
//!    denied.
//!
//! Every cannot-determine (unbalanced quotes around a cargo test, a tree
//! identity git cannot give, an unwritable ledger) resolves to deny
//! (CLAUDE.md §3). A command that contains no test / merge invocation is never
//! judged and is allowed by printing nothing.

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
            Judged::Test(w) => test_cmds.push(w),
            Judged::Other => {}
        }
    }
    if test_cmds.is_empty() {
        return Decision::Allow;
    }
    check_rerun(&test_cmds, cwd, ledger)
}

enum Judged {
    Deny(String),
    /// A scoped (allowed) test invocation, normalized.
    Test(Words),
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
            let program = w.iter().enumerate().find_map(|(i, a)| {
                if a == "--cmd" {
                    w.get(i + 1).cloned()
                } else {
                    a.strip_prefix("--cmd=").map(str::to_string)
                }
            });
            match program {
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
    let mut found = Judged::Other;
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
            Judged::Test(t) => found = Judged::Test(t),
            Judged::Other => {}
        }
    }
    found
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
    Judged::Test(norm)
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

/// `Some(reason)` when `cmd` is a full-suite invocation (rule 1 alone, no
/// ledger). Used by `condukt state test` so the binary itself never runs one.
pub fn full_suite_reason(cmd: &str) -> Option<String> {
    let segs = split_segments(&strip_heredoc_bodies(cmd))?;
    for seg in segs {
        let words = shlex::split(&seg)?;
        if let Judged::Deny(r) = judge_words(&words, false, 0) {
            return Some(r);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_suite_reason_flags_bare_cargo_test() {
        assert!(full_suite_reason("cargo test").is_some());
        assert!(full_suite_reason("cargo test -p x --test y").is_none());
    }
}
