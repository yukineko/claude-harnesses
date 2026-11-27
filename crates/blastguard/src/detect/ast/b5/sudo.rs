//! The `sudo` / `doas` arm of `b5::unwrap_at` and its long-option table
//! (backlog a761b38f), moved out of `b5.rs` (file-growth split), and the
//! reading of a shell-mode command as one shell body (backlog 8f8acf5e).

use super::*;

/// `argv[k]` named `base` (`sudo` or `doas`) as an unwrapped wrapper; `i` is
/// `k + 1`, the first word after the wrapper name. `*shell_cmd` is set when
/// the result is `At(i)` in shell mode (`-s`, `-i`, `--shell`, `--login`): the
/// wrapper's shell then runs the words `argv[i..]` as one `-c` body
/// ([`wrapper_shell_command`]).
///
/// SUBSET.md Table B.3 `sudo`, `doas` row (user ruling 2026-11-20 Q1, Q3).
/// sudo(8) long options are modelled by `sudo_long` (sudo only).
pub(super) fn unwrap_sudo(
    argv: &[Word],
    base: &str,
    mut i: usize,
    shell_cmd: &mut bool,
) -> Unwrapped {
    let word = |i: usize| argv.get(i).map(literal);
    let undet = |o: &str| Unwrapped::Undetermined(format!("`{base}` option `{o}` is not modelled"));
    let mut shell = false;
    // A mode that runs no command (`sudo -l`, `-v`, `-K`, `-V`).
    let mut no_exec = false;
    loop {
        match word(i) {
            Some(Err(_)) => {
                return Unwrapped::Undetermined(format!("a `{base}` operand is undetermined"))
            }
            Some(Ok("--")) => {
                i += 1;
                break;
            }
            Some(Ok("-u" | "-g" | "-C" | "-D" | "-h" | "-p" | "-r" | "-t" | "-U")) => i += 2,
            Some(Ok(w))
                if w.len() > 2
                    && ["-u", "-g", "-C", "-D", "-h", "-p", "-r", "-t", "-U"]
                        .iter()
                        .any(|o| w.starts_with(o)) =>
            {
                i += 1
            }
            Some(Ok("-s" | "-i")) => {
                shell = true;
                i += 1
            }
            Some(Ok("-E" | "-H" | "-n" | "-S" | "-k" | "-A" | "-P")) => i += 1,
            Some(Ok(
                "-l" | "--list" | "-v" | "--validate" | "-K" | "--remove-timestamp" | "-V"
                | "--version",
            )) => {
                no_exec = true;
                i += 1
            }
            Some(Ok("-e")) => {
                return Unwrapped::Undetermined(format!("`{base} -e` (sudoedit) is not modelled"))
            }
            // sudo(8) long options (backlog a761b38f). `doas` has none:
            // its long options fall through to `undet` below.
            Some(Ok(w)) if base == "sudo" && w.starts_with("--") => match sudo_long(w) {
                SudoLong::Value => i += 2,
                SudoLong::InlineValue | SudoLong::Flag => i += 1,
                SudoLong::Shell => {
                    shell = true;
                    i += 1
                }
                SudoLong::NoExec => {
                    no_exec = true;
                    i += 1
                }
                SudoLong::Edit => {
                    return Unwrapped::Undetermined(format!(
                        "`{base} --edit` (sudoedit) is not modelled"
                    ))
                }
                SudoLong::Unknown => return undet(w),
            },
            Some(Ok(w)) if w.starts_with('-') => return undet(w),
            _ => break,
        }
    }
    if no_exec {
        // Alone it runs nothing. A word after it (`sudo -l rm`) is not
        // modelled: Undetermined rather than assuming it is not run.
        return match word(i) {
            None => Unwrapped::Nothing,
            Some(_) => Unwrapped::Undetermined(format!(
                "`{base}` with a list / validate / version option and a command is not modelled"
            )),
        };
    }
    // `NAME=VALUE` operands before the command set its environment.
    loop {
        match word(i) {
            Some(Err(_)) => {
                return Unwrapped::Undetermined(format!("a `{base}` operand is undetermined"))
            }
            Some(Ok(w)) if w.contains('=') => i += 1,
            Some(Ok(_)) => {
                *shell_cmd = shell;
                return Unwrapped::At(i);
            }
            None if shell => return Unwrapped::ImplicitShell,
            // `sudo` / `doas` without a command prints its usage.
            None => return Unwrapped::Nothing,
        }
    }
}

