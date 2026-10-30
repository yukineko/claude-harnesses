//! `webqa weaken-check` — detect test-weakening edits to Playwright specs in a
//! unified diff.
//!
//! Three-valued outcome (never a bool): [`Outcome::Clean`] (exit 0),
//! [`Outcome::Findings`] (exit 1), [`Outcome::Undetermined`] (exit 2). Every
//! error path (unreadable file, `git` failure, non-empty input without any
//! parseable diff header) is `Undetermined`; nothing maps an error to clean.

use std::path::Path;
use std::process::Command;

/// One detected weakening.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub path: String,
    pub rule: &'static str,
    pub detail: String,
}

#[derive(Debug)]
pub enum Outcome {
    Clean,
    Findings(Vec<Finding>),
    Undetermined(String),
}

impl Outcome {
    pub fn exit_code(&self) -> i32 {
        match self {
            Outcome::Clean => 0,
            Outcome::Findings(_) => 1,
            Outcome::Undetermined(_) => 2,
        }
    }
}

/// `git diff --cached`, hardened against repository/user config that would
/// change what the parser sees: an external diff driver (`diff.external`,
/// `GIT_EXTERNAL_DIFF`) or a `textconv` attribute can replace the real patch
/// with arbitrary output (e.g. nothing, which would read as clean); colour,
/// `diff.noprefix` / `diff.mnemonicPrefix` and `diff.relative` change the
/// header and line shapes. Each is pinned off here.
const STAGED_DIFF_ARGS: &[&str] = &[
    "-c",
    "core.quotePath=true",
    "-c",
    "diff.noprefix=false",
    "-c",
    "diff.mnemonicPrefix=false",
    "diff",
    "--cached",
    "--no-ext-diff",
    "--no-textconv",
    "--no-color",
    "--no-relative",
    "--src-prefix=a/",
    "--dst-prefix=b/",
];

/// Run the check against a diff file, or the hardened `git diff --cached`
/// ([`STAGED_DIFF_ARGS`]) when `None`.
pub fn run(diff_file: Option<&Path>) -> Outcome {
    let text = match diff_file {
        Some(p) => match std::fs::read(p) {
            Ok(b) => String::from_utf8_lossy(&b).into_owned(),
            Err(e) => return Outcome::Undetermined(format!("cannot read {}: {e}", p.display())),
        },
        None => match Command::new("git").args(STAGED_DIFF_ARGS).output() {
            Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).into_owned(),
            Ok(o) => {
                return Outcome::Undetermined(format!(
                    "`git {}` exited with {:?}: {}",
                    STAGED_DIFF_ARGS.join(" "),
                    o.status.code(),
                    String::from_utf8_lossy(&o.stderr).trim()
                ))
            }
            Err(e) => return Outcome::Undetermined(format!("cannot run git: {e}")),
        },
    };
    check_diff(&text)
}

/// Is this path a Playwright spec we inspect?
pub fn is_spec_path(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    ["spec", "test"].iter().any(|k| {
        ["ts", "js", "tsx", "jsx", "mjs", "mts", "cjs", "cts"]
            .iter()
            .any(|e| name.ends_with(&format!(".{k}.{e}")))
    })
}

#[derive(Default)]
struct Hunk {
    added: Vec<String>,
    removed: Vec<String>,
}

struct FileDiff {
    /// Post-image path (what the finding is reported against).
    path: String,
    /// Pre-image path (differs from `path` on a rename).
    old_path: String,
    binary: bool,
    hunks: Vec<Hunk>,
}

