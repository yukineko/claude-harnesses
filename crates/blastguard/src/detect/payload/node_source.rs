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
/// `-` or no operand is stdin. Any `--opt=VALUE` word carries its value inside
/// the same word, so it is skipped as one word even when `--opt` is not
/// listed. Any other bare option is Undetermined (it may take the next word
/// as its value; never a guess that the next word is a script file).
///
/// `BOOL` must hold only options that never take a space-separated value
/// (listing a value-taking option there would turn its value into the
/// "script file" and miss a piped program); `VALUE` errs the other way.
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
        "--watch-path",
        "--test-reporter",
        "--test-reporter-destination",
        "--test-name-pattern",
        "--test-skip-pattern",
        "--experimental-default-type",
        "--inspect-port",
        "--debug-port",
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
        // node docs (https://nodejs.org/api/cli.html): these take no value,
        // or one only glued as `--opt=VALUE` (`--inspect[=[host:]port]`).
        "--inspect",
        "--inspect-brk",
        "--inspect-wait",
        "--watch",
        "--watch-preserve-output",
        "--test",
        "--test-only",
        "--experimental-strip-types",
        "--experimental-transform-types",
        "--no-experimental-strip-types",
        "--throw-deprecation",
        "--pending-deprecation",
        "--trace-uncaught",
        "--preserve-symlinks",
        "--preserve-symlinks-main",
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
        } else if BOOL.contains(&w) || (w.starts_with("--") && w.contains('=')) {
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
