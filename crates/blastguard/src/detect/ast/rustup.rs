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
            // `-v NAME` and the attached `-vNAME` both assign a variable.
            .all(|w| lit(&w.value).is_some_and(|s| !s.starts_with("-v"))),
        "export" => args.iter().all(|w| {
            lit(&w.value).is_some_and(|s| s.split('=').next() != Some("HOME") && s != "-n")
        }),
        "mkdir" => args.iter().all(|w| !word_taints(&w.value)),
        "git" => {
            args.first().and_then(|w| lit(&w.value)).is_some_and(|s| {
                matches!(
                    s,
                    "status" | "log" | "diff" | "show" | "rev-parse" | "stash"
                )
            }) && stash_ok(args)
                && args.iter().all(|w| {
                    !word_taints(&w.value)
                        && !lit(&w.value).is_some_and(|s| s.starts_with("--output"))
                })
        }
        _ => false,
    }
}

/// `git stash` restores no stashed content: bare, or `list` / `show` / `push`
/// / `save` / `drop` / `clear`. `pop`, `apply`, `branch` and any other form
/// rewrite the tree from content the line does not show (a dotfiles repo at
/// `HOME`), so they are not pure. Other git subcommands pass.
fn stash_ok(args: &[super::Word]) -> bool {
    if args.first().and_then(|w| lit(&w.value)) != Some("stash") {
        return true;
    }
    match args.get(1) {
        None => true,
        Some(w) => lit(&w.value)
            .is_some_and(|s| matches!(s, "list" | "show" | "push" | "save" | "drop" | "clear")),
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

/// The program of `text` holds the cargo env `.` / `source` inside a `for` /
/// `while` / `until` clause (the condition of a `while` / `until` included) or
/// a function body: there the next iteration / call runs it after the commands
/// that textually follow it. A loop elsewhere does not matter. Walked over the
/// parser's node types (`brush_parser::ast`); an unparsable text counts as
/// holding one.
fn dot_in_loop(text: &str) -> bool {
    match super::parse::parse(text) {
        Ok(program) => program
            .complete_commands
            .iter()
            .any(|l| list_has_dot(l, false)),
        Err(_) => true,
    }
}

use brush_parser::ast as bp;

/// `w` is the cargo env file named by a command word (inside a loop every
/// word is read, so a `$( . ~/.cargo/env )` of a loop body counts).
fn word_names_cargo_env(w: &bp::Word) -> bool {
    w.value.contains(".cargo/env")
}

fn list_has_dot(l: &bp::CompoundList, in_loop: bool) -> bool {
    l.0.iter().any(|item| {
        let and_or = &item.0;
        std::iter::once(&and_or.first)
            .chain(and_or.additional.iter().map(|a| match a {
                bp::AndOr::And(p) | bp::AndOr::Or(p) => p,
            }))
            .any(|p| p.seq.iter().any(|c| command_has_dot(c, in_loop)))
    })
}

fn command_has_dot(c: &bp::Command, in_loop: bool) -> bool {
    match c {
        bp::Command::Simple(s) => simple_has_dot(s, in_loop),
        bp::Command::Compound(cc, _) => compound_has_dot(cc, in_loop),
        // A function body runs when it is called, not where it is written.
        bp::Command::Function(f) => compound_has_dot(&f.body.0, true),
        // `[[ … ]]` runs no command (a `$( )` in it is not read: it is a word
        // of the test, outside every loop-order question this answers).
        bp::Command::ExtendedTest(..) => false,
    }
}

fn simple_has_dot(s: &bp::SimpleCommand, in_loop: bool) -> bool {
    let items = s
        .prefix
        .iter()
        .flat_map(|p| p.0.iter())
        .chain(s.suffix.iter().flat_map(|p| p.0.iter()));
    let words_hit = in_loop
        && (s.word_or_name.as_ref().is_some_and(word_names_cargo_env)
            || items.clone().any(|i| match i {
                bp::CommandPrefixOrSuffixItem::Word(w)
                | bp::CommandPrefixOrSuffixItem::AssignmentWord(_, w) => word_names_cargo_env(w),
                _ => false,
            }));
    words_hit
        || items.into_iter().any(|i| match i {
            bp::CommandPrefixOrSuffixItem::ProcessSubstitution(_, sub) => {
                list_has_dot(&sub.list, in_loop)
            }
            _ => false,
        })
}

fn compound_has_dot(c: &bp::CompoundCommand, in_loop: bool) -> bool {
    use bp::CompoundCommand as C;
    match c {
        C::Arithmetic(_) => false,
        C::BraceGroup(b) => list_has_dot(&b.list, in_loop),
        C::Subshell(s) => list_has_dot(&s.list, in_loop),
        C::ForClause(f) => list_has_dot(&f.body.list, true),
        C::ArithmeticForClause(f) => list_has_dot(&f.body.list, true),
        C::WhileClause(w) | C::UntilClause(w) => {
            list_has_dot(&w.0, true) || list_has_dot(&w.1.list, true)
        }
        C::IfClause(i) => {
            list_has_dot(&i.condition, in_loop)
                || list_has_dot(&i.then, in_loop)
                || i.elses.iter().flatten().any(|e| {
                    e.condition.iter().any(|c| list_has_dot(c, in_loop))
                        || list_has_dot(&e.body, in_loop)
                })
        }
        C::CaseClause(c) => c
            .cases
            .iter()
            .any(|i| i.cmd.iter().any(|l| list_has_dot(l, in_loop))),
        C::Coprocess(p) => command_has_dot(&p.body, true),
    }
}

/// The line's cargo env `.` / `source` is not the exempt form (its operand
/// names another file, e.g. after `HOME=/tmp/x`), or some command before the
/// last one taints it.
pub(crate) fn line_tainted(text: &str, cmds: &[SimpleCommand]) -> bool {
    let dots: Vec<&SimpleCommand> = cmds.iter().filter(|c| is_cargo_env_dot(c)).collect();
    let Some(last) = dots.iter().map(|c| c.span.start).max() else {
        return false;
    };
    // Textual order is not run order inside a loop (the `.` of the next
    // iteration runs after the commands below it): with a loop on the line
    // the order is undetermined and the exemption is dropped. A function
    // definition already makes the whole analysis Undetermined.
    dot_in_loop(text)
        || dots.iter().any(|c| !is_rustup_env_source(c))
        || cmds.iter().any(|c| c.span.start < last && tainted_by(c))
}