/// Decode a git C-quoted path (`"a/x\"y"`). `None` = malformed quoting.
fn unquote_c(s: &str) -> Option<String> {
    let inner = s.strip_prefix('"')?.strip_suffix('"')?;
    let mut bytes: Vec<u8> = Vec::new();
    let mut it = inner.bytes();
    while let Some(b) = it.next() {
        if b != b'\\' {
            if b == b'"' {
                return None;
            }
            bytes.push(b);
            continue;
        }
        let e = it.next()?;
        let v = match e {
            b'"' => b'"',
            b'\\' => b'\\',
            b'a' => 7,
            b'b' => 8,
            b'f' => 12,
            b'n' => b'\n',
            b'r' => b'\r',
            b't' => b'\t',
            b'v' => 11,
            b'0'..=b'3' => {
                let mut n = u32::from(e - b'0');
                for _ in 0..2 {
                    let d = it.next()?;
                    if !(b'0'..=b'7').contains(&d) {
                        return None;
                    }
                    n = n * 8 + u32::from(d - b'0');
                }
                u8::try_from(n).ok()?
            }
            _ => return None,
        };
        bytes.push(v);
    }
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// Path token of a `--- `/`+++ ` line with its `a/`/`b/` prefix removed.
/// `Ok(None)` = `/dev/null`. Unquoted paths end at git's trailing TAB.
fn header_path(raw: &str, prefix: &str) -> Result<Option<String>, String> {
    let p = if raw.starts_with('"') {
        // A quoted path may be followed by a TAB too.
        let tok = raw.trim_end_matches(['\t', ' ', '\r']);
        unquote_c(tok).ok_or_else(|| format!("malformed quoted path in diff header: {raw}"))?
    } else {
        raw.split('\t')
            .next()
            .unwrap_or(raw)
            .trim_end_matches('\r')
            .to_string()
    };
    if p == "/dev/null" {
        return Ok(None);
    }
    Ok(Some(p.strip_prefix(prefix).unwrap_or(&p).to_string()))
}

/// `(old, new)` from the remainder of a `diff --git ` line.
fn git_header_paths(rest: &str) -> Result<(String, String), String> {
    let bad = || format!("cannot parse diff header: diff --git {rest}");
    let (old, new) = if rest.starts_with('"') {
        // Find the closing quote of the first token.
        let b = rest.as_bytes();
        let mut i = 1;
        while i < b.len() && b[i] != b'"' {
            i += if b[i] == b'\\' { 2 } else { 1 };
        }
        if i >= b.len() {
            return Err(bad());
        }
        let a = unquote_c(&rest[..=i]).ok_or_else(bad)?;
        let tail = rest[i + 1..].trim_start();
        let n = if tail.starts_with('"') {
            unquote_c(tail.trim_end()).ok_or_else(bad)?
        } else {
            tail.to_string()
        };
        (a, n)
    } else if let Some((a, n)) = rest.rsplit_once(" \"") {
        (a.to_string(), unquote_c(&format!("\"{n}")).ok_or_else(bad)?)
    } else {
        match rest.rsplit_once(" b/") {
            Some((a, n)) => (a.to_string(), format!("b/{n}")),
            None => (rest.to_string(), rest.to_string()),
        }
    };
    Ok((
        old.strip_prefix("a/").unwrap_or(&old).to_string(),
        new.strip_prefix("b/").unwrap_or(&new).to_string(),
    ))
}

fn parse(text: &str) -> Result<Vec<FileDiff>, String> {
    let mut files: Vec<FileDiff> = Vec::new();
    let mut in_hunk = false;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            in_hunk = false;
            let (old, new) = git_header_paths(rest)?;
            files.push(FileDiff {
                path: new,
                old_path: old,
                binary: false,
                hunks: Vec::new(),
            });
            continue;
        }
        let Some(cur) = files.last_mut() else {
            continue;
        };
        if line.starts_with("@@") {
            in_hunk = true;
            cur.hunks.push(Hunk::default());
            continue;
        }
        if !in_hunk {
            if let Some(p) = line.strip_prefix("+++ ") {
                if let Some(p) = header_path(p, "b/")? {
                    cur.path = p;
                }
            } else if let Some(p) = line.strip_prefix("--- ") {
                if let Some(p) = header_path(p, "a/")? {
                    cur.old_path = p;
                }
            } else if let Some(p) = line.strip_prefix("rename from ") {
                cur.old_path = header_path(p, "")?.unwrap_or_default();
            } else if let Some(p) = line.strip_prefix("rename to ") {
                cur.path = header_path(p, "")?.unwrap_or_default();
            } else if line.starts_with("Binary files ") || line.starts_with("GIT binary patch") {
                cur.binary = true;
            }
            continue;
        }
        let Some(h) = cur.hunks.last_mut() else {
            continue;
        };
        if let Some(a) = line.strip_prefix('+') {
            h.added.push(a.to_string());
        } else if let Some(r) = line.strip_prefix('-') {
            h.removed.push(r.to_string());
        }
    }
    Ok(files)
}

pub fn check_diff(text: &str) -> Outcome {
    if text.trim().is_empty() {
        return Outcome::Clean;
    }
    let files = match parse(text) {
        Ok(f) => f,
        Err(why) => return Outcome::Undetermined(why),
    };
    if files.is_empty() {
        return Outcome::Undetermined(
            "input is non-empty but contains no `diff --git` header; not a parseable unified diff"
                .to_string(),
        );
    }
    let mut findings = Vec::new();
    for f in files
        .iter()
        .filter(|f| is_spec_path(&f.path) || is_spec_path(&f.old_path))
    {
        if f.binary {
            return Outcome::Undetermined(format!(
                "binary diff of spec file {}; its content cannot be inspected",
                f.path
            ));
        }
        for h in &f.hunks {
            check_hunk(&f.path, h, &mut findings);
        }
        if let Err(why) = check_timeouts(f, &mut findings) {
            return Outcome::Undetermined(why);
        }
    }
    if findings.is_empty() {
        Outcome::Clean
    } else {
        Outcome::Findings(findings)
    }
}

