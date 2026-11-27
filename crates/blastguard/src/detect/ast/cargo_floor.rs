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
//!   `BUILD.RS` as `build.rs`). A written name that is undetermined counts as
//!   a source landing only (it may be run by `cargo run`), NOT as a
//!   build-time one: otherwise every `cargo test` line that writes somewhere
//!   undetermined would be Ask (round 3 ruling; the `build.rs` / config
//!   residual is stated at [`landing`]). The redirect of cargo's own output
//!   and a `tee` fed by cargo in a pipeline write a log, not a file cargo
//!   reads, and are not landings;
//! * a cargo command: a command whose head is `cargo`, `cargo-<sub>`, or a
//!   plain exec wrapper (`env`, `command`, `rustup`, `nohup`, `time`, ...)
//!   with a `cargo` word after it. An undetermined command word may be cargo,
//!   so it counts as one;
//! * what cargo then executes: a subcommand that runs the built program
//!   (`run` / `r` / `bench` / `install`) executes a written Rust source or
//!   Cargo file; ANY subcommand runs a written `build.rs` and reads a written
//!   cargo config (`.cargo/config[.toml]`, which can name a `rustc-wrapper`
//!   or runner). The subcommand is read past cargo's global options
//!   (`+toolchain`, `--offline`, `--color X`, `-Z X`, `--manifest-path X`,
//!   ...); an unknown option or an undetermined word hides it, so it counts
//!   as a run.
//!
//! Bare `cargo test` / `cargo build` over an ordinary `.rs` stays outside
//! (user ruling 2026-11-18, B-4).

use super::{analyze, Analysis, Context, RedirOp, RedirTarget, Script, SimpleCommand, Value};
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
    for (i, c) in script.commands.iter().enumerate() {
        let own_output = matches!(cargo_index(c), Some(Some(_)));
        // `cargo test 2>&1 | tee "$LOG"`: the stage after cargo in a pipeline
        // stores cargo's OUTPUT, a log, not a file cargo executes.
        let logs_cargo = matches!(c.context, Context::Pipeline { pos, .. } if pos > 0)
            && i > 0
            && matches!(cargo_index(&script.commands[i - 1]), Some(Some(_)));
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
                Name::Unknown => w.unknown |= !(is_tee && logs_cargo),
            }
        }
        if is_tee && logs_cargo {
            continue;
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
        // An undetermined written name is NOT a build-time landing: it would
        // make every `cargo test` / `build` / `check` line that also writes
        // somewhere undetermined (`echo x > "$F" && cargo test`) Ask, and in
        // a subagent Deny. RESIDUAL (coordinator ruling, round 3): a name that
        // is undetermined AND is `build.rs` / `.cargo/config` with a
        // non-running subcommand is not caught here.
        build_time: w.files.iter().any(|f| {
            let b = base(f);
            b == "build.rs"
                // The cargo config is `.cargo/config[.toml]`; a bare
                // relative name may sit in a `cd .cargo` directory
                // (unknown), a name under another directory
                // (`notes/config`) is not it.
                || (matches!(b.as_str(), "config" | "config.toml")
                    && (!f.contains('/') || f.contains("/.cargo/") || f.starts_with(".cargo/")))
        }),
    }
}

/// The subcommand of `cargo ARGS`, read past cargo's global options.
enum Sub {
    Known(String),
    /// An unknown option or an undetermined word: the position is unknown.
    Hidden,
    /// No subcommand word.
    Absent,
}

/// Global options without a value, and with a value (separate word; the
/// `--opt=value` spelling is one word).
const FLAGS: &[&str] = &[
    "--offline",
    "--frozen",
    "--locked",
    "-q",
    "--quiet",
    "-v",
    "--verbose",
    "-vv",
    "-qq",
];
const VALUED: &[&str] = &["--color", "--config", "-Z", "-C", "--manifest-path"];

fn subcommand(args: &[super::Word]) -> Sub {
    let mut it = args.iter();
    while let Some(w) = it.next() {
        let Name::Known(s) = name(&w.value) else {
            return Sub::Hidden;
        };
        if s.starts_with('+') || FLAGS.contains(&s) {
            continue;
        }
        if VALUED.contains(&s) {
            if !matches!(it.next().map(|v| name(&v.value)), Some(Name::Known(_))) {
                return Sub::Hidden;
            }
            continue;
        }
        let attached = s.split_once('=').is_some_and(|(o, _)| VALUED.contains(&o))
            || (s.len() > 2 && (s.starts_with("-Z") || s.starts_with("-C")));
        if attached {
            continue;
        }
        if s.starts_with('-') {
            return Sub::Hidden;
        }
        return Sub::Known(s.to_ascii_lowercase());
    }
    Sub::Absent
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
    let sub = match attached {
        Some(s) => Sub::Known(s),
        None => subcommand(&c.argv[k + 1..]),
    };
    let runs_program = match sub {
        Sub::Hidden => true,
        Sub::Known(s) => matches!(s.as_str(), "run" | "r" | "bench" | "install"),
        Sub::Absent => false,
    };
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

#[cfg(test)]
mod tests {
    use super::verdict;
    use crate::model::Decision;

    const LIB: &str = "cat > src/lib.rs <<'EOF'\npub fn f() {}\nEOF\n";

    fn asks(cmd: &str) -> bool {
        matches!(verdict(cmd), Decision::Ask(_))
    }

    /// Round 3: this layer alone gives no objection to a cargo line whose
    /// only other write is a LOG (cargo's own output, a `tee` fed by cargo) or
    /// an undetermined name, and reads cargo's global options.
    #[test]
    fn cargo_floor_logs_and_global_options_are_no_objection() {
        for cmd in [
            "echo x > \"$F\" && cargo test".to_string(),
            "cargo test 2>&1 | tee \"$LOG\"".to_string(),
            "cargo test > \"$LOG\" 2>&1".to_string(),
            format!("{LIB}cargo --offline test"),
            format!("{LIB}cargo --locked -q build"),
            format!("{LIB}cargo --color always check"),
            format!("{LIB}cargo -Z unstable-options test"),
            format!("{LIB}cargo --manifest-path Cargo.toml test"),
            format!("{LIB}cargo +nightly --frozen test"),
        ] {
            assert!(!asks(&cmd), "{cmd}");
        }
    }

    #[test]
    fn cargo_floor_runs_after_global_options_still_ask() {
        for cmd in [
            format!("{LIB}cargo --offline run"),
            format!("{LIB}cargo --color always run"),
            format!("{LIB}cargo --manifest-path Cargo.toml r"),
            format!("{LIB}cargo --unknown-option test"),
            "echo x > \"$F\" && cargo run".to_string(),
        ] {
            assert!(asks(&cmd), "{cmd}");
        }
    }
}
