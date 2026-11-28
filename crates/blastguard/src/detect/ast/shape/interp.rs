//! Interpreters the evaluator's `program_source` does not model (`awk -f`,
//! `make -f`, `deno run`, `bun`, `osascript`, `php`, `uv run`, `tsx`, `lua`,
//! `cargo run`): what program each runs, for the learning shape (spec round 4,
//! user ruling 2026-11-28).
//!
//! Each yields the program's file (hashed by its current content), an inline
//! body (hashed), or [`Prog::Unpinned`] (`?`: never learnable as Allow) when
//! the program cannot be named statically. A word the evaluator could not
//! resolve is [`Unresolved`], never a literal and never "no operand".

use super::super::{Value, Word};

pub(super) enum Prog {
    File(Value),
    Inline(String),
    Unpinned,
}

/// Command names this module models (also what a wrapper's operand is checked
/// against: a wrapped interpreter of this list is `?`).
pub(super) const EXTRA: &[&str] = &[
    "awk",
    "gawk",
    "mawk",
    "nawk",
    "make",
    "gmake",
    "deno",
    "bun",
    "osascript",
    "php",
    "uv",
    "tsx",
    "ts-node",
    "lua",
    "luajit",
    "cargo",
];

/// A word whose text the evaluator could not determine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Unresolved;

fn lit(w: &Word) -> Result<&str, Unresolved> {
    match &w.value {
        Value::Literal(s) => Ok(s.as_str()),
        Value::Undetermined(_) => Err(Unresolved),
    }
}

/// The text of the word after `args[i]`, as an inline body.
fn inline_after(args: &[Word], i: usize) -> Prog {
    match args.get(i + 1).map(lit) {
        Some(Ok(b)) => Prog::Inline(b.to_string()),
        Some(Err(Unresolved)) | None => Prog::Unpinned,
    }
}

fn file_after_flag(args: &[Word], i: usize) -> Prog {
    match args.get(i + 1) {
        Some(w) => Prog::File(w.value.clone()),
        None => Prog::Unpinned,
    }
}

fn is_option(w: &Word) -> bool {
    matches!(lit(w), Ok(s) if s.starts_with('-'))
}

fn script_ext(s: &str) -> bool {
    [".js", ".mjs", ".cjs", ".ts", ".mts", ".cts", ".jsx", ".tsx"]
        .iter()
        .any(|e| s.ends_with(e))
}

/// The first operand that is not an option, from `args` (an unresolved word is
/// an operand).
fn first_operand(args: &[Word]) -> Option<(usize, &Word)> {
    args.iter().enumerate().find(|(_, w)| !is_option(w))
}

pub(super) fn extra_programs(base: &str, args: &[Word]) -> Vec<Prog> {
    match base {
        "awk" | "gawk" | "mawk" | "nawk" => awk(args),
        "make" | "gmake" => make(args),
        "deno" => deno(args),
        "bun" => bun(args),
        "osascript" => osascript(args),
        "php" => php(args),
        "uv" => uv(args),
        "tsx" | "ts-node" => match first_operand(args) {
            Some((_, w)) => vec![Prog::File(w.value.clone())],
            None => Vec::new(),
        },
        "lua" | "luajit" => lua(args),
        "cargo" => cargo(args),
        _ => Vec::new(),
    }
}

fn awk(args: &[Word]) -> Vec<Prog> {
    let mut out = Vec::new();
    let mut i = 0;
    let mut program_file = false;
    while i < args.len() {
        match lit(&args[i]) {
            Ok("-f") | Ok("--file") => {
                program_file = true;
                out.push(file_after_flag(args, i));
                i += 2;
            }
            Ok(s) if s.starts_with("--file=") => {
                program_file = true;
                out.push(Prog::File(Value::Literal(s["--file=".len()..].to_string())));
                i += 1;
            }
            Ok(s) if s.starts_with("-f") && s.len() > 2 && !s.starts_with("--") => {
                program_file = true;
                out.push(Prog::File(Value::Literal(s[2..].to_string())));
                i += 1;
            }
            // Options with a separate value.
            Ok("-v") | Ok("-F") | Ok("-e") | Ok("--source") | Ok("-i") => i += 2,
            Ok("--") => i += 1,
            Ok(s) if s.starts_with('-') && s.len() > 1 => i += 1,
            // The first operand is the program text (unless `-f` gave it).
            Ok(_) | Err(Unresolved) => {
                if !program_file {
                    out.push(match lit(&args[i]) {
                        Ok(body) => Prog::Inline(body.to_string()),
                        Err(Unresolved) => Prog::Unpinned,
                    });
                }
                break;
            }
        }
    }
    out
}

fn make(args: &[Word]) -> Vec<Prog> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match lit(&args[i]) {
            Ok("-f") | Ok("--file") | Ok("--makefile") => {
                out.push(file_after_flag(args, i));
                i += 2;
            }
            Ok(s) if s.starts_with("--file=") || s.starts_with("--makefile=") => {
                let v = s.split_once('=').map(|x| x.1).unwrap_or("");
                out.push(Prog::File(Value::Literal(v.to_string())));
                i += 1;
            }
            Ok(s) if s.starts_with("-f") && s.len() > 2 && !s.starts_with("--") => {
                out.push(Prog::File(Value::Literal(s[2..].to_string())));
                i += 1;
            }
            // An unresolved word may be `-f <file>`.
            Err(Unresolved) => {
                out.push(Prog::Unpinned);
                i += 1;
            }
            Ok(_) => i += 1,
        }
    }
    out
}

