//! RED 2026-11-13: process-group kill spawned without `--` leaks the group on Linux.
//!
//! Commit 0a16ac9f fixed `tests/support/bounded.rs`: it ran
//! `kill -TERM -<pgid>` through `Command::new("kill")`. procps-ng
//! `/usr/bin/kill` (Linux; probed with procps-ng 4.0.4 on WSL2) reads the
//! negative pgid as an option when no `--` precedes it, exits 0 and signals
//! nothing. `kill -TERM -- -<pgid>` works there and is valid POSIX (`--` ends
//! options), so BSD/macOS are unaffected.
//!
//! The same argv shape remained in `tests/log_subcommand_20261113.rs` (the
//! timeout path of the stdin-held `blastguard log` test): on Linux a timed-out
//! child process group would leak silently, because `kill` exits 0 and the
//! result is discarded.
//!
//! This file scans the blastguard crate's `src/` and `tests/` for spawns of
//! the `kill` program and fails when an argument starting with `-` (other than
//! `--`) follows the signal option without a preceding `--`. The controls pin
//! that the scanner (a) flags the pre-fix bounded.rs shape, (b) accepts the
//! fixed shape and the harmless `kill -0 <pid>` liveness probe, and (c) sees
//! real kill sites in the tree at all — an empty scan must not read as clean.
//!
//! The fixtures spell the program as `KILL` and substitute it at runtime, so
//! this file never contains the needle itself and does not scan itself in.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};

fn needle() -> String {
    format!("Command::new({:?})", "kill")
}

fn fixture(src: &str) -> String {
    src.replace("Command::new(KILL)", &needle())
}

/// Index just past the expression starting at `s[0]` that is closed by the
/// matching bracket of `open`.
fn balanced_end(s: &str, open: char, close: char) -> Option<usize> {
    let mut depth = 0i32;
    let mut in_str = false;
    let mut prev = '\0';
    for (i, c) in s.char_indices() {
        if in_str {
            if c == '"' && prev != '\\' {
                in_str = false;
            }
        } else if c == '"' {
            in_str = true;
        } else if c == open {
            depth += 1;
        } else if c == close {
            depth -= 1;
            if depth == 0 {
                return Some(i + 1);
            }
        }
        prev = if prev == '\\' && c == '\\' { '\0' } else { c };
    }
    None
}

fn split_top_level(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut in_str = false;
    let mut prev = '\0';
    let mut cur = String::new();
    for c in s.chars() {
        if in_str {
            if c == '"' && prev != '\\' {
                in_str = false;
            }
        } else if c == '"' {
            in_str = true;
        } else if "([{".contains(c) {
            depth += 1;
        } else if ")]}".contains(c) {
            depth -= 1;
        } else if c == ',' && depth == 0 {
            out.push(cur.trim().to_string());
            cur.clear();
            prev = c;
            continue;
        }
        cur.push(c);
        prev = c;
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_string());
    }
    out
}

