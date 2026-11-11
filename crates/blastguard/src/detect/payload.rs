use super::{
    backtick_body_as_run, command_candidates, first_line_quoted_here_document_body,
    first_shell_word, here_document_delimiter_is_quoted, here_string_spans,
    interpreter_code_verdict, is_assignment, is_bare_redirect_op, is_code_interpreter,
    is_exec_wrapper, is_foreign_shell, is_redirect_token, is_shell, is_short_flag,
    normalized_command, opens_here_document, quoted_string_literals, recommend_against,
    scan_balanced, split_statements_into_pipe_stages, wrapped_command_heads, xargs_command_start,
    xargs_reads_arg_file, xargs_shell, Ctx, Decision, Stop, VerdictAcc,
};

/// True if this stage names a program FILE for the interpreter to run, rather
/// than taking its program from stdin. Flags, redirections and a bare `-` are
/// not program files.
fn stage_has_program_operand(rest: &[&str]) -> bool {
    let mut skip_next = false;
    for tok in rest {
        if skip_next {
            skip_next = false;
            continue;
        }
        if is_bare_redirect_op(tok) {
            skip_next = true;
            continue;
        }
        if is_redirect_token(tok) || tok.starts_with('-') {
            continue;
        }
        return true;
    }
    false
}

/// Cross-segment closure of the stdin mirrors of an inline-eval invocation.
///
/// [`analyze_command_at`] judges ONE segment from its tokens alone, so two
/// spellings escape it entirely: the here-document BODY (which lives on the
/// lines after the segment) and the pipe (`echo … | python3`, where the
/// interpreter stage carries no operand at all and so looks like a bare REPL).
/// Both hand a program to an interpreter exactly as `-c` does, and both were
/// `Allow` — while the same file's [`stage_is_interpreter_terminal`] already
/// called `… | python3` a code-exec sink in the egress path. This runs over the
/// FULL command text, where the body and the upstream stage are both visible.
pub(super) fn analyze_interpreter_stdin_exec(cmd: &str, depth: usize, ctx: &Ctx<'_>) -> Decision {
    let heredoc_open = opens_here_document(cmd);
    let mut acc = VerdictAcc::default();
    // What `echo`/`printf` write to a file another stage runs
    // (`echo 'rm -rf ~' > x.rs && sh x.rs`); see `echo_script_file`.
    let echoed = super::echo_script_file::echoed_script_verdict(cmd, depth, ctx);
    if let Some(deny) = acc.record(echoed) {
        return deny;
    }
    for statement in split_statements_into_pipe_stages(cmd) {
        for (i, stage) in statement.iter().enumerate() {
            // The here-string mirror (`bash <<< 'rm -rf ~'`), judged before
            // and independently of the stdin-program rule below, which it can
            // only add to. See `shell_here_string_verdict`.
            if let Some(deny) = acc.record(shell_here_string_verdict(stage, depth, ctx)) {
                return deny;
            }
            // A here-string `cat`/`tee` writes to a file a shell may run
            // (`cat > x.rs <<< 'rm -rf ~' && sh x.rs`); see `here_string_file`.
            let written =
                super::here_string_file::written_here_string_verdict(cmd, stage, depth, ctx);
            if let Some(deny) = acc.record(written) {
                return deny;
            }
            let tokens: Vec<&str> = stage.split_whitespace().collect();
            // The interpreter may sit behind an exec-wrapper (`env sh`,
            // `env -i bash`, `/usr/bin/env python3`, `command sh`): reading
            // token 0 alone took `env` for the program and let
            // `echo "rm -rf /" | env sh` through unjudged, while the egress
            // twin `stage_is_interpreter_terminal` and the here-string twin
            // `shell_here_string_verdict` already resolve the wrapper.
            //
            // Resolved with `wrapped_command_heads`, NOT `command_candidates`:
            // the latter makes every token behind a wrapper a candidate, so
            // grep's OPERAND in `echo x | env grep bash` was taken for the
            // reader and the line asked. Only a head the wrapper chain can
            // actually exec counts here.
            let heads = wrapped_command_heads(&tokens);
            // `… | xargs sh`: xargs turns its stdin into ARGUMENTS of the
            // shell it runs (`sh rm -rf /` runs a script file named `rm`, or
            // `sh -c {}` runs the item as a command line). What executes is the
            // piped text, re-parsed as argv, which no reading here models.
            //
            // Fed by a pipe or here-document, by a `<` redirect, or by an
            // argument file (`-a FILE` / `--arg-file`, whose value
            // `xargs_command_start` consumes): none of those items is on the
            // command line. `xargs -a FILE sh` was Allow while this was gated
            // on `i > 0 || heredoc_open` alone. A bare `xargs sh` at the head
            // of a statement reads the hook's inherited stdin and is left to
            // the other rules (the nesting-cap Ask in `sh -c xargs sh -c …`
            // must stay the reported reason).
            let stdin_redirect = tokens
                .iter()
                .any(|t| t.starts_with('<') || t.starts_with("0<"));
            if let Some(inner) = heads
                .iter()
                .filter(|&&idx| normalized_command(tokens[idx]) == "xargs")
                .find_map(|&idx| {
                    let rest = &tokens[idx + 1..];
                    let opts = &rest[..xargs_command_start(rest).unwrap_or(rest.len())];
                    let fed = i > 0 || heredoc_open || stdin_redirect || xargs_reads_arg_file(opts);
                    if fed {
                        xargs_shell(rest)
                    } else {
                        None
                    }
                })
            {
                if let Some(deny) = acc.record(Decision::ask(format!(
                    "`{}` hands its input items to `{inner}` as arguments — the command that runs \
is built from text (stdin, a redirect or an `-a` file) that is not on the command line, so \
blastguard cannot read it",
                    stage.trim()
                ))) {
                    return deny;
                }
            }
            let Some(idx) = heads.into_iter().find(|&idx| {
                let head = normalized_command(tokens[idx]);
                is_shell(&head) || is_code_interpreter(&head)
            }) else {
                continue;
            };
            let rest = &tokens[idx + 1..];
            // `osascript` removing files through Finder / System Events, however
            // the program arrives — judged before the program-file test, since
            // `osascript -l JavaScript <<'E'` reads `JavaScript` as one.
            if normalized_command(tokens[idx]) == "osascript" && (i > 0 || heredoc_open) {
                let texts = stdin_program_texts(cmd, stage, &statement, i, heredoc_open);
                if let Some(deny) = super::interpreter::osascript_program_delete(&texts) {
                    return deny;
                }
            }
            let has_program_file = stage_has_program_operand(rest);
            let takes_stdin_program = interpreter_stdin_operand(rest).is_some()
                || (!has_program_file && (i > 0 || heredoc_open));
            if !takes_stdin_program {
                continue;
            }
            // The program is whatever the pipeline or the here-document
            // delivers, so the whole command line is the text to audit — but
            // it must NOT be handed back to `analyze_shell_payload`, which
            // re-enters this very function on the same string and overflows the
            // stack. Everything gathered below is a strict SUBSTRING of `cmd`,
            // so the descent terminates.
            //
            // Only the program this stage actually receives is audited — not
            // the whole command line. Scanning the line let text from an
            // unrelated statement become the finding: `rmdir olddir; echo hi |
            // python3` was denied for "runs `rmdir`" when the program was `hi`.
            // A reason must name what was observed about THIS program.
            let program_texts = stdin_program_texts(cmd, stage, &statement, i, heredoc_open);
            let shape = format!("`{}` takes its program from stdin", stage.trim());
            let python = python_stdin_reading(cmd, tokens[idx], &program_texts, depth, ctx);
            if let Some(deny) = acc.record(interpreter_code_verdict(
                &shape,
                &program_texts,
                depth,
                ctx,
                !here_document_delimiter_is_quoted(cmd),
                false,
                python.as_ref().is_some_and(|(_, data)| *data),
            )) {
                return deny;
            }
            // The Python judge's reading of the same body adds its SYSTEM-HARM
            // Deny only (user ruling 2026-11-07: system harm is Deny through
            // any wrapper); every other verdict stays the scan's above (see
            // `python_stdin_reading`).
            if let Some((verdict, _)) = python.filter(|(v, _)| v.is_system_harm()) {
                return verdict;
            }
            // The quoted literals above are NOT everything the upstream stage
            // emits: an UNQUOTED expansion beside them (`echo "a" $(ls) | sh`)
            // is dropped by `quoted_string_literals`, so the program judged was
            // `a` alone and the line was allowed — while `echo "a $(ls)" | sh`,
            // whose substitution sits inside the literal, asked. The output of
            // that expansion is part of the program and is not on the command
            // line. Recorded after the literal verdict so a Deny found there
            // still wins.
            if i > 0 {
                if let Some(word) =
                    unquoted_expansion(&backticks_as_dollar_paren(&statement[i - 1]))
                {
                    if let Some(deny) = acc.record(Decision::ask(format!(
                        "{shape}, and the stage piping into it emits the output of an unquoted \
shell expansion (`{word}`) — that output is the program, it is not on the command line, so \
blastguard cannot tell whether anything here is irreversible"
                    ))) {
                        return deny;
                    }
                }
                // The quoted literals are the WHOLE program only when the
                // upstream stage emits nothing but its own arguments (`echo`,
                // `printf`). Any other upstream — `grep -E "a" f`, `jq -r . f`,
                // `tr a b < f` — emits text from a file or from stdin, and its
                // quoted words are a pattern or a filter, not the program:
                // `grep -E "a" f | sh` judged `a` as the program and was
                // allowed, while `cat f | sh` (no literal at all) asked.
                // Recorded after the literal verdict so a Deny found in a
                // literal still wins.
                if !stage_emits_only_its_arguments(&statement[i - 1]) {
                    if let Some(deny) = acc.record(Decision::ask(format!(
                        "{shape}, and the stage piping into it (`{}`) is not a literal emitter \
(`echo` / `printf`) — what it emits can include text from a file or from stdin, which is not on \
the command line, so blastguard cannot read the program",
                        statement[i - 1].trim()
                    ))) {
                        return deny;
                    }
                }
            }
        }
    }
    acc.finish()
}

