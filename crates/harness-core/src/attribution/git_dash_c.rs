//! `git -C <dir>`: the directory a git command runs in.

use super::plan::logical_join;
use super::*;

/// The operands of the `-C <dir>` global options of a `git` stage, in order.
/// git takes `-C` as a separate word only (`-C=dir`/`-Cdir` are not options;
/// `git help git`). Scanning stops at the subcommand (first non-option word).
pub(super) fn git_dash_c_operands(words: &[String]) -> Vec<String> {
    let mut it = words.iter().map(String::as_str).peekable();
    // Skip leading `VAR=val` assignments, then require the program be `git`.
    while it
        .peek()
        .is_some_and(|w| w.contains('=') && !w.starts_with('-') && !w.contains('/'))
    {
        it.next();
    }
    if it.next().and_then(|p| p.rsplit('/').next()) != Some("git") {
        return Vec::new();
    }
    let mut out = Vec::new();
    while let Some(w) = it.next() {
        match w {
            "-C" => match it.next() {
                Some(d) => out.push(d.to_string()),
                None => break,
            },
            "-c" | "--namespace" | "--git-dir" | "--work-tree" | "--config-env" | "--exec-path" => {
                it.next();
            }
            _ if w.starts_with('-') => {}
            _ => break,
        }
    }
    out
}

/// Add the directory a `git -C a -C b ...` runs in (each non-absolute operand
/// is relative to the preceding one, an empty one changes nothing) to `out`,
/// joined LOGICALLY with each candidate base. With no known base a relative
/// chain is kept as is (it cannot be placed, like an entered relative dir).
pub(super) fn insert_dash_c_dirs(out: &mut BTreeSet<PathBuf>, ops: &[String], bases: &[PathBuf]) {
    if ops.is_empty() {
        return;
    }
    let chain = |start: Option<&PathBuf>| -> PathBuf {
        let mut cur = start.cloned().unwrap_or_default();
        for o in ops {
            if !o.is_empty() {
                cur = logical_join(&cur, Path::new(o));
            }
        }
        cur
    };
    let mut put = |d: PathBuf| {
        if d.as_os_str().is_empty() {
            return;
        }
        if d.is_relative() {
            out.insert(d);
        } else if d.exists() {
            out.insert(resolve(&d));
        }
    };
    if bases.is_empty() {
        put(chain(None));
    } else {
        for b in bases {
            put(chain(Some(b)));
        }
    }
}
