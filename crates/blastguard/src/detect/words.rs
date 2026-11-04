use super::{is_code_interpreter, xargs_command_start};

pub(super) fn basename(tok: &str) -> &str {
    tok.rsplit('/').next().unwrap_or(tok)
}

/// Parse the FIRST shell word of `s`, honouring single quotes, double quotes and
/// backslash escapes, and return it with ONE level of quoting removed.
///
/// Whitespace-only tokenisation elsewhere in this file never unquotes, so a
/// command word written in any of the standard quoting forms (`\rm`, `"rm"`,
/// `'rm'`, `r""m`) survives as a literal that matches no rule arm and falls
/// through to Allow. This is the shared primitive that closes that hole; it is
/// also what makes a `-c` operand recoverable as a single word regardless of
/// what follows it on the line.
///
/// Deliberately NOT a full shell lexer: no expansion of `$VAR`, `${…}`,
/// `$(…)`/backticks, `~`, globs or history. See `normalized_command` for the
/// list of forms that remain out of reach and why.
pub(super) fn first_shell_word(s: &str) -> Option<String> {
    let mut chars = s.chars().peekable();
    while chars.peek().is_some_and(|c| c.is_whitespace()) {
        chars.next();
    }
    let mut out = String::new();
    let mut saw_word = false;
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                // Single quotes: everything up to the next `'` is literal.
                saw_word = true;
                for d in chars.by_ref() {
                    if d == '\'' {
                        break;
                    }
                    out.push(d);
                }
            }
            '"' => {
                // Double quotes: only `\"`, `\\`, `\$` and '\`' are escapes.
                saw_word = true;
                while let Some(d) = chars.next() {
                    if d == '"' {
                        break;
                    }
                    if d == '\\' {
                        match chars.next() {
                            Some(e @ ('"' | '\\' | '$' | '`')) => out.push(e),
                            Some(e) => {
                                out.push('\\');
                                out.push(e);
                            }
                            None => out.push('\\'),
                        }
                    } else {
                        out.push(d);
                    }
                }
            }
            '\\' => {
                // Backslash escapes the next char (`\rm` -> `rm`, the standard
                // alias-bypass idiom); a trailing lone `\` is kept literal.
                saw_word = true;
                match chars.next() {
                    Some(d) => out.push(d),
                    None => out.push('\\'),
                }
            }
            c if c.is_whitespace() => break,
            c => {
                saw_word = true;
                out.push(c);
            }
        }
    }
    if saw_word {
        Some(out)
    } else {
        None
    }
}

/// Normalise a COMMAND WORD to the bare program name used for rule matching.
///
/// Applies, in order: one level of shell quoting/escaping removal
/// (`first_shell_word`) and then `basename`. Together these defend the forms
/// that are reachable in ordinary use and that all name the SAME program:
///
///   * `\rm`      — the standard alias-bypass idiom (verified bypass)
///   * `"rm"` / `'rm'` / `r""m` — quoted or split command words
///   * `/bin/rm`, `$HOME/bin/rm` (literal path part) — via `basename`
///   * `command rm`, `sudo rm`, `env rm`, … — via `command_index`'s wrapper skip
///
/// OUT OF REACH (deliberately not attempted — each needs runtime state we do
/// not have, and guessing would deny benign commands):
///   * `$(which rm)`, `` `which rm` ``, `$RM`, `${RM}` — command/parameter
///     substitution: the value is only known at execution time.
///   * user aliases and shell functions (`alias x='rm -rf'`) — defined in the
///     user's rc files, not visible from the tool call.
///   * `r$'m'` / `$'\x72m'` ANSI-C quoting and `%` -style expansions.
///   * a program reached through a symlink or a PATH shadow whose NAME is not
///     `rm` (e.g. `~/bin/cleanup` that execs `rm -rf`) — nothing in the command
///     line says so.
///
/// Operand tokens are also intentionally left un-normalised: unquoting them
/// would change glob/path semantics that `analyze_rm` reasons about.
pub(super) fn normalized_command(tok: &str) -> String {
    let unquoted = first_shell_word(tok).unwrap_or_else(|| tok.to_string());
    basename(&unquoted).to_string()
}