/// The verdict of the command words a wrapper's shell runs
/// (`sudo -s 'rm -rf /etc'`, backlog 8f8acf5e, user ruling 2026-11-21):
/// sudo(8) passes the words to the shell's `-c`, so `cmd.argv[at..]` joined by
/// single spaces is judged like every literal shell body — this layer's
/// recursive reading (with `cmd`'s stdin as the body's inherited stdin) AND the
/// full detector as `sh -c '<body>'` ([`judge_body`]). The caller takes the
/// stricter of this and the direct-argv reading (never below Ask).
///
/// sudo also backslash-escapes the words' shell metacharacters; that is not
/// modelled here (the words' metacharacters are read as live, the restrictive
/// reading). A word whose value is undetermined makes the body Undetermined.
pub(super) fn wrapper_shell_command(
    cmd: &SimpleCommand,
    at: usize,
    depth: usize,
    judge: &Probes<'_>,
    inherited: &StdinSource,
) -> Decision {
    let words = match literals(cmd.argv.get(at..).unwrap_or(&[])) {
        Ok(w) => w,
        Err(r) => {
            return undetermined(format_args!(
                "a word of the command a wrapper's shell runs is undetermined ({r})"
            ))
        }
    };
    let body = words.join(" ");
    let mut acc = VerdictAcc::default();
    if let Some(deny) = acc.record(layer(
        &body,
        depth + 1,
        judge,
        inherited_stdin(cmd, inherited),
    )) {
        return deny;
    }
    if let Some(deny) = acc.record(judge_body(&body, Runner::Shell("sh"), judge)) {
        return deny;
    }
    acc.finish()
}

/// How `unwrap_sudo` treats one sudo(8) long option (backlog a761b38f).
enum SudoLong {
    /// `--opt VALUE`: the next word is the option's argument.
    Value,
    /// `--opt=VALUE` (or `--preserve-env=list`): the argument is inline.
    InlineValue,
    /// A flag with no argument.
    Flag,
    /// `--login` / `--shell`: as `-i` / `-s`.
    Shell,
    /// A mode that runs no command (`--list`, `--validate`, ...).
    NoExec,
    /// `--edit` (sudoedit): not modelled, like `-e`.
    Edit,
    /// Any other long option: not modelled (Undetermined).
    Unknown,
}

/// Classify a sudo(8) long option `w` (starting with `--`, not `--` itself).
/// Only the options listed here are modelled; anything else is
/// [`SudoLong::Unknown`], which the caller resolves to Undetermined.
fn sudo_long(w: &str) -> SudoLong {
    // Long options taking a value: `-u -g -C -D -h(host) -p -r -t -U`.
    const VALUE: [&str; 9] = [
        "--user",
        "--group",
        "--close-from",
        "--chdir",
        "--host",
        "--prompt",
        "--role",
        "--type",
        "--other-user",
    ];
    // `-E -H -n -S -k -A -P -b`.
    const FLAG: [&str; 8] = [
        "--preserve-env",
        "--set-home",
        "--non-interactive",
        "--stdin",
        "--reset-timestamp",
        "--askpass",
        "--preserve-groups",
        "--background",
    ];
    if let Some((name, _)) = w.split_once('=') {
        return if VALUE.contains(&name) || name == "--preserve-env" {
            SudoLong::InlineValue
        } else {
            SudoLong::Unknown
        };
    }
    match w {
        _ if VALUE.contains(&w) => SudoLong::Value,
        _ if FLAG.contains(&w) => SudoLong::Flag,
        "--login" | "--shell" => SudoLong::Shell,
        "--list" | "--validate" | "--remove-timestamp" | "--version" | "--help" => SudoLong::NoExec,
        "--edit" => SudoLong::Edit,
        _ => SudoLong::Unknown,
    }
}
