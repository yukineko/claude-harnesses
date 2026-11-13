//! Where a reader (a shell, `source` / `.`, Python, another code
//! interpreter) takes its PROGRAM from, read off its argv words: a script
//! operand, stdin, an inline string, or undetermined. Split out of
//! `payload.rs` (file-growth cap); the readings are unchanged.

/// How a reader's arguments decide where its program comes from.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum ProgramSource {
    /// The operand at this word index is the script file.
    File(usize),
    /// No script operand: the program is read from stdin.
    Stdin,
    /// The program is an inline string (`-c`, `-e`, `-m` …): neither an
    /// operand nor stdin is the program.
    Inline,
    /// An option whose shape is not modelled precedes the operands, so which
    /// operand (if any) is the script cannot be proved.
    Undetermined(String),
}

/// Where a shell (`bash`, `sh`, `zsh` …) takes its program from, given its
/// argv words after the command. Options end at `--`, `-`, or the first word
/// that is not an option; `-o` / `-O` (and `+o` / `+O`) take the next word,
/// `--rcfile` / `--init-file` too (or a glued `=VALUE`); `-c` makes the
/// program inline and `-s` makes it stdin. An option letter cluster with
/// `o`/`O` not at its end, or a long option not listed here, is Undetermined.
///
/// This decides only the MAIN program. The `--rcfile` / `--init-file` value is
/// a second program the shell runs (an interactive bash sources it); it is
/// judged separately, see [`shell_startup_file_values`].
pub(super) fn shell_program_source(argv: &[&str]) -> ProgramSource {
    const LONG_FLAGS: [&str; 15] = [
        "--debug",
        "--debugger",
        "--dump-po-strings",
        "--dump-strings",
        "--help",
        "--login",
        "--noediting",
        "--noprofile",
        "--norc",
        "--posix",
        "--pretty-print",
        "--restricted",
        "--verbose",
        "--version",
        "--wordexp",
    ];
    const LONG_VALUE: [&str; 2] = ["--rcfile", "--init-file"];
    let mut stdin_mode = false;
    let mut k = 0;
    while k < argv.len() {
        let w = argv[k];
        if w == "--" || w == "-" {
            k += 1;
            break;
        }
        if w.starts_with("--") {
            if LONG_FLAGS.contains(&w)
                || LONG_VALUE
                    .iter()
                    .any(|o| w.strip_prefix(o).is_some_and(|r| r.starts_with('=')))
            {
                k += 1;
            } else if LONG_VALUE.contains(&w) {
                k += 2;
            } else {
                return ProgramSource::Undetermined(format!(
                    "the shell option `{w}` is not one blastguard models"
                ));
            }
            continue;
        }
        let Some(letters) = w.strip_prefix('-').or_else(|| w.strip_prefix('+')) else {
            break;
        };
        if letters.is_empty() || !letters.chars().all(|c| c.is_ascii_alphabetic()) {
            return ProgramSource::Undetermined(format!(
                "the shell option `{w}` is not one blastguard models"
            ));
        }
        if let Some(p) = letters.find(['o', 'O']) {
            if p + 1 != letters.len() {
                return ProgramSource::Undetermined(format!(
                    "the shell option cluster `{w}` takes a value in the middle"
                ));
            }
        }
        if letters.contains('c') {
            return ProgramSource::Inline;
        }
        if letters.contains('s') {
            stdin_mode = true;
        }
        k += if letters.ends_with(['o', 'O']) { 2 } else { 1 };
    }
    if stdin_mode || k >= argv.len() {
        ProgramSource::Stdin
    } else {
        ProgramSource::File(k)
    }
}

/// The values of every `--rcfile` / `--init-file` option among a shell's argv
/// words (`--rcfile FILE`, `--rcfile=FILE`): the startup file an interactive
/// bash sources, i.e. a program it runs. Every word is scanned, wherever it
/// sits and whether or not `-i` is given (a word after the script operand is
/// an argument, not an option; reading it as one is the restrictive side).
pub(super) fn shell_startup_file_values<'a>(argv: &[&'a str]) -> Vec<&'a str> {
    const OPTS: [&str; 2] = ["--rcfile", "--init-file"];
    let mut out = Vec::new();
    for (k, w) in argv.iter().enumerate() {
        for opt in OPTS {
            if *w == opt {
                if let Some(v) = argv.get(k + 1) {
                    out.push(*v);
                }
            } else if let Some(v) = w.strip_prefix(opt).and_then(|r| r.strip_prefix('=')) {
                out.push(v);
            }
        }
    }
    out
}

/// `source FILE ARGS…` / `. FILE ARGS…`: the first word (after a `--`) is the
/// file. Any option is Undetermined (bash 5.3 has `source -p PATH`).
pub(super) fn source_program_source(argv: &[&str]) -> ProgramSource {
    match argv {
        [] => ProgramSource::Inline,
        ["--", ..] if argv.len() > 1 => ProgramSource::File(1),
        ["--"] => ProgramSource::Inline,
        [w, ..] if w.starts_with('-') => ProgramSource::Undetermined(format!(
            "the `source` option `{w}` is not one blastguard models"
        )),
        _ => ProgramSource::File(0),
    }
}

/// Where `python` / `python3` takes its program from: `-c` / `-m` inline,
/// `-W` / `-X` take a value (glued or the next word), the other single-letter
/// flags take none, `-` or no operand is stdin.
pub(super) fn python_program_source(argv: &[&str]) -> ProgramSource {
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
        let Some(letters) = w.strip_prefix('-') else {
            break;
        };
        if letters.starts_with('-') || letters.is_empty() {
            return ProgramSource::Undetermined(format!(
                "the python option `{w}` is not one blastguard models"
            ));
        }
        let mut step = 1;
        for (pos, c) in letters.char_indices() {
            match c {
                'c' | 'm' => return ProgramSource::Inline,
                'W' | 'X' => {
                    if pos + 1 == letters.len() {
                        step = 2;
                    }
                    break;
                }
                'b' | 'B' | 'd' | 'E' | 'h' | 'i' | 'I' | 'O' | 'P' | 'q' | 's' | 'S' | 'u'
                | 'v' | 'V' | 'x' | 'R' => {}
                _ => {
                    return ProgramSource::Undetermined(format!(
                        "the python option `{w}` is not one blastguard models"
                    ))
                }
            }
        }
        k += step;
    }
    if argv.get(k).is_none_or(|w| *w == "-") {
        ProgramSource::Stdin
    } else {
        ProgramSource::File(k)
    }
}

/// Any other code interpreter (`perl`, `ruby`, `node`, `osascript` …): with no
/// option the first operand is the script (none: stdin); an option cluster
/// holding `e` / `E` (`-e`, `-ne`, `-E`, `--eval`) makes the program inline;
/// any other option's value-taking is not modelled, so Undetermined.
pub(super) fn interpreter_program_source(argv: &[&str]) -> ProgramSource {
    match argv.first() {
        None => ProgramSource::Stdin,
        Some(w) if *w == "-" => ProgramSource::Stdin,
        Some(w) if w.starts_with('-') => {
            let inline = argv.iter().take_while(|a| a.starts_with('-')).any(|a| {
                *a == "--eval"
                    || (!a.starts_with("--")
                        && a[1..].chars().all(|c| c.is_ascii_alphabetic())
                        && a.contains(['e', 'E']))
            });
            if inline {
                ProgramSource::Inline
            } else {
                ProgramSource::Undetermined(format!(
                    "the interpreter option `{w}` is not one blastguard models"
                ))
            }
        }
        Some(_) => ProgramSource::File(0),
    }
}