pub(super) fn is_assignment(tok: &str) -> bool {
    if let Some(eq) = tok.find('=') {
        let name = &tok[..eq];
        !name.is_empty()
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            && name
                .chars()
                .next()
                .map(|c| !c.is_ascii_digit())
                .unwrap_or(false)
    } else {
        false
    }
}

/// True if `cmd` is an exec-wrapper: a program whose own arguments end in a
/// COMMAND that it then runs, so the effective command word is BEHIND it.
pub(super) fn is_exec_wrapper(cmd: &str) -> bool {
    matches!(
        cmd,
        "sudo"
            | "doas"
            | "nohup"
            | "env"
            | "command"
            | "builtin"
            | "exec"
            | "time"
            | "nice"
            | "ionice"
            | "timeout"
            | "stdbuf"
            | "setsid"
            | "flock"
            | "chroot"
            // Multi-call binaries: `busybox sh -c …` / `busybox rm -rf …` run
            // the applet named by the next word (daf7611b).
            | "busybox"
            | "toybox"
    )
}

/// Candidate indices of the effective command word, skipping leading `VAR=val`
/// assignments. Callers analyse EVERY candidate and deny if ANY of them is
/// destructive.
///
/// D3 (verified fail-open, fixed): this used to be `command_index` — a function
/// that computed the SINGLE index it believed was the command word by modelling
/// which of each wrapper's options take a separate value token
/// (`wrapper_flag_takes_value`) and how many positional operands the wrapper
/// consumes (`wrapper_leading_operands`). That model was wrong for GNU LONG
/// options, which accept `--opt VALUE` as two tokens: `wrapper_flag_takes_value`
/// returned false for every flag whose length was not exactly 2, commented "long
/// flags already carry their value inline". So the VALUE token broke the flag
/// loop, was eaten as the wrapper's leading operand, and the real command word
/// was misresolved — `timeout --kill-after 10 5 rm -rf /path`,
/// `chroot --userspec root /newroot rm -rf /path`, `stdbuf --output 0 rm …`,
/// `flock --wait 5 /tmp/l rm …`, `sudo --user root rm …`, `env --unset FOO rm …`
/// and `nice --adjustment 5 rm …` were all ALLOW. The short forms denied
/// correctly, which is why the tests missed it.
///
/// The fix deliberately does NOT extend the option model — that is the "reason
/// about token positions" pattern that has already rotted twice in this file,
/// and every long option added later would reopen the same gap. Both option
/// tables are DELETED. Instead, once a wrapper is seen, EVERY subsequent
/// non-empty token position becomes a candidate command word. The candidate set
/// is additive and evaluated deny-if-ANY, so whichever position the wrapper
/// really execs is guaranteed to be among them, however its options parse.
///
/// This mirrors the shape BG-1 already used for `-c` payloads
/// (see `dash_c_payloads`).
///
/// Widening justification: a candidate at a non-command position denies only if
/// that token is ITSELF a destructive command word with destructive operands
/// following it (e.g. an option value that literally reads `rm -rf <path>`).
/// That is a false positive, and a recoverable one — the acceptable side of this
/// module's fail-closed bias.
///
/// When the first non-assignment token is NOT a wrapper, exactly one candidate
/// is returned, so ordinary commands are unaffected.
pub(super) fn command_candidates(tokens: &[&str]) -> Vec<usize> {
    let mut i = 0;
    while i < tokens.len() {
        let t = tokens[i];
        if t.is_empty() {
            i += 1;
            continue;
        }
        if is_assignment(t) {
            i += literal_assignment_span(tokens, i);
            continue;
        }
        if is_exec_wrapper(&normalized_command(t)) {
            // Every position behind the wrapper is a candidate.
            return (i + 1..tokens.len())
                .filter(|&j| !tokens[j].is_empty() && !opens_unclosed_quote(tokens[j]))
                .collect();
        }
        return vec![i];
    }
    Vec::new()
}

