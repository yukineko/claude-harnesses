//! `env`'s option walk and `env -S STRING` modelling for the B.5 layer
//! (backlog b1e6f2a7). Split out of `b5.rs` to keep it under its size cap.

use super::*;

/// Where `env`'s option walk (from `argv[k + 1]`) ends.
pub(super) enum EnvEnd<'a> {
    /// What `unwrap_at` returns for this `env`.
    Unwrapped(Unwrapped),
    /// A `-S` / `--split-string` word at `argv[at]` (`flag`): `inline` is
    /// the glued string (`-SSTRING`, `--split-string=STRING`); without it
    /// the string is the next word.
    Split {
        at: usize,
        flag: &'a str,
        inline: Option<&'a str>,
    },
}

/// `env`'s options, `NAME=V` operands and command word from `argv[k + 1]`.
pub(super) fn env_walk(argv: &[Word], k: usize) -> EnvEnd<'_> {
    let mut i = k + 1;
    loop {
        match argv.get(i).map(literal) {
            None => return EnvEnd::Unwrapped(Unwrapped::Nothing),
            Some(Err(_)) => {
                return EnvEnd::Unwrapped(Unwrapped::Undetermined(
                    "an `env` operand is undetermined".into(),
                ))
            }
            Some(Ok(w)) => match w {
                "-i" | "-" | "--ignore-environment" | "-0" | "--null" | "-v" | "--debug" => i += 1,
                "-u" | "--unset" => i += 2,
                "--" => return EnvEnd::Unwrapped(Unwrapped::At(i + 1)),
                w if w.starts_with("--unset=") || (w.starts_with("-u") && w.len() > 2) => i += 1,
                "-S" | "--split-string" => {
                    return EnvEnd::Split {
                        at: i,
                        flag: w,
                        inline: None,
                    }
                }
                w if w.starts_with("--split-string=") || (w.starts_with("-S") && w.len() > 2) => {
                    let inline = w
                        .strip_prefix("--split-string=")
                        .or_else(|| w.strip_prefix("-S"));
                    return EnvEnd::Split {
                        at: i,
                        flag: w,
                        inline,
                    };
                }
                w if w.starts_with('-') => {
                    return EnvEnd::Unwrapped(Unwrapped::Undetermined(format!(
                        "`env` option `{w}` is not modelled"
                    )))
                }
                w if w.contains('=') => i += 1,
                _ => return EnvEnd::Unwrapped(Unwrapped::At(i)),
            },
        }
    }
}

/// How many `env -S` strings one command may split (`env -S '-S …'`
/// nests); past it the command is Undetermined.
const MAX_ENV_SPLITS: usize = 8;

/// One `classify` step for `env -S` at `cmd.argv[k]`: `None` when it is not
/// one; else the command with the split words spliced in (`splits` counts
/// the nesting) or the reason it is Undetermined.
pub(super) fn env_split_step(
    cmd: &SimpleCommand,
    k: usize,
    splits: &mut usize,
) -> Option<Result<SimpleCommand, String>> {
    let split = env_split_at(&cmd.argv, k)?;
    *splits += 1;
    Some(match split {
        Ok(_) if *splits > MAX_ENV_SPLITS => Err("`env -S` strings nested too deeply".into()),
        Ok(argv) => Ok(SimpleCommand {
            argv,
            ..cmd.clone()
        }),
        Err(r) => Err(r),
    })
}

/// `argv` with `argv[k]`'s `env -S STRING` (`-SSTRING`, `--split-string
/// STRING`, `--split-string=STRING`) replaced by the words env splits the
/// literal STRING into (GNU `build_argv`, the splitter the legacy layer
/// uses: `executors::env_split_argv`). GNU env goes on reading options and
/// `NAME=V` operands over the split words, so the caller unwraps the
/// returned argv again at `k`. `None` when `argv[k]` is not such an `env`;
/// `Err` when the string is not settled statically (a non-literal word, a
/// `$` / `${VAR}`, an escape env rejects or reads differently, `\c` inside
/// `"…"`, a trailing backslash, an open quote, no string at all).
fn env_split_at(argv: &[Word], k: usize) -> Option<Result<Vec<Word>, String>> {
    let name = argv.get(k).map(literal)?.ok()?;
    if basename(name) != "env" {
        return None;
    }
    let EnvEnd::Split { at, flag, inline } = env_walk(argv, k) else {
        return None;
    };
    let (string, span, after) = match inline {
        Some(s) => (s, argv[at].span.clone(), at + 1),
        None => match argv.get(at + 1) {
            Some(w) => match literal(w) {
                Ok(s) => (s, w.span.clone(), at + 2),
                Err(r) => {
                    return Some(Err(format!(
                        "the `env {flag}` string is undetermined ({r})"
                    )))
                }
            },
            None => return Some(Err(format!("`env {flag}` has no string"))),
        },
    };
    let split = match crate::detect::executors::env_split_argv(string) {
        Ok(split) => split,
        Err(why) => return Some(Err(format!("the `env {flag}` string holds {why}"))),
    };
    let mut out = argv[..at].to_vec();
    out.extend(split.into_iter().map(|arg| Word {
        raw: format!("'{}'", arg.replace('\'', r"'\''")),
        value: Value::Literal(arg),
        quoting: super::super::Quoting::Single,
        span: span.clone(),
    }));
    out.extend_from_slice(argv.get(after..).unwrap_or(&[]));
    Some(Ok(out))
}