/// The literal text an argument expression starts with, when knowable:
/// `"-TERM"` -> `-TERM`, `format!("-{pgid}")` -> `-{pgid}`, `&x` -> recurse.
/// `None` for opaque expressions (`pid.to_string()`).
fn literal_prefix(expr: &str) -> Option<String> {
    let e = expr.trim().trim_start_matches('&').trim();
    let e = e.strip_prefix("format!(").unwrap_or(e).trim();
    let rest = e.strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// Argument expressions passed to the `kill` command constructed at `at`
/// (the byte offset of the needle), in order. Follows the builder chain to
/// its `;`, and when the command was bound with `let [mut] NAME =`, the
/// immediately following `NAME.arg(..)` / `NAME.args(..)` statements.
fn kill_args(src: &str, at: usize) -> Vec<String> {
    let mut args = Vec::new();
    let line_start = src[..at].rfind('\n').map_or(0, |i| i + 1);
    let binding = src[line_start..at]
        .trim()
        .strip_prefix("let ")
        .map(|s| s.trim_start_matches("mut ").trim())
        .and_then(|s| s.strip_suffix('='))
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let mut pos = at + needle().len();
    loop {
        // Walk one statement, collecting .arg / .args calls.
        loop {
            let rest = &src[pos..];
            let t = rest.trim_start();
            pos += rest.len() - t.len();
            if t.starts_with(".args(") {
                let inner_start = pos + ".args(".len();
                let Some(len) = balanced_end(&src[pos + ".args".len()..], '(', ')') else {
                    return args;
                };
                let inner = &src[inner_start..pos + ".args".len() + len - 1];
                let inner = inner.trim();
                let inner = inner
                    .strip_prefix('[')
                    .and_then(|s| s.strip_suffix(']'))
                    .or_else(|| inner.strip_prefix("&[").and_then(|s| s.strip_suffix(']')))
                    .unwrap_or(inner);
                args.extend(split_top_level(inner));
                pos += ".args".len() + len;
            } else if t.starts_with(".arg(") {
                let Some(len) = balanced_end(&src[pos + ".arg".len()..], '(', ')') else {
                    return args;
                };
                args.push(
                    src[pos + ".arg(".len()..pos + ".arg".len() + len - 1]
                        .trim()
                        .to_string(),
                );
                pos += ".arg".len() + len;
            } else if let Some(method) = t.strip_prefix('.') {
                // Another builder call (.status(), .env(..), ...): skip it.
                let name_end = method
                    .find(|c: char| !(c.is_alphanumeric() || c == '_'))
                    .map_or(t.len(), |i| i + 1);
                pos += name_end;
                if src[pos..].starts_with('(') {
                    let Some(len) = balanced_end(&src[pos..], '(', ')') else {
                        return args;
                    };
                    pos += len;
                }
            } else {
                break;
            }
        }
        // End of statement. Continue only into `NAME.arg(` / `NAME.args(`.
        let rest = &src[pos..];
        let t = rest.trim_start();
        let t = t.strip_prefix(';').unwrap_or(t);
        let t2 = t.trim_start();
        if binding.is_empty() {
            return args;
        }
        let call = format!("{binding}.arg");
        if t2.starts_with(&call) {
            pos = src.len() - t2.len() + binding.len();
        } else {
            return args;
        }
    }
}

/// A kill spawn is unsafe when, after its first (signal/option) argument, an
/// argument starting with `-` other than `--` appears before any `--`.
fn unsafe_kill(args: &[String]) -> Option<String> {
    let mut iter = args.iter();
    let first = iter.next()?;
    let first_lit = literal_prefix(first);
    if first_lit.as_deref() == Some("--") {
        return None;
    }
    if !first_lit.as_deref().is_some_and(|s| s.starts_with('-')) {
        // First argument is not an option: it is itself an operand.
        return None;
    }
    for a in iter {
        match literal_prefix(a).as_deref() {
            Some("--") => return None,
            Some(s) if s.starts_with('-') => return Some(a.clone()),
            _ => {}
        }
    }
    None
}

/// Every (byte offset, args, offending arg) for kill spawns in `src`.
fn scan_source(src: &str) -> Vec<(usize, Vec<String>, Option<String>)> {
    let n = needle();
    src.match_indices(&n)
        .map(|(at, _)| {
            let args = kill_args(src, at);
            let bad = unsafe_kill(&args);
            (at, args, bad)
        })
        .collect()
}

fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let rd = std::fs::read_dir(dir).unwrap_or_else(|e| panic!("read_dir {dir:?}: {e}"));
    for ent in rd {
        let p = ent
            .unwrap_or_else(|e| panic!("dir entry in {dir:?}: {e}"))
            .path();
        if p.is_dir() {
            rs_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// (relative path, line, args, offending arg) for every kill spawn in the crate.
fn scan_crate() -> Vec<(String, usize, Vec<String>, Option<String>)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    rs_files(&root.join("src"), &mut files);
    rs_files(&root.join("tests"), &mut files);
    assert!(!files.is_empty(), "scan found no .rs files under {root:?}");
    let mut hits = Vec::new();
    for f in files {
        let src = std::fs::read_to_string(&f).unwrap_or_else(|e| panic!("read {f:?}: {e}"));
        for (at, args, bad) in scan_source(&src) {
            let line = src[..at].matches('\n').count() + 1;
            let rel = f.strip_prefix(root).unwrap().display().to_string();
            hits.push((rel, line, args, bad));
        }
    }
    hits
}

const BEFORE_FIX: &str = r#"
    let killed = Command::new(KILL)
        .arg("-TERM")
        .arg(format!("-{pgid}"))
        .status();
"#;

const AFTER_FIX: &str = r#"
    let killed = Command::new(KILL)
        .arg("-TERM")
        .arg("--")
        .arg(format!("-{pgid}"))
        .status();
"#;

const BOUND_THEN_ARGS_BAD: &str = r#"
    let mut cmd = Command::new(KILL);
    cmd.args(["-KILL", &format!("-{}", child.id())]);
    let _ = cmd.status();
"#;

const LIVENESS_PROBE: &str = r#"
    let mut cmd = Command::new(KILL);
    cmd.arg("-0").arg(pid.to_string());
"#;

#[test]
fn control_scanner_flags_the_pre_fix_bounded_rs_shape() {
    let hits = scan_source(&fixture(BEFORE_FIX));
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert_eq!(hits[0].1, vec!["\"-TERM\"", "format!(\"-{pgid}\")"]);
    assert_eq!(
        hits[0].2.as_deref(),
        Some("format!(\"-{pgid}\")"),
        "{hits:?}"
    );

    let hits = scan_source(&fixture(BOUND_THEN_ARGS_BAD));
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert!(
        hits[0].2.is_some(),
        "bound `let mut cmd` + .args form: {hits:?}"
    );
}

#[test]
fn control_scanner_accepts_the_fixed_shape_and_the_liveness_probe() {
    let hits = scan_source(&fixture(AFTER_FIX));
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert_eq!(hits[0].1.len(), 3, "{hits:?}");
    assert_eq!(hits[0].2, None, "{hits:?}");

    let hits = scan_source(&fixture(LIVENESS_PROBE));
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert_eq!(hits[0].1, vec!["\"-0\"", "pid.to_string()"], "{hits:?}");
    assert_eq!(hits[0].2, None, "{hits:?}");
}

/// The tree scan is not vacuous: it sees the fixed `support/bounded.rs` site,
/// parses its three arguments and judges it clean.
#[test]
fn control_tree_scan_sees_the_fixed_bounded_rs_site() {
    let hits = scan_crate();
    let bounded: Vec<_> = hits
        .iter()
        .filter(|h| h.0.ends_with("bounded.rs") && h.0.contains("support"))
        .collect();
    assert_eq!(bounded.len(), 1, "all hits: {hits:#?}");
    assert_eq!(bounded[0].2.len(), 3, "{bounded:#?}");
    assert_eq!(bounded[0].3, None, "{bounded:#?}");
}

#[test]
#[ignore = "RED 2026-11-13: log_subcommand_20261113.rs spawns `kill -TERM -<pid>` without `--`; procps-ng kill signals nothing (group leaks on Linux)"]
fn no_process_group_kill_without_double_dash() {
    let bad: Vec<_> = scan_crate().into_iter().filter(|h| h.3.is_some()).collect();
    assert!(
        bad.is_empty(),
        "kill spawned with a negative pid/pgid and no preceding `--` (procps-ng kill \
         on Linux exits 0 and signals nothing; use `kill -SIG -- -<pgid>`):\n{bad:#?}"
    );
}