/// Tokens [`command_candidates`] skips for the leading assignment at
/// `tokens[i]`: the whole shell word when its quoted value spans several
/// whitespace-split tokens (`x='recursive rm'` is `x='recursive` + `rm'`),
/// otherwise 1.
///
/// Backlog ce04adeb (#1527): skipping only `x='recursive` made `rm'` the
/// command word, and `python3 -c "x='recursive rm'<newline>y=a[:1]"` (the
/// newline lost to the whitespace split) was denied as "rm with a wildcard".
///
/// The multi-token skip applies only when the word provably ends
/// (`shell_word_token_span` is `Some`) AND holds no `$` or backtick, i.e.
/// no expansion or command substitution can run inside it. Anything else
/// keeps the one-token skip, so the tokens after it stay candidates.
fn literal_assignment_span(tokens: &[&str], i: usize) -> usize {
    match super::payload::shell_word_token_span(tokens, i) {
        Some(span) if !tokens[i..i + span].iter().any(|t| t.contains(['$', '`'])) => span,
        _ => 1,
    }
}

/// Indices of the command words an exec-wrapper chain can actually run —
/// `env [opts] [VAR=val] [--] CMD`, `command CMD`, `nice -n 5 CMD`,
/// `timeout 5 CMD`, `sudo -u root CMD`, nested wrappers — WITHOUT the operands
/// of the command finally reached. The last index is that command; earlier
/// ones are the wrappers and any token that may be either an option's value or
/// the command.
///
/// Unlike [`command_candidates`], which makes EVERY token behind a wrapper a
/// candidate (sound for deny-if-ANY destructive checks, where a false
/// candidate only adds a false positive), this is for asking "what program
/// reads this stage's stdin", where grep's operand in `env grep bash` must
/// not be read as a shell.
///
/// Ambiguity resolves to MORE candidates, never fewer: an option token not
/// known to take no value (see [`wrapper_flag_is_boolean`]) makes the next
/// token a candidate AND keeps scanning past it, so a value-taking option
/// missing from any table widens the set instead of hiding the command.
pub(super) fn wrapped_command_heads(tokens: &[&str]) -> Vec<usize> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < tokens.len() && (tokens[i].is_empty() || is_assignment(tokens[i])) {
        i += 1;
    }
    while i < tokens.len() {
        out.push(i);
        let wrapper = normalized_command(tokens[i]);
        if !is_exec_wrapper(&wrapper) {
            return out;
        }
        // Wrappers whose syntax puts one positional operand before the
        // command: `timeout DURATION`, `chroot NEWROOT`, `flock FILE`.
        let mut positional =
            usize::from(matches!(wrapper.as_str(), "timeout" | "chroot" | "flock"));
        let (mut value_pending, mut opts_done) = (false, false);
        let mut j = i + 1;
        let next_head = loop {
            let Some(&t) = tokens.get(j) else {
                break None;
            };
            j += 1;
            if t.is_empty() {
                continue;
            }
            if !opts_done && t == "--" {
                opts_done = true;
                value_pending = false;
                continue;
            }
            if !opts_done && t.starts_with('-') && t.len() > 1 {
                value_pending = !t.contains('=') && !wrapper_flag_is_boolean(&wrapper, t);
                continue;
            }
            if value_pending {
                // An option value, or the command itself: both.
                out.push(j - 1);
                value_pending = false;
                continue;
            }
            if wrapper == "env" && is_assignment(t) {
                continue;
            }
            if positional > 0 {
                positional -= 1;
                continue;
            }
            break Some(j - 1);
        };
        match next_head {
            Some(h) => i = h,
            None => return out,
        }
    }
    out
}

/// True when `flag` is an option of `wrapper` KNOWN to take no separate value.
/// A flag missing here is treated as possibly value-taking, which only adds a
/// candidate in [`wrapped_command_heads`] — the restrictive direction.
pub(super) fn wrapper_flag_is_boolean(wrapper: &str, flag: &str) -> bool {
    let known: &[&str] = match wrapper {
        "env" => &[
            "-i",
            "-0",
            "-v",
            "--ignore-environment",
            "--null",
            "--debug",
        ],
        "command" => &["-p", "-v", "-V"],
        "sudo" => &[
            "-E",
            "-H",
            "-n",
            "-S",
            "-k",
            "-b",
            "-i",
            "-s",
            "--preserve-env",
        ],
        "timeout" => &["-v", "--verbose", "--foreground", "--preserve-status"],
        "setsid" => &["-f", "-w", "-c", "--fork", "--wait", "--ctty"],
        "time" => &["-p", "-v"],
        "exec" => &["-c", "-l"],
        _ => &[],
    };
    known.contains(&flag)
}