/// The Python judge's reading of a QUOTED here-document body that `head` (a
/// bare `python` / `python3` / `python3.N`) runs as its program
/// ([`super::heredoc::python_stdin_here_document`]'s shape), as
/// `(verdict, strings_are_data)`; `None` for any other stage or shape, or a
/// body that does not tokenise as Python (then only the program-text scan
/// judges it, exactly as before).
///
/// The verdict ([`super::interpreter::python_stdin_body_reading`]) places
/// the body's relative paths after the `cd` that runs before the reader, so
/// `cd /etc && python3 - <<'EOF'` / `open('hosts','w')` is judged as
/// `cd /etc && : > hosts` (tests/heredoc_system_harm_holes_20261111.rs case
/// A); the program-text scan reads no `cd`. The caller takes that verdict
/// only when it is a system-harm Deny ([`Decision::is_system_harm`]): a write
/// into a tracked file of another work tree, which the shell rules deny as
/// out of scope, stays the program-text scan's to judge
/// (tests/subagent_heredoc_fp_20261108.rs `control_cd_python_stdin_no_arrow`
/// pins Allow there). `strings_are_data` is true only
/// when `program_texts` is exactly that body and the parser showed none of
/// its strings reaches a shell or a process (case G).
fn python_stdin_reading(
    cmd: &str,
    head: &str,
    program_texts: &[String],
    depth: usize,
    ctx: &Ctx<'_>,
) -> Option<(Decision, bool)> {
    let python = normalized_command(head)
        .strip_prefix("python")
        .is_some_and(|v| {
            v.is_empty()
                || v == "3"
                || v.strip_prefix("3.")
                    .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
        });
    if !python {
        return None;
    }
    let parts = super::heredoc::python_stdin_here_document(cmd)?;
    let reading = super::interpreter::python_stdin_body_reading(&parts, depth, ctx)?;
    let body = parts.body.strip_suffix('\n').unwrap_or(&parts.body);
    let exact = matches!(program_texts, [only] if only == body);
    Some((reading.verdict, exact && reading.strings_are_data))
}

