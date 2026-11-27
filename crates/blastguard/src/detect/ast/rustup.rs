//! When the r21 exemption (`. "$HOME/.cargo/env"` is rustup's PATH setup and
//! runs no text of the line, [`super::is_rustup_env_source`]) no longer
//! holds. Backlog 92f4a28d: the exemption assumed `~/.cargo/env` is the file
//! rustup wrote, but an earlier command of the SAME line can replace it
//! (`ln -sf /dev/stdin ~/.cargo/env`, `cp evil.sh ~/.cargo/env`) or feed the
//! sourced stdin (`exec 0<<< '…'`), so the `.` would run line text.
//!
//! The rule is an ALLOWLIST (no per-writer denylist: `env ln`, `nice ln`,
//! `find -exec ln`, `tar -C ~`, `curl -o` … are endless): the exemption holds
//! only if EVERY earlier simple command of the line (all segments, subshells,
//! groups) is one of a few commands read below as writing nothing
//! ([`known_pure`]). Anything else — an unknown command, any wrapper, a
//! redirect to an undeterminable or cargo-env-like target, a `HOME`
//! assignment, an interpreter — drops it; a bare `. "$HOME/.cargo/env"`
//! stays exempt. `source` is read exactly like `.`.
//!
//! [`tainted_by`] is one command's part, read in order by the evaluator;
//! [`line_tainted`] is the whole line's, read by the relink layer through
//! [`super::targets`].

use super::{is_rustup_env_source, RedirOp, RedirTarget, SimpleCommand, Value};

/// A literal that may name `~/.cargo/env`, the directory it lives in, or the
/// file by its last component alone (`cd ~/.cargo; mkdir env`).
fn may_name_cargo_env(p: &str) -> bool {
    p.contains(".cargo") || p.trim_end_matches('/').rsplit('/').next() == Some("env")
}

/// A word that is not a literal, or may name `~/.cargo/env`.
fn word_taints(v: &Value) -> bool {
    match v {
        Value::Undetermined(_) => true,
        Value::Literal(p) => may_name_cargo_env(p),
    }
}

fn lit(v: &Value) -> Option<&str> {
    // `None` is "not a literal": every caller reads it as NOT known-pure
    // (`is_some_and`), never as a determined empty value.
    let Value::Literal(s) = v else {
        return None;
    };
    Some(s.as_str())
}

/// `sc` is a `.` / `source` of one operand spelled as the cargo env file
/// (`"$HOME/.cargo/env"`, `~/.cargo/env`, …), whatever it evaluates to.
pub(crate) fn is_cargo_env_dot(sc: &SimpleCommand) -> bool {
    matches!(sc.argv.as_slice(),
        [dot, f] if matches!(lit(&dot.value), Some("." | "source")) && f.raw.contains(".cargo/env"))
}

/// Commands read as writing no file, running no program and rebinding no
/// stdin, judged only from the AST (all the words must be literals that do
/// not name the cargo env file):
///
/// * `cd`, `true`, `false`, `:`, `pwd`, `test`, `[`, `echo`, and the readers
///   `cat`, `ls`, `head`, `tail`, `wc`: no file effect (their output goes
///   where a checked redirect sends it);
/// * `printf` without `-v` (`-v` assigns a shell variable);
/// * `export` of names other than `HOME`;
/// * `mkdir` of paths that do not name the cargo env file;
/// * `git status|log|diff|show|stash|rev-parse` without an `--output`
///   option: they read the repository or move its work tree, never `~/.cargo`
///   unless a word names it.
fn pure_head(sc: &SimpleCommand) -> bool {
    let Some(head) = sc.argv.first().and_then(|w| lit(&w.value)) else {
        return false;
    };
    let args = &sc.argv[1..];
    match head {
        "cd" | "true" | "false" | ":" | "pwd" | "test" | "[" | "echo" => true,
        // Readers with no option that writes a file: their only write is a
        // redirect, which `known_pure` checks.
        "cat" | "ls" | "head" | "tail" | "wc" => true,
        "printf" => args
            .iter()
            .all(|w| lit(&w.value).is_some_and(|s| s != "-v")),
        "export" => args.iter().all(|w| {
            lit(&w.value).is_some_and(|s| s.split('=').next() != Some("HOME") && s != "-n")
        }),
        "mkdir" => args.iter().all(|w| !word_taints(&w.value)),
        "git" => {
            args.first().and_then(|w| lit(&w.value)).is_some_and(|s| {
                matches!(
                    s,
                    "status" | "log" | "diff" | "show" | "stash" | "rev-parse"
                )
            }) && args.iter().all(|w| {
                !word_taints(&w.value) && !lit(&w.value).is_some_and(|s| s.starts_with("--output"))
            })
        }
        _ => false,
    }
}

/// `sc` writes nothing that can change what `. "$HOME/.cargo/env"` runs.
fn known_pure(sc: &SimpleCommand) -> bool {
    // `HOME=… cmd` / `HOME=…` rebinds what `$HOME` names.
    if sc.assignments.iter().any(|(n, _)| n == "HOME") {
        return false;
    }
    for r in &sc.redirects {
        // `exec`-style stdin rebinding is not a pure command's business, and
        // an output redirect must name a determined, non-cargo-env file.
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
                let written = r.written.as_ref().unwrap_or(v);
                if word_taints(v) || word_taints(written) {
                    return false;
                }
            }
        }
    }
    // An assignment-only command: no program runs.
    if sc.argv.is_empty() {
        return true;
    }
    if let Value::Literal(c) = &sc.cwd {
        if may_name_cargo_env(c) {
            return false;
        }
    }
    pure_head(sc)
}

/// `sc`, run before an `. "$HOME/.cargo/env"`, may have changed what that
/// `.` sources or the stdin it reads.
pub(crate) fn tainted_by(sc: &SimpleCommand) -> bool {
    !is_rustup_env_source(sc) && !known_pure(sc)
}

/// The line's cargo env `.` / `source` is not the exempt form (its operand
/// names another file, e.g. after `HOME=/tmp/x`), or some command before the
/// last one taints it.
pub(crate) fn line_tainted(cmds: &[SimpleCommand]) -> bool {
    let dots: Vec<&SimpleCommand> = cmds.iter().filter(|c| is_cargo_env_dot(c)).collect();
    let Some(last) = dots.iter().map(|c| c.span.start).max() else {
        return false;
    };
    dots.iter().any(|c| !is_rustup_env_source(c))
        || cmds.iter().any(|c| c.span.start < last && tainted_by(c))
}
