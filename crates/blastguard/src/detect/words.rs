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
/// LINE CONTINUATION (backlog 6cac93af round 4): a backslash followed by a
/// newline is REMOVED when unquoted or inside double quotes, as POSIX sh does,
/// so `python3 -c "o\<NL>pen('/etc/hosts','a')"` yields the program text
/// `open('/etc/hosts','a')` that python actually receives. Inside single
/// quotes (and so inside `$'…'`, read here as `$` + a single-quoted run) the
/// pair is kept literally. A real newline is never touched: only the
/// backslash-newline pair joins.
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
                            // POSIX line continuation: `\<newline>` inside
                            // double quotes is removed (`"o\<NL>pen"` is
                            // `open` to the program).
                            Some('\n') => {}
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
                    // POSIX line continuation: an unquoted `\<newline>` is
                    // removed and the word continues on the next line.
                    Some('\n') => {}
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
///   * `=rm` — zsh equals expansion, shell-scoped (see below)
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
///
/// ZSH EQUALS EXPANSION (backlog aa7acd12, user ruling 2026-11-18): Claude
/// Code's Bash tool runs commands with zsh, where option `equals` is on, so an
/// UNQUOTED word `=WORD` is replaced by the path of the program `WORD` found on
/// PATH — `=rm -rf ~` runs `/bin/rm -rf ~`. Such a word resolves to `WORD`
/// here, before every rule, exactly as a quoted spelling does (see
/// [`equals_command_word`]). What follows the `=` is then normalised like any
/// command word, so an expansion there (`=$x`) stays an expansion and is asked
/// about by the callers that check the raw token for one. A quoted or escaped
/// `=` (`'=rm'`, `"=rm"`, `\=rm`) is not expanded by zsh and is left as is.
/// The stripping is shell-scoped: inside text run by bash (`bash -c '=rm …'`)
/// the `=` is kept, since bash does not expand it — see
/// [`equals_command_word`].
pub(super) fn normalized_command(tok: &str) -> String {
    let tok = equals_command_word(tok).unwrap_or(tok);
    let unquoted = first_shell_word(tok).unwrap_or_else(|| tok.to_string());
    basename(&unquoted).to_string()
}

/// The `WORD` of a raw token `=WORD` that zsh's equals expansion turns into the
/// PATH program `WORD` (see [`normalized_command`]); `None` for any other
/// token.
///
/// Only ONE leading `=` is removed, as zsh does: `==rm` names the program
/// `=rm`. A bare `=` names nothing and is left alone.
///
/// The program reached this way is always the one on PATH, never a shell
/// builtin or function of the same name (zsh substitutes its path). Callers
/// that model a builtin's effect on the shell's own state (`cd` moving the
/// working directory) must check this before applying it.
///
/// SHELL-SCOPED (user ruling 2026-11-18): only a shell that performs equals
/// expansion turns `=WORD` into a program path. While the text being judged is
/// run by a shell positively known to be bash ([`equals_scope`] set to
/// `false`, e.g. the payload of `bash -c '…'`), this returns `None`: bash runs
/// a command literally named `=WORD` (not found). Everywhere else — the Bash
/// tool's own top level (zsh), `zsh -c`, `sh -c` and any shell whose identity
/// is not known — the `=` is stripped (the strict side).
pub(super) fn equals_command_word(tok: &str) -> Option<&str> {
    if !ZSH_EQUALS_ACTIVE.with(std::cell::Cell::get) {
        return None;
    }
    tok.strip_prefix('=').filter(|rest| !rest.is_empty())
}

thread_local! {
    /// Whether the shell running the text currently being judged performs zsh
    /// equals expansion (see [`equals_command_word`]). `true` (strip `=`) is
    /// the default and the fail-closed value; only [`equals_scope`] changes it,
    /// and only for the duration of its guard.
    static ZSH_EQUALS_ACTIVE: std::cell::Cell<bool> = const { std::cell::Cell::new(true) };
}

/// Guard returned by [`equals_scope`]; restores the previous setting on drop
/// (also while unwinding), so a scope can never leak into later analysis.
pub(super) struct EqualsScope {
    prev: bool,
}

impl Drop for EqualsScope {
    fn drop(&mut self) {
        ZSH_EQUALS_ACTIVE.with(|c| c.set(self.prev));
    }
}

/// Judge text run by a shell that does (`true`) or does not (`false`) perform
/// zsh equals expansion until the returned guard is dropped.
pub(super) fn equals_scope(active: bool) -> EqualsScope {
    EqualsScope {
        prev: ZSH_EQUALS_ACTIVE.with(|c| c.replace(active)),
    }
}