/// The program text an interpreter stage receives on stdin: the
/// here-document body and/or the literals the upstream `echo`/`printf` stage
/// of THIS statement emits. Shared by the stdin-program audit of
/// [`analyze_interpreter_stdin_exec`] and its `osascript` application-delete
/// check.
fn stdin_program_texts(
    cmd: &str,
    stage: &str,
    statement: &[String],
    i: usize,
    heredoc_open: bool,
) -> Vec<String> {
    let mut program_texts: Vec<String> = Vec::new();
    if heredoc_open {
        // The here-document body. When THIS stage opens a quoted
        // here-document on the first line and its closing line is
        // found ([`first_line_quoted_here_document_body`]), the body is
        // exactly the lines in between: the lines after the closing
        // delimiter are ordinary shell, judged by every other rule, and
        // a quoted `>` there (`echo '>'`) is not this program's text
        // (backlog aafeaa05). On any doubt it stays everything after
        // the first newline, the restrictive over-approximation.
        if let Some(body) = first_line_quoted_here_document_body(cmd, stage) {
            program_texts.push(body.to_string());
        } else if let Some((_, body)) = cmd.split_once('\n') {
            program_texts.push(body.to_string());
        }
    }
    if i > 0 {
        // Piped in: the program is whatever the upstream stage of THIS
        // statement emits, which is readable when it emits a literal.
        // A backtick there is a command SUBSTITUTION, not a quote: the
        // stage emits the body's OUTPUT, not its text. Read as a quote,
        // `echo \`ls\` | sh` offered `ls` as the program and was
        // allowed, while `echo $(ls) | sh` asked. Spelled as its
        // `$( )` twin, it gets the twin's verdict.
        program_texts.extend(quoted_string_literals(&backticks_as_dollar_paren(
            &statement[i - 1],
        )));
    }
    program_texts
}