/// One timeout expression found on a line.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Timeout {
    /// Evaluated value (integer literal or product of integer literals).
    Value(u64),
    /// An expression we cannot evaluate (identifier, sum, overflow, ...).
    Opaque(String),
}

/// Evaluate `123`, `60_000`, `60 * 1000`, `(5 * 60_000)`; anything else is
/// `None`. `0` disables the timeout in Playwright, so it evaluates to
/// `u64::MAX` (larger than any finite timeout).
fn eval_product(expr: &str) -> Option<u64> {
    let mut acc: u64 = 1;
    for factor in strip_parens(expr).split('*') {
        let f: String = strip_parens(factor).chars().filter(|c| *c != '_').collect();
        if f.is_empty() || !f.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        acc = acc.checked_mul(f.parse::<u64>().ok()?)?;
    }
    Some(if acc == 0 { u64::MAX } else { acc })
}

fn strip_parens(e: &str) -> &str {
    let mut e = e.trim();
    while let Some(inner) = e.strip_prefix('(').and_then(|x| x.strip_suffix(')')) {
        e = inner.trim();
    }
    e
}

/// The argument expression at the start of `rest`: up to the first `,`, `)`,
/// `}` or `;` outside brackets/parentheses.
fn leading_expr(rest: &str) -> &str {
    let mut depth: i32 = 0;
    for (i, c) in rest.char_indices() {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' if depth > 0 => depth -= 1,
            ',' | ')' | '}' | ';' if depth == 0 => return &rest[..i],
            _ => {}
        }
    }
    rest
}

/// A `setTimeout(` argument that is a callback (global JS `setTimeout`), not a
/// test timeout.
fn is_callback(expr: &str) -> bool {
    expr.contains("=>") || expr.starts_with("function") || expr.starts_with("async")
}

/// Timeout expressions following `timeout:` or `setTimeout(` on a line. Any
/// expression that does not evaluate is [`Timeout::Opaque`], whatever it
/// starts with; only a `setTimeout(` callback is not a timeout.
fn timeouts(line: &str) -> Vec<Timeout> {
    let mut out = Vec::new();
    for key in ["timeout:", "setTimeout("] {
        let mut rest = line;
        while let Some(i) = rest.find(key) {
            rest = &rest[i + key.len()..];
            let expr = leading_expr(rest).trim();
            match eval_product(expr) {
                Some(n) => out.push(Timeout::Value(n)),
                None if key == "setTimeout(" && is_callback(expr) => {}
                None if expr.is_empty() => out.push(Timeout::Opaque("<empty>".to_string())),
                None => out.push(Timeout::Opaque(expr.to_string())),
            }
        }
    }
    out
}

/// Playwright's default per-test timeout.
const DEFAULT_TIMEOUT_MS: u64 = 30_000;