/// The current equals-expansion setting (see [`equals_scope`]); used by
/// re-analysis of text that the SAME shell runs (`eval`, `exec`, `source`).
pub(super) fn zsh_equals_active() -> bool {
    ZSH_EQUALS_ACTIVE.with(std::cell::Cell::get)
}

/// Whether the shell named `shell` (a normalised command word for which
/// [`is_shell`] holds) expands `=WORD`. Only bash is positively known not to;
/// every other shell, including ones whose identity is unknown (`sh` may be
/// zsh on macOS), is treated as expanding — the strict side.
pub(super) fn shell_expands_equals(shell: &str) -> bool {
    shell != "bash"
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
/// "Every position" means every position where a shell WORD STARTS
/// ([`wrapped_word_starts`]): a whitespace-split token strictly inside a
/// quoted word whose quotes provably close is part of one argv entry, never a
/// command word the wrapper could exec (backlog b56a5813).
///
/// When the first non-assignment token is NOT a wrapper, exactly one candidate
/// is returned, so ordinary commands are unaffected.
///
/// A leading assignment WORD whose value is a command substitution spanning
/// several tokens ([`substitution_assignment_word_span`]:
/// `y=$(L=/tmp/x; true) rm $L`) contributes two candidate sets: the body's
/// own (its first command word, kept for backlog a03fe3cd) AND the outer
/// command's after the whole word (backlog d068ef20: before, the body's
/// tokens were the only candidates and the outer `rm` was never judged).
/// When that word's end is undetermined the former reading applies unchanged.
pub(super) fn command_candidates(tokens: &[&str]) -> Vec<usize> {
    candidates_from(tokens, true)
}

/// [`command_candidates`] without the candidates that start INSIDE a leading
/// assignment's command substitution: the command word(s) of the simple
/// command itself. For a rule about the shape of that ONE command
/// ([`super::unknown_wrapper_ask`]); a substitution body is judged as its own
/// command line by `executed_substitution_payloads`. Identical to
/// [`command_candidates`] when no leading assignment word spans a
/// substitution.
pub(super) fn outer_command_candidates(tokens: &[&str]) -> Vec<usize> {
    candidates_from(tokens, false)
}

/// Every position where a program word stands as parsed: the
/// [`command_candidates`] AND the head word itself. Behind a wrapper
/// (`env`, `nohup`, `sudo` …) `command_candidates` returns only the words
/// AFTER the wrapper, so the wrapper at the head is not among them; a rule
/// about the wrapper itself (`env -S`, daf7611b) needs both. A token spelled
/// like a wrapper inside a quoted argument is in neither set.
pub(super) fn program_word_positions(tokens: &[&str]) -> Vec<usize> {
    let mut out = command_candidates(tokens);
    let mut i = 0;
    while i < tokens.len() {
        let t = tokens[i];
        if t.is_empty() {
            i += 1;
            continue;
        }
        if is_assignment(t) {
            i += substitution_assignment_word_span(tokens, i)
                .unwrap_or_else(|| literal_assignment_span(tokens, i));
            continue;
        }
        if !out.contains(&i) {
            out.push(i);
        }
        break;
    }
    out
}

fn candidates_from(tokens: &[&str], with_bodies: bool) -> Vec<usize> {
    let mut i = 0;
    while i < tokens.len() {
        let t = tokens[i];
        if t.is_empty() {
            i += 1;
            continue;
        }
        if is_assignment(t) {
            if let Some(span) = substitution_assignment_word_span(tokens, i) {
                let end = i + span;
                let mut out: Vec<usize> = if with_bodies {
                    candidates_from(&tokens[i + 1..end], true)
                        .into_iter()
                        .map(|k| k + i + 1)
                        .collect()
                } else {
                    Vec::new()
                };
                out.extend(
                    candidates_from(&tokens[end..], with_bodies)
                        .into_iter()
                        .map(|k| k + end),
                );
                return out;
            }
            i += literal_assignment_span(tokens, i);
            continue;
        }
        if is_exec_wrapper(&normalized_command(t)) {
            return wrapped_word_starts(tokens, i + 1);
        }
        return vec![i];
    }
    Vec::new()
}

/// The D3 candidate positions behind a wrapper, from `tokens[from]` on: the
/// first token of every shell word, minus a word's first token that opens a
/// quote it does not close ([`opens_unclosed_quote`], unchanged).
///
/// Backlog b56a5813: the candidates used to be EVERY non-empty token, so the
/// fragments INSIDE one quoted argument were read as command words.
/// `env -u VAR backlog add --title "x: python3 -c 'open(\"/etc/f\",\"w\")'
/// ok"` made the fragment `python3` a candidate, its neighbours `-c
/// 'open(\"/etc/f\",\"w\")'` an inline program, and the line was Denied as
/// an interpreter write of `/etc/f\'` — text no process ever runs; the same
/// argument without the wrapper (one candidate, `backlog`) was Allow.
///
/// A word is skipped whole only when [`super::payload::shell_word_token_span`]
/// shows where it ends. When it cannot (a quote still open at the end of the
/// tokens) that word's first token is skipped as before and every LATER token
/// stays a candidate — the undetermined case keeps the wider, restrictive
/// candidate set. Not a fail-open for the same reason given on
/// [`opens_unclosed_quote`]: a quoted multi-word argument is never the program
/// a wrapper execs, and the wrappers that RUN a string (`env -S`, `flock -c`,
/// `sh -c` behind any wrapper) are re-analysed by their own arms over the
/// reassembled word.
fn wrapped_word_starts(tokens: &[&str], from: usize) -> Vec<usize> {
    let mut out = Vec::new();
    let mut j = from;
    while j < tokens.len() {
        let t = tokens[j];
        if t.is_empty() {
            j += 1;
            continue;
        }
        if !opens_unclosed_quote(t) {
            out.push(j);
        }
        j += super::payload::shell_word_token_span(tokens, j).unwrap_or(1);
    }
    out
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

/// Number of whitespace-split tokens, from `tokens[i]` on, that make up ONE
/// shell word when `tokens[i]` is an assignment whose value opens a command
/// substitution (`$(` or a backtick, optionally behind a leading `"`) that
/// `tokens[i]` itself does not close — `y=$(L=/tmp/x;` `true)` is one word,
/// and so is its split form `y=$(` `L=/tmp/x;` `true)` from
/// [`super::split_substitution_opening_assignments`]. `None` when `tokens[i]`
/// is no such opener (including `$((` arithmetic and a substitution closed
/// inside the token) or when the word's end is undetermined: the substitution
/// never closes in `tokens`, or a quote / nested substitution after it never
/// does.
///
/// Backlog d068ef20: a prefix assignment whose substitution body holds
/// whitespace was read as several words, every one of them an assignment or
/// body text, so the OUTER command after the word (`rm $L` in
/// `y=$(L=/tmp/x; true) rm $L`) was never a command candidate and the line
/// was Allow. The tokens joined by one space are scanned with the same
/// substitution scan [`super::split_segments`] uses
/// ([`super::closed_substitution_end`]), and the word's end maps back to a
/// token count only when it falls exactly on a token's end (never past the
/// slice; anything else is `None`).
fn substitution_assignment_word_span(tokens: &[&str], i: usize) -> Option<usize> {
    use super::segment::{closed_substitution_end, quote_escape_pair};
    let tok = *tokens.get(i)?;
    if !is_assignment(tok) {
        return None;
    }
    let eq = tok.find('=')?;
    let value = &tok[eq + 1..];
    let quote = usize::from(value.starts_with('"'));
    let after_quote = &value[quote..];
    if !(after_quote.starts_with("$(") || after_quote.starts_with('`'))
        || after_quote.starts_with("$((")
    {
        return None;
    }
    let joined = tokens[i..].join(" ");
    let chars: Vec<char> = joined.chars().collect();
    let open = tok[..eq + 1 + quote].chars().count();
    let close = closed_substitution_end(&chars, open)?;
    if close < tok.chars().count() {
        // Closed inside its own token: the one-token reading already holds.
        return None;
    }
    // The word continues after the closer up to the first blank outside
    // quotes; a leading `"` is still open there.
    let (mut in_s, mut in_d) = (false, quote == 1);
    let mut k = close + 1;
    while k < chars.len() {
        let c = chars[k];
        if quote_escape_pair(&chars, k, in_s, in_d).is_some() {
            k += 2;
            continue;
        }
        if c == '\'' && !in_d {
            in_s = !in_s;
        } else if c == '"' && !in_s {
            in_d = !in_d;
        } else if !in_s && matches!(c, '$' | '`') {
            if c == '`' || chars.get(k + 1) == Some(&'(') {
                k = closed_substitution_end(&chars, k)? + 1;
                continue;
            }
        } else if c.is_whitespace() && !in_s && !in_d {
            break;
        }
        k += 1;
    }
    if in_s || in_d {
        return None;
    }
    // Map the word's end back onto `tokens[i..]` by each token's own end
    // offset in `joined`. A caller's tokens need not be whitespace-free (a
    // token holding a blank made a count of blanks overshoot the slice and
    // panic, d068ef20 integration), so the end must coincide EXACTLY with a
    // token end; anything else is undetermined and keeps the former reading.
    let mut token_end = 0;
    for (n, t) in tokens[i..].iter().enumerate() {
        token_end += t.chars().count();
        if token_end == k {
            return Some(n + 1);
        }
        if token_end > k {
            return None;
        }
        token_end += 1;
    }
    None
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