/// True when `stage` emits only text spelled in its own arguments: its
/// command, behind any exec-wrapper chain, is `echo` or `printf`. Every head
/// candidate [`wrapped_command_heads`] yields must be a wrapper or one of
/// those two, so an ambiguous chain resolves to "not a literal emitter" (the
/// restrictive side). A stage that is not a simple command (`(cat f)`,
/// `{ cat f; }`) has no such head and is likewise not a literal emitter.
fn stage_emits_only_its_arguments(stage: &str) -> bool {
    let tokens: Vec<&str> = stage.split_whitespace().collect();
    let heads = wrapped_command_heads(&tokens);
    let Some(&last) = heads.last() else {
        return false;
    };
    let is_emitter =
        |idx: usize| matches!(normalized_command(tokens[idx]).as_str(), "echo" | "printf");
    is_emitter(last)
        && heads
            .iter()
            .all(|&idx| is_emitter(idx) || is_exec_wrapper(&normalized_command(tokens[idx])))
}

/// The first shell expansion in `stage` that sits OUTSIDE every quote — a
/// `$(`, `${`, `$NAME` / special parameter, or a backtick — as a short sample
/// of the text from that point. `None` when every expansion is inside quotes
/// (those reach the program through [`quoted_string_literals`]) or there is
/// none. An unquoted backslash-escaped `$` or backtick COUNTS: the escape only
/// stops THIS stage from expanding it, so the stage emits a literal `$(ls)` /
/// `` `ls` `` that the shell reading the pipe then runs (`echo "a" \$(ls) | sh`
/// executes `ls`). `$'…'` / `$"…"` are quoting, not expansion. An unclosed
/// backtick counts: what it emits cannot be read either.
///
/// PATHNAME and TILDE expansion count too: an unquoted `*`, `?` or `[` turns
/// into whatever file names exist when the line runs, and a word-initial `~`
/// into a home directory, so `echo "a" * | sh` hands the shell a program made
/// of file names that are not on the command line.
fn unquoted_expansion(stage: &str) -> Option<String> {
    let chars: Vec<char> = stage.chars().collect();
    let (mut in_s, mut in_d) = (false, false);
    let mut k = 0;
    while k < chars.len() {
        let c = chars[k];
        if c == '\\' && !in_s {
            // An escaped glob or tilde is likewise emitted literally and
            // expanded by the reading shell (`echo "a" \* | sh`).
            if !in_d && matches!(chars.get(k + 1), Some('$' | '`' | '*' | '?' | '[' | '~')) {
                return Some(chars[k..].iter().take(40).collect());
            }
            k += 2;
            continue;
        }
        if c == '\'' && !in_d {
            in_s = !in_s;
        } else if c == '"' && !in_s {
            in_d = !in_d;
        } else if !in_s && !in_d {
            let expands = match c {
                '`' | '*' | '?' | '[' => true,
                '~' => k == 0 || chars[k - 1].is_whitespace(),
                '$' => chars.get(k + 1).is_some_and(|n| {
                    matches!(n, '(' | '{' | '_' | '@' | '*' | '#' | '?' | '$' | '!' | '-')
                        || n.is_ascii_alphanumeric()
                }),
                _ => false,
            };
            if expands {
                return Some(chars[k..].iter().take(40).collect());
            }
        }
        k += 1;
    }
    None
}

/// `stage` with every closed, unescaped backtick substitution outside single
/// quotes rewritten as the `$( )` substitution it is equivalent to. Used where
/// a reader treats a backtick as a quote character ([`quoted_string_literals`])
/// but the text is a shell stage, in which a backtick runs its body. An
/// unclosed backtick is left as written.
fn backticks_as_dollar_paren(stage: &str) -> String {
    let chars: Vec<char> = stage.chars().collect();
    let mut out = String::with_capacity(stage.len() + 8);
    let (mut in_s, mut in_d) = (false, false);
    let mut k = 0;
    while k < chars.len() {
        let c = chars[k];
        if c == '\\' && !in_s && k + 1 < chars.len() {
            out.push(c);
            out.push(chars[k + 1]);
            k += 2;
            continue;
        }
        if c == '\'' && !in_d {
            in_s = !in_s;
        } else if c == '"' && !in_s {
            in_d = !in_d;
        } else if c == '`' && !in_s {
            if let Some(end) = scan_balanced(&chars, k + 1, Stop::Backtick, 0) {
                out.push_str("$(");
                out.push_str(&backtick_body_as_run(&chars[k + 1..end]));
                out.push(')');
                k = end + 1;
                continue;
            }
        }
        out.push(c);
        k += 1;
    }
    out
}

