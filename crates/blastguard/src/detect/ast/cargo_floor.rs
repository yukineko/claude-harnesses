//! Backlog 596ef57d: a line that WRITES a file cargo executes by itself and
//! also runs cargo is Ask, whatever wrote the file (heredoc, `printf >`,
//! `tee`, `cp`, ...) and however cargo is reached (`cargo r`, `cargo +nightly
//! run`, `env cargo run`, `rustup run stable cargo run`, `cargo-run`).
//!
//! Raise-only, like the other layers: `Allow` here means no objection. The
//! decision is structural, from the parsed script:
//!
//! * the files the line writes: the target of every writing redirect and the
//!   destination operand(s) of `tee` / `cp` / `mv` / `install` / `ln`;
//!   compared case-insensitively (a case-insensitive filesystem reads
//!   `BUILD.RS` as `build.rs`). A written name that is undetermined may be
//!   any of them, so it counts as every kind of landing (fail closed) — except
//!   the redirect of cargo's own output, which is not a file cargo reads;
//! * a cargo command: a command whose head is `cargo`, `cargo-<sub>`, or a
//!   plain exec wrapper (`env`, `command`, `rustup`, `nohup`, `time`, ...)
//!   with a `cargo` word after it. An undetermined command word may be cargo,
//!   so it counts as one;
//! * what cargo then executes: a subcommand that runs the built program
//!   (`run` / `r` / `bench` / `install`) executes a written Rust source or
//!   Cargo file; ANY subcommand runs a written `build.rs` and reads a written
//!   cargo config (`.cargo/config[.toml]`, which can name a `rustc-wrapper`
//!   or runner). A leading cargo option or an undetermined word hides the
//!   subcommand, so it counts as a run.
//!
//! Bare `cargo test` / `cargo build` over an ordinary `.rs` stays outside
//! (user ruling 2026-11-18, B-4).

use super::{analyze, Analysis, RedirOp, RedirTarget, Script, SimpleCommand, Value};
use crate::model::Decision;

/// Heads that run the command after them without changing what it does.
const WRAPPERS: &[&str] = &[
    "env", "command", "rustup", "nohup", "time", "exec", "nice", "timeout", "sudo", "doas",
    "stdbuf", "setsid", "xargs",
];

/// Commands whose destination operand is a file the line writes.
const DEST_WRITERS: &[&str] = &["cp", "mv", "install", "ln"];

fn base(s: &str) -> String {
    s.trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(s)
        .to_ascii_lowercase()
}

/// A word's text, or the explicit state that it is not known.
enum Name<'a> {
    Known(&'a str),
    Unknown,
}

fn name(v: &Value) -> Name<'_> {
    match v {
        Value::Literal(s) => Name::Known(s.as_str()),
        Value::Undetermined(_) => Name::Unknown,
    }
}

fn is_cargo_word(s: &str) -> bool {
    let b = base(s);
    b == "cargo" || b.starts_with("cargo-")
}

/// The files the line writes (lowercased), and whether some written name is
/// not known.
struct Written {
    files: Vec<String>,
    unknown: bool,
}

fn written(script: &Script) -> Written {
    let mut w = Written {
        files: Vec::new(),
        unknown: false,
    };
    for c in &script.commands {
        let own_output = matches!(cargo_index(c), Some(Some(_)));
        for r in &c.redirects {
            let writes = matches!(
                r.op,
                RedirOp::Out
                    | RedirOp::Append
                    | RedirOp::Clobber
                    | RedirOp::InOut
                    | RedirOp::OutErr
                    | RedirOp::AppendOutErr
            );
            if !writes {
                continue;
            }
            // The name as written, and the path placed against a tracked `cd`
            // (which carries the directory the written name lacks).
            let placed = match &r.target {
                RedirTarget::Path(p) => Some(name(p)),
                _ => None,
            };
            for n in r.written.as_ref().map(name).into_iter().chain(placed) {
                match n {
                    Name::Known(s) => w.files.push(s.to_ascii_lowercase()),
                    Name::Unknown => w.unknown |= !own_output,
                }
            }
        }
        let Some(head) = c.argv.first() else {
            continue;
        };
        let Name::Known(head) = name(&head.value) else {
            continue;
        };
        let head = base(head);
        let is_tee = head == "tee";
        if !is_tee && !DEST_WRITERS.contains(&head.as_str()) {
            continue;
        }
        let mut operands = Vec::new();
        for a in c.argv.iter().skip(1) {
            match name(&a.value) {
                Name::Known(s) if s.starts_with('-') => {}
                Name::Known(s) => operands.push(s.to_ascii_lowercase()),
                Name::Unknown => w.unknown = true,
            }
        }
        if is_tee {
            w.files.extend(operands);
        } else if let Some(d) = operands.pop() {
            w.files.push(d);
        }
    }
    w
}

