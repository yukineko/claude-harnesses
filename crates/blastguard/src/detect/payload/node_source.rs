//! How `node` takes its program (backlog f9861c4e), split out of `payload.rs`
//! (file-growth cap).

use super::program_source::ProgramSource;
use crate::detect::normalized_command;

/// True when `head` is `node` or `nodejs`.
pub(super) fn is_node_head(head: &str) -> bool {
    matches!(normalized_command(head).as_str(), "node" | "nodejs")
}

/// Where `node` takes its program from: `-e` / `-p` / `-c` (and their long
/// forms) inline; `-r` / `--require` / `--import` / `--loader` … take a value
/// (the next word unless glued with `=`); the listed boolean flags take none;
/// `-` or no operand is stdin. Any other option is Undetermined (never a
/// guess that the next word is a script file).
pub(super) fn node_program_source(argv: &[&str]) -> ProgramSource {
    const VALUE: &[&str] = &[
        "-r",
        "--require",
        "--import",
        "--loader",
        "--experimental-loader",
        "-C",
        "--conditions",
        "--input-type",
        "--title",
        "--env-file",
    ];
    const BOOL: &[&str] = &[
        "-i",
        "--interactive",
        "-v",
        "--version",
        "-h",
        "--help",
        "--no-warnings",
        "--trace-warnings",
        "--trace-deprecation",
        "--no-deprecation",
        "--expose-gc",
        "--enable-source-maps",
        "--experimental-vm-modules",
        "--experimental-modules",
        "--abort-on-uncaught-exception",
    ];
    let mut k = 0;
    while k < argv.len() {
        let w = argv[k];
        if w == "-" {
            return ProgramSource::Stdin;
        }
        if w == "--" {
            k += 1;
            break;
        }
        if !w.starts_with('-') {
            break;
        }
        if matches!(
            w,
            "-e" | "-p" | "-pe" | "-c" | "--eval" | "--print" | "--check"
        ) || w.starts_with("--eval=")
            || w.starts_with("--print=")
        {
            return ProgramSource::Inline;
        }
        if VALUE.contains(&w) {
            k += 2;
        } else if BOOL.contains(&w)
            || w.split_once('=').is_some_and(|(f, _)| {
                w.starts_with("--") && (VALUE.contains(&f) || BOOL.contains(&f))
            })
        {
            k += 1;
        } else {
            return ProgramSource::Undetermined(format!(
                "the node option `{w}` is not one blastguard models"
            ));
        }
    }
    if argv.get(k).is_none_or(|w| *w == "-") {
        ProgramSource::Stdin
    } else {
        ProgramSource::File(k)
    }
}