/// File-level timeout comparison across all hunks. `Err` = cannot determine.
fn check_timeouts(f: &FileDiff, out: &mut Vec<Finding>) -> Result<(), String> {
    let removed: Vec<&String> = f.hunks.iter().flat_map(|h| &h.removed).collect();
    let added: Vec<&String> = f.hunks.iter().flat_map(|h| &h.added).collect();
    // Lines identical on both sides merely moved.
    let rem_t: Vec<Timeout> = removed
        .iter()
        .filter(|l| !added.iter().any(|a| a.trim() == l.trim()))
        .flat_map(|l| timeouts(l))
        .collect();
    let add_t: Vec<Timeout> = added
        .iter()
        .filter(|l| !removed.iter().any(|r| r.trim() == l.trim()))
        .flat_map(|l| timeouts(l))
        .collect();
    let vals = |v: &[Timeout]| -> Vec<u64> {
        v.iter()
            .filter_map(|t| match t {
                Timeout::Value(n) => Some(*n),
                Timeout::Opaque(_) => None,
            })
            .collect()
    };
    let (rv, av) = (vals(&rem_t), vals(&add_t));
    // An added timeout we cannot evaluate may be larger than whatever it
    // replaces (a literal, or the 30 s default when nothing was removed), so
    // it is undetermined either way.
    if let Some(Timeout::Opaque(e)) = add_t.iter().find(|t| matches!(t, Timeout::Opaque(_))) {
        let what = if rv.is_empty() {
            format!("is added (no literal timeout removed, so it replaces the {DEFAULT_TIMEOUT_MS} ms default)")
        } else {
            "replaces a literal timeout".to_string()
        };
        return Err(format!(
            "{}: timeout expression `{e}` {what} and cannot be evaluated",
            f.path
        ));
    }
    // A removed timeout we cannot evaluate has an unknown value: whatever
    // replaces it (a literal, or the 30 s default when nothing is added) may
    // be larger, so the comparison is undetermined either way.
    if let Some(Timeout::Opaque(e)) = rem_t.iter().find(|t| matches!(t, Timeout::Opaque(_))) {
        let what = if add_t.is_empty() {
            format!("is removed (no timeout added, so it reverts to the {DEFAULT_TIMEOUT_MS} ms default)")
        } else {
            "is replaced by a literal timeout".to_string()
        };
        return Err(format!(
            "{}: timeout expression `{e}` {what} and its removed value cannot be evaluated",
            f.path
        ));
    }
    let raised = match (rv.iter().max(), av.iter().max()) {
        (Some(r), Some(a)) if a > r => {
            Some(format!("timeout raised {} -> {}", show_ms(*r), show_ms(*a)))
        }
        (None, Some(a)) if *a > DEFAULT_TIMEOUT_MS => Some(format!(
            "timeout {} added (above the {DEFAULT_TIMEOUT_MS} ms default) with none removed",
            show_ms(*a)
        )),
        // Removing a timeout below the default with nothing added reverts it
        // to the 30 s default: a raise.
        (Some(_), None) => rv
            .iter()
            .min()
            .filter(|r| **r < DEFAULT_TIMEOUT_MS)
            .map(|r| {
                format!(
                    "timeout {} removed with none added, reverting to the {DEFAULT_TIMEOUT_MS} ms default",
                    show_ms(*r)
                )
            }),
        _ => None,
    };
    if let Some(detail) = raised {
        out.push(Finding {
            path: f.path.clone(),
            rule: "timeout-raised",
            detail,
        });
    }
    Ok(())
}

fn show_ms(v: u64) -> String {
    if v == u64::MAX {
        "0 (disabled)".to_string()
    } else {
        v.to_string()
    }
}

fn count(lines: &[String], needle: &str) -> usize {
    lines.iter().map(|l| l.matches(needle).count()).sum()
}

const WEAK_MATCHERS: [&str; 4] = [
    ".toBeTruthy(",
    ".toBeDefined(",
    ".not.toBeNull(",
    ".not.toBeUndefined(",
];

fn check_hunk(path: &str, h: &Hunk, out: &mut Vec<Finding>) {
    let mut push = |rule: &'static str, detail: String| {
        out.push(Finding {
            path: path.to_string(),
            rule,
            detail,
        })
    };
    // A line that merely moved (identical in removed and added) is not new.
    let is_new = |l: &String| !h.removed.iter().any(|r| r.trim() == l.trim());

    for l in h.added.iter().filter(|l| is_new(l)) {
        if [".skip(", ".skip.", ".fixme(", ".fixme."]
            .iter()
            .any(|k| l.contains(k))
        {
            push("skip-added", format!("added: {}", l.trim()));
        }
        if l.contains(".only(") || l.contains(".only.") {
            push("only-added", format!("added: {}", l.trim()));
        }
        if l.contains(".slow(") {
            push(
                "slow-added",
                format!("added (triples the timeout): {}", l.trim()),
            );
        }
        if l.contains("waitForTimeout(") {
            push("wait-timeout-added", format!("added: {}", l.trim()));
        }
    }

    let (exp_r, exp_a) = (count(&h.removed, "expect("), count(&h.added, "expect("));
    if exp_r > exp_a {
        push(
            "expect-removed",
            format!(
                "{} expect() call(s) removed, {} added in this hunk",
                exp_r, exp_a
            ),
        );
    }

    if h.removed.iter().any(|l| l.contains("expect(")) {
        for l in &h.added {
            if let Some(w) = WEAK_MATCHERS.iter().find(|w| l.contains(**w)) {
                if !h.removed.iter().any(|r| r.contains(*w)) {
                    push(
                        "matcher-weakened",
                        format!(
                            "weak matcher `{}` replaces stronger assertion",
                            w.trim_matches(['.', '('])
                        ),
                    );
                }
            }
        }
    }
}

/// Print findings / reason and return the process exit code.
pub fn report(outcome: &Outcome) -> i32 {
    match outcome {
        Outcome::Clean => {}
        Outcome::Findings(fs) => {
            for f in fs {
                println!("{}: {}: {}", f.path, f.rule, f.detail);
            }
        }
        Outcome::Undetermined(why) => {
            eprintln!("webqa weaken-check: cannot determine: {why}");
        }
    }
    outcome.exit_code()
}