/// What a written file means to cargo.
struct Landing {
    /// A Rust source or Cargo manifest: run by `cargo run` and friends.
    source: bool,
    /// `build.rs` / cargo config: used by every cargo subcommand.
    build_time: bool,
}

fn landing(w: &Written) -> Landing {
    Landing {
        source: w.unknown
            || w.files
                .iter()
                .any(|f| f.ends_with(".rs") || f.ends_with(".toml")),
        build_time: w.unknown
            || w.files.iter().any(|f| {
                let b = base(f);
                b == "build.rs"
                    // The cargo config is `.cargo/config[.toml]`; a bare
                    // relative name may sit in a `cd .cargo` directory
                    // (unknown), a name under another directory
                    // (`notes/config`) is not it.
                    || (matches!(b.as_str(), "config" | "config.toml")
                        && (!f.contains('/')
                            || f.contains("/.cargo/")
                            || f.starts_with(".cargo/")))
            }),
    }
}

/// Where cargo sits in `c`: `None` when `c` is not cargo, `Some(None)` when
/// the command word is undetermined (it may be cargo), `Some(Some(k))` when
/// `argv[k]` is the cargo word of `cargo ...` or `WRAPPER ... cargo ...`.
fn cargo_index(c: &SimpleCommand) -> Option<Option<usize>> {
    let head = c.argv.first()?;
    let Name::Known(head) = name(&head.value) else {
        return Some(None);
    };
    if is_cargo_word(head) {
        return Some(Some(0));
    }
    if !WRAPPERS.contains(&base(head).as_str()) {
        return None;
    }
    c.argv
        .iter()
        .position(|w| matches!(name(&w.value), Name::Known(s) if is_cargo_word(s)))
        .map(Some)
}

/// Whether `c` runs cargo in a way that executes the written files.
fn runs_cargo_over(c: &SimpleCommand, l: &Landing) -> bool {
    let k = match cargo_index(c) {
        None => return false,
        // An undetermined command word may be cargo running anything.
        Some(None) => return l.source || l.build_time,
        Some(Some(k)) => k,
    };
    // `cargo-run`: the subcommand is spelled in the command word.
    let attached = match name(&c.argv[k].value) {
        Name::Known(s) => base(s).strip_prefix("cargo-").map(str::to_owned),
        Name::Unknown => None,
    };
    let mut sub = attached;
    let mut hidden = false;
    if sub.is_none() {
        for w in &c.argv[k + 1..] {
            match name(&w.value) {
                Name::Unknown => {
                    hidden = true;
                    break;
                }
                Name::Known(s) if s.starts_with('+') => {}
                // A global option may take a value (`--manifest-path X
                // run`), so the subcommand's position is not known.
                Name::Known(s) if s.starts_with('-') => {
                    hidden = true;
                    break;
                }
                Name::Known(s) => {
                    sub = Some(s.to_ascii_lowercase());
                    break;
                }
            }
        }
    }
    let runs_program = hidden || matches!(sub.as_deref(), Some("run" | "r" | "bench" | "install"));
    if runs_program {
        l.source
    } else {
        l.build_time
    }
}

pub(super) fn verdict(command: &str) -> Decision {
    let Analysis::Known(script) = analyze(command) else {
        return Decision::Allow;
    };
    let l = landing(&written(&script));
    if !l.source && !l.build_time {
        return Decision::Allow;
    }
    if script.commands.iter().any(|c| runs_cargo_over(c, &l)) {
        return Decision::ask(
            "this line writes a Rust source, `build.rs` or cargo config and also runs cargo, which executes such files (`cargo run`, a build script, the cargo config) without naming them — blastguard cannot judge the program the file holds",
        );
    }
    Decision::Allow
}