/// The shell or code interpreter `xargs` (given its arguments `rest`) runs,
/// if its command word — resolved through any exec-wrapper — is one. The same
/// reader set `analyze_interpreter_stdin_exec` uses: `xargs python3` hands the
/// items to python as argv (a script path, `-c` code) exactly as `xargs sh`
/// does to the shell.
pub(super) fn xargs_shell(rest: &[&str]) -> Option<String> {
    let start = xargs_command_start(rest)?;
    let inner = &rest[start..];
    wrapped_command_heads(inner)
        .into_iter()
        .map(|idx| normalized_command(inner[idx]))
        .find(|head| is_shell(head) || is_code_interpreter(head))
}

/// True when `tok` OPENS a quote it does not close within the same
/// whitespace-delimited token — i.e. the token is the first fragment of a
/// multi-word quoted STRING (`'rm` in `grep -rn 'rm -rf' src/`), not a command
/// word.
///
/// Tokenisation elsewhere splits on whitespace only, so such a fragment
/// unquotes to a bare `rm` and, as a D3 candidate position, would deny ordinary
/// work like `stdbuf --output 0 grep -rn 'rm -rf' src/` — the same false
/// positive CA-blastguard-016 fixed for `find -exec`.
///
/// Not a fail-open: excluding these fragments cannot hide a real payload,
/// because a quoted multi-word string is never a program that exists on disk,
/// and the ways such a string actually gets EXECUTED — `sh -c '…'`, `eval '…'`,
/// `flock -c '…'`, `env -S '…'` — are all re-analysed by their own arms
/// (`dash_c_payloads`, the `eval` arm, `executors::env_split_string_payloads`),
/// which reassemble the full quoted word
/// across token boundaries rather than reading a single fragment. A properly
/// closed quoted command word (`sudo 'rm' -rf /path`) closes within its token
/// and therefore remains a candidate.
pub(super) fn opens_unclosed_quote(tok: &str) -> bool {
    unclosed_quote_char(tok).is_some()
}

/// The quote character `tok` leaves open, if any.
///
/// Callers that must skip the WHOLE quoted string (not merely its first token)
/// need to know which quote to look for when finding the end — see
/// `unknown_wrapper_ask`.
pub(super) fn unclosed_quote_char(tok: &str) -> Option<char> {
    let mut chars = tok.chars();
    match chars.next() {
        Some(q @ ('\'' | '"')) if !chars.any(|c| c == q) => Some(q),
        _ => None,
    }
}

/// Wrappers whose `-c` operand is a SHELL COMMAND STRING (run via `sh -c`),
/// not an opaque option value.
///
/// D2 (verified fail-open, fixed): `flock`'s `-c` / `--command` takes a shell
/// command string and executes it through `sh -c`, exactly like `bash -c`. It
/// used to be listed in `wrapper_flag_takes_value` as a value-taking option, so
/// the payload was SKIPPED as an opaque flag value and never analysed:
/// `flock /tmp/l -c 'rm -rf /Users/yuki/src'` and
/// `flock -c 'rm -rf /Users/yuki/src' /tmp/l` were both ALLOW. The payload is
/// now routed into the same `dash_c_payloads` re-analysis path used for shells.
pub(super) fn takes_shell_command_flag(cmd: &str) -> bool {
    matches!(cmd, "flock")
}

pub(super) fn is_short_flag(tok: &str) -> bool {
    tok.starts_with('-') && !tok.starts_with("--")
}

/// True if any short flag bundle in `rest` contains `ch`, or the long flag is set.
pub(super) fn has_short(rest: &[&str], ch: char) -> bool {
    rest.iter().any(|t| is_short_flag(t) && t.contains(ch))
}