/// A here-string fed to a SHELL is shell code: `bash <<< 'rm -rf ~'` runs
/// `rm -rf ~` exactly as `echo 'rm -rf ~' | sh` does, and the pipe spelling was
/// already denied by the stdin-mirror rule in
/// [`analyze_interpreter_stdin_exec`] while the here-string spelling reached
/// Allow (observed 2026-10-14 on 0.2.109). [`analyze_here_string_egress`] only
/// looks for a fetch/decode in the operand, so nothing judged what the operand
/// itself runs.
///
/// Each operand is judged by the same [`interpreter_code_verdict`] the pipe
/// spelling goes through (which shell-analyses the text), whether or not the
/// shell also names a script file or `-c` payload: a shell's stdin can still be
/// read as code from there (`bash -c 'source /dev/stdin' <<< …`), and telling
/// a script operand from an option value is a model of option parsing this file
/// has repeatedly got wrong. Judging the operand regardless can only add a
/// verdict — the deny-if-any widening `dash_c_payloads` already uses.
///
/// What cannot be read statically resolves to Ask, never Allow (CLAUDE.md §3):
///   * a double-quoted or bare operand containing `$`/backtick — the OUTER
///     shell expands it, so the text here is a template, not the script
///     (`interpreter_code_verdict`'s `expansion_possible` guard);
///   * a bare operand containing a quote or backslash — the shell joins and
///     unescapes such a word in ways this scan does not model (`rm' -rf ~'`
///     is ONE word), so the operand recorded here is not what runs;
///   * a non-POSIX shell (`fish`, `pwsh`, …), whose language a POSIX reading
///     cannot clear ([`is_foreign_shell`]).
///
/// Every Ask this function returns carries [`RECOMMEND_NO`] (user rulings R1
/// and R0, 2026-10-14): `bash <<<"$(…)"` runs code that only exists at run
/// time, so the recommended answer is not to run it. A literal destructive
/// operand (`bash <<< 'rm -rf ~'`) is still a Deny.
///
/// A SINGLE-quoted operand is passed verbatim, so its text IS the script, and a
/// `$(…)` in it is ordinary shell syntax judged by the shell analysis itself.
///
/// The shell is looked for in the stage with every here-string removed, so a
/// glued `bash<<<'…'` still names `bash`, and through [`command_candidates`] so
/// a wrapper (`sudo bash <<< …`) does not hide it.
fn shell_here_string_verdict(stage: &str, depth: usize, ctx: &Ctx<'_>) -> Decision {
    let spans = here_string_spans(stage);
    if spans.is_empty() {
        return Decision::Allow;
    }
    let chars: Vec<char> = stage.chars().collect();
    let mut remainder = String::new();
    let mut at = 0;
    for span in &spans {
        remainder.extend(&chars[at.min(span.start)..span.start]);
        remainder.push(' ');
        at = span.end;
    }
    remainder.extend(&chars[at.min(chars.len())..]);
    let tokens: Vec<&str> = remainder.split_whitespace().collect();
    let Some(shell) = command_candidates(&tokens)
        .into_iter()
        .map(|idx| normalized_command(tokens[idx]))
        .find(|head| is_shell(head))
    else {
        return Decision::Allow;
    };
    let shape = format!("`{shell}` reads a here-string as its script");
    let mut acc = VerdictAcc::default();
    for span in &spans {
        if span.bare && span.operand.contains(['\'', '"', '\\']) {
            if let Some(deny) = acc.record(Decision::ask(format!(
                "{shape}, and that operand (`{}`) is a quoted/escaped word blastguard does not \
unquote — it cannot tell what text the shell will run",
                span.operand
            ))) {
                return deny;
            }
            continue;
        }
        let verdict = interpreter_code_verdict(
            &shape,
            std::slice::from_ref(&span.operand),
            depth,
            ctx,
            !span.single_quoted,
            false,
            false,
        );
        if let Some(deny) = acc.record(verdict) {
            return deny;
        }
    }
    if is_foreign_shell(&shell) {
        if let Some(deny) = acc.record(Decision::ask(format!(
            "{shape}, and `{shell}` is not a POSIX shell — blastguard's reading of the script \
proves nothing about what it does"
        ))) {
            return deny;
        }
    }
    // User rulings R1/R0 (2026-10-14): every Ask reached here is a shell about
    // to run a here-string blastguard could not clear, so it carries
    // `RECOMMEND_NO`. A Deny returned above is unaffected.
    recommend_against(acc.finish())
}

pub(super) fn interpreter_stdin_operand<'a>(rest: &[&'a str]) -> Option<&'a str> {
    rest.iter()
        .find(|t| {
            matches!(
                **t,
                "-" | "/dev/stdin" | "/dev/fd/0" | "/proc/self/fd/0" | "/dev/tty"
            )
        })
        .copied()
}