fn deno(args: &[Word]) -> Vec<Prog> {
    let Some((i, sub)) = first_operand(args) else {
        return Vec::new();
    };
    match lit(sub) {
        Ok("run") => file_after(&args[i + 1..]),
        Ok("eval") => match first_operand(&args[i + 1..]).map(|(_, w)| lit(w)) {
            Some(Ok(body)) => vec![Prog::Inline(body.to_string())],
            Some(Err(Unresolved)) | None => vec![Prog::Unpinned],
        },
        Ok("task") | Ok("x") | Ok("repl") | Ok("install") => vec![Prog::Unpinned],
        Ok(s) if script_ext(s) => vec![Prog::File(sub.value.clone())],
        Ok(_) => Vec::new(),
        Err(Unresolved) => vec![Prog::Unpinned],
    }
}

/// The program file named by the first operand of `args`; a URL or an
/// unresolved word is not pinned.
fn file_after(args: &[Word]) -> Vec<Prog> {
    match first_operand(args) {
        Some((_, w)) => match lit(w) {
            Ok(s) if s.contains("://") => vec![Prog::Unpinned],
            Ok(_) => vec![Prog::File(w.value.clone())],
            Err(Unresolved) => vec![Prog::Unpinned],
        },
        None => vec![Prog::Unpinned],
    }
}

fn bun(args: &[Word]) -> Vec<Prog> {
    let Some((i, first)) = first_operand(args) else {
        return Vec::new();
    };
    match lit(first) {
        Ok("run") => match first_operand(&args[i + 1..]) {
            Some((_, w)) if matches!(lit(w), Ok(s) if script_ext(s)) => {
                vec![Prog::File(w.value.clone())]
            }
            // A package.json script or a binary name: not a file.
            Some(_) | None => vec![Prog::Unpinned],
        },
        Ok("x") | Ok("exec") | Ok("repl") | Ok("create") => vec![Prog::Unpinned],
        Ok(s) if script_ext(s) => vec![Prog::File(first.value.clone())],
        Ok(_) => Vec::new(),
        Err(Unresolved) => vec![Prog::Unpinned],
    }
}

fn osascript(args: &[Word]) -> Vec<Prog> {
    let mut out = Vec::new();
    let mut i = 0;
    let mut inline = false;
    while i < args.len() {
        match lit(&args[i]) {
            Ok("-e") => {
                inline = true;
                out.push(inline_after(args, i));
                i += 2;
            }
            Ok("-l") => i += 2,
            Ok(s) if s.starts_with('-') && s.len() > 1 => i += 1,
            Ok(_) | Err(Unresolved) => {
                if !inline {
                    out.push(Prog::File(args[i].value.clone()));
                }
                break;
            }
        }
    }
    out
}

fn php(args: &[Word]) -> Vec<Prog> {
    let mut i = 0;
    while i < args.len() {
        match lit(&args[i]) {
            Ok("-r") => return vec![inline_after(args, i)],
            Ok("-f") => return vec![file_after_flag(args, i)],
            // Options with a separate value.
            Ok("-c") | Ok("-d") | Ok("-S") | Ok("-t") => i += 2,
            Ok(s) if s.starts_with('-') && s.len() > 1 => i += 1,
            Ok(_) | Err(Unresolved) => return vec![Prog::File(args[i].value.clone())],
        }
    }
    Vec::new()
}

fn uv(args: &[Word]) -> Vec<Prog> {
    let Some((i, sub)) = first_operand(args) else {
        return Vec::new();
    };
    if lit(sub) != Ok("run") {
        return Vec::new();
    }
    // `uv run [opts] script.py`: the first `.py` operand; any other command
    // (`uv run pytest`, `uv run --with x script.py` with an unknown option
    // arity) is not pinned.
    match args[i + 1..]
        .iter()
        .find(|w| matches!(lit(w), Ok(s) if s.ends_with(".py")))
    {
        Some(w) => vec![Prog::File(w.value.clone())],
        None => vec![Prog::Unpinned],
    }
}

fn lua(args: &[Word]) -> Vec<Prog> {
    let mut i = 0;
    while i < args.len() {
        match lit(&args[i]) {
            Ok("-e") => return vec![inline_after(args, i)],
            Ok("-l") => i += 2,
            Ok("-") => return vec![Prog::Unpinned],
            Ok(s) if s.starts_with('-') && s.len() > 1 => i += 1,
            Ok(_) | Err(Unresolved) => return vec![Prog::File(args[i].value.clone())],
        }
    }
    Vec::new()
}

/// `cargo run` compiles and runs the package's sources: no single file names
/// the program.
fn cargo(args: &[Word]) -> Vec<Prog> {
    let sub = args
        .iter()
        .find(|w| !matches!(lit(w), Ok(s) if s.starts_with('-') || s.starts_with('+')));
    match sub.map(lit) {
        Some(Ok("run")) | Some(Ok("r")) => vec![Prog::Unpinned],
        Some(Ok(_)) | Some(Err(Unresolved)) | None => Vec::new(),
    }
}