/// True if `tok` is a shell redirection token rather than an operand — either an
/// operator with its target attached (`2>&1`, `>&2`, `2>/dev/null`, `>file`) or a
/// bare operator (`>`, `>>`, `2>`, `&>`). Redirection is punctuation the shell
/// consumes itself; it is never an argument the command can act on.
pub(super) fn is_redirect_token(tok: &str) -> bool {
    tok.contains('>') || tok.contains('<')
}

/// True if `tok` is a redirect operator with NO attached target (`>`, `>>`,
/// `2>`, `&>`), meaning the FOLLOWING token is its target and is likewise not a
/// command operand. Recognised by "made only of redirect punctuation/fd digits
/// AND ending in `>`" — `2>&1` and `>&2` end in a digit and carry their own
/// target, so they consume only themselves.
pub(super) fn is_bare_redirect_op(tok: &str) -> bool {
    tok.ends_with('>')
        && tok
            .chars()
            .all(|c| matches!(c, '>' | '<' | '&' | '0'..='9'))
}

/// True if `rest` contains at least one OPERAND — a token that is neither one of
/// the command's own flags nor shell redirection punctuation. Operands are
/// identified exactly as `analyze_rm` identifies them (`!t.starts_with('-')`) so
/// the two stay in agreement about what counts as a destroyable target.
pub(super) fn has_operand(rest: &[&str]) -> bool {
    let mut i = 0;
    while i < rest.len() {
        let t = rest[i];
        if t.is_empty() {
            i += 1;
        } else if is_bare_redirect_op(t) {
            // Operator plus the target token it owns.
            i += 2;
        } else if is_redirect_token(t) || t.starts_with('-') {
            i += 1;
        } else {
            return true;
        }
    }
    false
}

/// Shells that take a command line as a string argument (e.g. `sh -c "…"`).
///
/// Every call site asks the same question — "does this program RUN code it is
/// handed?" (an egress pipe terminal, a `find -exec` target, a stdin program,
/// a `-c` payload to re-analyse) — so the list is every shell, not only the
/// POSIX ones this module can parse. daf7611b: with only
/// `sh|bash|zsh|ksh|dash` here, `fish -c "rm -rf …"`, `pwsh -c …`,
/// `tcsh -c …` and `csh -c …` matched no arm and were ALLOWED unexamined,
/// while `curl … | fish` was not an egress sink either. The non-POSIX ones
/// are additionally [`is_foreign_shell`], which the `-c` arm turns into an Ask
/// because a POSIX reading of their payload proves nothing.
pub(super) fn is_shell(cmd: &str) -> bool {
    matches!(
        cmd,
        "sh" | "bash"
            | "rbash"
            | "zsh"
            | "ksh"
            | "mksh"
            | "lksh"
            | "oksh"
            | "pdksh"
            | "dash"
            | "ash"
            | "hush"
            | "yash"
            | "posh"
    ) || is_foreign_shell(cmd)
}

/// Shells whose command language is NOT POSIX sh, so this module's analyser
/// cannot read their payloads: fish's `(cmd)` substitution, csh history and
/// modifiers, PowerShell cmdlets (`Remove-Item -Recurse`), nushell pipelines.
/// A POSIX pass over the payload can still find a recognisable `rm -rf` (and
/// Deny), but finding nothing there is "could not analyse", not "clean".
pub(super) fn is_foreign_shell(cmd: &str) -> bool {
    matches!(
        cmd,
        "fish" | "csh" | "tcsh" | "pwsh" | "powershell" | "nu" | "elvish" | "xonsh" | "rc" | "es"
    )
}

/// True unless a foreign shell's arguments are ONLY informational flags
/// (`fish --version`). Any other argument — an inline command flag in any
/// spelling (`-c`, `--command=`, `-C`, `-Command`, `-EncodedCommand`) or a
/// script path — makes it run code blastguard cannot read. Deliberately not a
/// list of the code-running flags: that list is per-shell, abbreviable
/// (PowerShell accepts any unambiguous prefix) and is exactly the kind of list
/// that goes stale.
pub(super) fn foreign_shell_runs_code(rest: &[&str]) -> bool {
    rest.iter().any(|t| {
        !matches!(
            *t,
            "--version" | "-version" | "-Version" | "--help" | "-help" | "-Help" | "-h" | "-?"
        )
    })
}