/// Strip one layer of matching surrounding quotes from a reconstructed
/// argument string. Tokenisation has already split on whitespace, so a quoted
/// payload like `"rm -rf /"` arrives as `"rm -rf /"` — peel the wrapper so the
/// inner command line can be re-analysed.
pub(super) fn strip_wrapping_quotes(s: &str) -> &str {
    let s = s.trim();
    let bytes = s.as_bytes();
    if bytes.len() >= 2 {
        let (first, last) = (bytes[0], bytes[bytes.len() - 1]);
        if (first == b'"' || first == b'\'') && first == last {
            return &s[1..s.len() - 1];
        }
    }
    s
}

/// For `<shell> … -c <payload>`, return every candidate payload command line to
/// re-analyse. Returns an empty vec when there is no `-c` flag (e.g.
/// `bash script.sh`, whose file we cannot inspect).
///
/// CA-blastguard-012 (verified bypass): this used to JOIN all words after `-c`
/// and then peel quotes only when the JOINED string began and ended with the
/// same quote char. Appending ANY trailing token defeated that —
/// `sh -c "rm -rf /some/path" --help` (or `; true`, or a bare `arg0`) left the
/// last char belonging to the trailing token, nothing was peeled, the payload's
/// command word parsed as the literal `"rm`, and analysis gave up: ALLOW.
/// Trailing tokens are ordinary shell usage (`sh -c '…' scriptname arg1`), where
/// they become `$0`, `$1`, … and are NOT executed — so the operand must be read
/// as a single properly-quoted shell WORD, independent of what follows it.
///
/// Two candidates are returned, both analysed, deny-if-either:
///   1. the first shell word after `-c` — the real, faithful shell semantics;
///   2. the legacy join-then-peel string, when it differs — so an UNQUOTED
///      multi-word payload (`sh -c rm -rf /path`) keeps being denied exactly as
///      before. (Real `sh` would only run `rm` there, but the historical
///      denial is the conservative side of the trade and costs no realistic
///      false positive.)
///
/// CA-blastguard-014 (verified bypass, fail-open): the word after `-c` is not
/// necessarily the payload. A shell keeps parsing OPTIONS until the first
/// non-option operand, so `bash -c -- 'rm -rf /some/path'`, `sh -c -- "rm …"`
/// and `bash -c -x 'rm …'` all really execute the payload while the old code
/// read `--` / `-x` as the "first shell word" and gave up: ALLOW (verified:
/// `bash -c -- 'echo RAN'`, `sh -c -- 'echo RAN'` and `bash -c -x 'echo RAN'`
/// all print RAN).
///
/// BG-1 (incomplete fix): the CA-blastguard-014 scan skipped `-`-prefixed
/// tokens and stopped at the first token that did not start with `-`. For a
/// VALUE-TAKING option that token is the option's ARGUMENT, not the payload —
/// `bash -c -o pipefail 'rm -rf /some/path'` stopped on `pipefail`, and `+o` /
/// `+O` were not recognised as options at all (`bash -c +o history 'rm …'`).
/// All of these really execute the payload; all were ALLOW. The old docstring's
/// claim to "mirror the shell's own option parsing" was therefore false.
///
/// The fix deliberately does NOT model option parsing more precisely — that is
/// the "reason about token positions" approach that has regressed this file
/// three rounds running, and every value-taking option added later (`-o`, `-O`,
/// `+o`, `--rcfile`, `--init-file`, …) would reopen the same gap. Instead it
/// pushes a candidate for EVERY position after `-c`. The candidate set is
/// additive and evaluated deny-if-ANY, so extra candidates can only ever WIDEN
/// detection: whichever position the real shell picks as the command string is
/// guaranteed to be among them, however the intervening options parse.
///
/// Widening justification (no false positive of consequence): a candidate at a
/// non-payload position is re-analysed by the full pipeline, so it denies only
/// if that text is ITSELF a destructive command line. The residue is a token
/// that is both an option's value and a destructive command line (e.g.
/// `bash -c 'echo hi' 'rm -rf /path'`, where the second string is `$0`, not
/// code) — a deny there is a false positive, but a recoverable one, and it is
/// the deliberate side of this module's fail-closed bias for shell-eval
/// wrappers.
///
/// Fail-open condition: the real payload's first shell word is not derivable
/// from any suffix of `rest` — e.g. the payload arrives through a variable
/// (`bash -c "$CMD"`) or is assembled at runtime. That is unchanged by this fix
/// and out of reach of any static analysis of the command line.
pub(super) fn dash_c_payloads(rest: &[&str]) -> Vec<String> {
    let Some(pos) = rest
        .iter()
        .position(|t| is_short_flag(t) && t.contains('c'))
    else {
        return Vec::new();
    };
    let mut out = payloads_after(rest, pos);
    // Glued short form `-c<payload>` (value in the SAME token, after `c`):
    // `payloads_after` only sees LATER tokens, so `flock /l -c'rm -rf /'` left
    // the payload unanalysed — the twin of the `--command=` attached-form
    // bypass (138607bc). By getopt convention an option that takes an argument
    // is last in a short bundle, so everything after the first `c` is its value.
    if let Some(cidx) = rest[pos].find('c') {
        let inline = &rest[pos][cidx + 1..];
        out.extend(inline_command_payloads(rest, pos, inline));
    }
    out
}

/// The `dash_c_payloads` candidate set for a command-string flag already located
/// at index `pos`. Split out so callers with a different spelling of the flag
/// (e.g. flock's long form `--command`) reuse the identical candidate logic
/// rather than duplicating it.
pub(super) fn payloads_after(rest: &[&str], pos: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut push_first_word = |from: usize| {
        if from >= rest.len() {
            return;
        }
        let joined = rest[from..].join(" ");
        if let Some(word) = first_shell_word(&joined) {
            // The `!word.is_empty()` guard matters: `sh -c ''` yields
            // `Some("")`, which must not be re-analysed as an empty command.
            if !word.is_empty() && !out.contains(&word) {
                out.push(word);
            }
        }
    };
    // Candidate 1: EVERY position after `-c`. This subsumes the historical
    // `pos + 1` candidate (so nothing previously denied can regress to Allow)
    // and any option-skipping heuristic, without needing to know which of the
    // shell's own options take a separate value token.
    for from in pos + 1..rest.len() {
        push_first_word(from);
    }

    // Candidate 2: legacy join-then-peel, unchanged.
    let joined = rest[pos + 1..].join(" ");
    let legacy = strip_wrapping_quotes(&joined);
    if !legacy.is_empty() && !out.iter().any(|p| p == legacy) {
        out.push(legacy.to_string());
    }
    out
}

/// Number of whitespace-split tokens, starting at `rest[from]`, that one shell
/// WORD spans — `"x='a b'` + `c"` is one word over two tokens. `None` when a
/// quote is still open at the end of `rest`: where the word ends is then
/// undetermined.
pub(super) fn shell_word_token_span(rest: &[&str], from: usize) -> Option<usize> {
    // `None` outside quotes, otherwise the open quote char.
    let mut open: Option<char> = None;
    for (n, tok) in rest[from..].iter().enumerate() {
        let mut chars = tok.chars();
        // An escape on the token's LAST char escapes the whitespace that split
        // it, so the word continues into the next token.
        let mut escaped_separator = false;
        while let Some(c) = chars.next() {
            match (open, c) {
                (None, '\\') | (Some('"'), '\\') => {
                    escaped_separator = chars.next().is_none();
                }
                (None, '\'' | '"') => open = Some(c),
                (Some(q), c) if c == q => open = None,
                _ => {}
            }
        }
        if open.is_none() && !escaped_separator {
            return Some(n + 1);
        }
    }
    None
}

/// The [`payloads_after`] candidate set for an INTERPRETER's inline-eval flag
/// (`python3 -c`, `perl -e`, `node -e`, …), minus the candidates that start
/// INSIDE an already-delimited shell word.
///
/// Backlog 1943843c: `payloads_after` re-reads EVERY whitespace position after
/// the flag as a fresh command string. Inside a quoted program that means a
/// candidate starting mid-string: `python3 -c "if 'recursive rm' in t:
/// print(t[:5])"` yielded the candidate `rm' in t: print(t[:5])"`, whose open
/// `'` swallowed the rest, and `rm in t: print(t[:5])"` was denied as "rm with
/// a wildcard" — a command line that exists nowhere: no process ever receives
/// that text as an argument.
///
/// Not handled HERE: a program whose assignment value holds a space and is
/// followed by a NEWLINE, `python3 -c "x='recursive rm'
/// y=a[:1]"`. `rest` was split on all whitespace, so the newline is gone and
/// the shell pass over the whole program reads one statement; its command
/// word was `rm'` until `command_candidates` learned to skip a quoted
/// assignment's whole word (`literal_assignment_span`, backlog ce04adeb).
/// Positions strictly INSIDE a word whose quotes all close are
/// therefore not candidates; the word's whole text still is (and is read by the
/// full program analysis: shell pass, literal descent, destructive calls).
///
/// Every word BOUNDARY after the flag stays a candidate — a later
/// `-e 'system "rm -rf /"'` (perl/ruby/node accept several) is still judged.
/// Undetermined stays on the widening side: from the first word whose quote
/// never closes, every remaining position is a candidate exactly as before.
pub(super) fn interpreter_payloads_after(rest: &[&str], pos: usize) -> Vec<String> {
    let mut keep: Vec<String> = Vec::new();
    let mut boundary = pos + 1;
    while boundary < rest.len() {
        let Some(span) = shell_word_token_span(rest, boundary) else {
            // Undetermined word end: every position, as before.
            return payloads_after(rest, pos);
        };
        if let Some(word) = first_shell_word(&rest[boundary..].join(" ")) {
            if !word.is_empty() && !keep.contains(&word) {
                keep.push(word);
            }
        }
        boundary += span;
    }
    // The legacy join-then-peel candidate is kept unchanged: it is not a
    // mid-word fragment.
    let joined = rest[pos + 1..].join(" ");
    let legacy = strip_wrapping_quotes(&joined);
    if !legacy.is_empty() && !keep.iter().any(|p| p == legacy) {
        keep.push(legacy.to_string());
    }
    keep
}

/// Payloads for a command-string flag whose value is GLUED INSIDE the flag
/// token — the long attached form `--command=<payload>` (value after `=`) and
/// the short glued form `-c<payload>` (value after the `c`). `inline` is the
/// in-token remainder already peeled off by the caller; the payload may also
/// spill into following tokens once the line is split on whitespace
/// (`--command='rm -rf /'` → `--command='rm`, `-rf`, `/'`), so we splice the
/// inline remainder back in front of `rest[pos + 1..]` and reuse the identical
/// candidate logic. `payloads_after` alone only inspects tokens strictly AFTER
/// the flag, so without this the inline value was never analysed — the
/// depth-0 Allow bypass this closes (138607bc).
pub(super) fn inline_command_payloads(rest: &[&str], pos: usize, inline: &str) -> Vec<String> {
    // An empty inline means the value is NOT glued (`--command`, `-c`, or a
    // bundle like `-nc` with the value in the next token) — `payloads_after`
    // already covers that, so there is nothing extra to reconstruct here.
    if inline.is_empty() {
        return Vec::new();
    }
    // A dummy flag at index 0 lets us reuse `payloads_after`'s "everything after
    // the flag" candidate logic with the inline value spliced in as the first
    // operand token.
    let mut synthetic = vec!["--x", inline];
    synthetic.extend_from_slice(&rest[pos + 1..]);
    payloads_after(&synthetic, 0)
}

/// Split a leading assignment whose value OPENS a command substitution that
/// the token itself does not close — `B=$(ls`, `B="$(ls`, `` B=`ls `` — into
/// the assignment-with-opener (`B=$(`) and the substitution's first word
/// (`ls`).
///
/// Backlog a03fe3cd (#1471): the tokeniser splits on whitespace, so
/// `B=$(ls -d GLOB | sort -V | tail -1)` reached [`analyze_segment`] (cut at
/// the `|` by [`split_segments`]) as `B=$(ls`, `-d`, `GLOB`.
/// [`command_candidates`] skips `B=$(ls` as an assignment and took `-d` as the
/// command word, so the line asked "`-d` is a command blastguard has no rule
/// for". The word that actually runs there is the substitution's first word,
/// `ls`; `B=$( ls …` (a space after the opener) already parsed that way.
///
/// Only LEADING assignment tokens are rewritten (command position), and only
/// when the opener is unclosed within the token, so `B=$(pwd)` stays a pure
/// assignment. Exposing the body's first word as a command candidate only
/// ever adds a word for the rule engine to judge; the body is still also
/// re-analysed through [`executed_substitution_payloads`] when it is closed.
pub(super) fn split_substitution_opening_assignments(tokens: Vec<&str>) -> Vec<&str> {
    let mut out = Vec::with_capacity(tokens.len() + 1);
    let mut leading = true;
    for tok in tokens {
        if leading && is_assignment(tok) {
            if let Some(cut) = unclosed_substitution_opener_end(tok) {
                out.push(&tok[..cut]);
                out.push(&tok[cut..]);
                leading = false;
                continue;
            }
            out.push(tok);
            continue;
        }
        leading = false;
        out.push(tok);
    }
    out
}

/// Byte index just past the `$(` / backtick that opens an assignment token's
/// value (optionally inside a leading `"`), when the rest of the token is a
/// non-empty word that does not close that substitution. `None` otherwise.
fn unclosed_substitution_opener_end(tok: &str) -> Option<usize> {
    let eq = tok.find('=')?;
    let value = &tok[eq + 1..];
    let quote = usize::from(value.starts_with('"'));
    let after_quote = &value[quote..];
    let (opener_len, closer) = if after_quote.starts_with("$(") {
        (2, ')')
    } else if after_quote.starts_with('`') {
        (1, '`')
    } else {
        return None;
    };
    let cut = eq + 1 + quote + opener_len;
    let body = &tok[cut..];
    if body.is_empty() || body.contains(closer) {
        return None;
    }
    Some(cut)
}
