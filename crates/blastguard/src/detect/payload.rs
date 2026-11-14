use super::printf_eval::{evaluate_printf_stage, PrintfStage};
use super::{
    backtick_body_as_run, command_candidates, first_line_quoted_here_document_body,
    first_shell_word, here_document_delimiter_is_quoted, here_string_spans,
    interpreter_code_verdict, is_assignment, is_bare_redirect_op, is_code_interpreter,
    is_exec_wrapper, is_foreign_shell, is_redirect_token, is_shell, is_short_flag,
    normalized_command, opens_here_document, quoted_string_literals, recommend_against,
    scan_balanced, split_statements_into_pipe_stages, wrapped_command_heads, xargs_command_start,
    xargs_reads_arg_file, xargs_shell, Ctx, Decision, PythonReader, Stop, VerdictAcc,
};

mod compound_readers;
mod exec_fds;
mod node_source;
mod program_source;

use exec_fds::{exec_opened_fds, ExecFd};
use node_source::{is_node_head, node_program_source};
use program_source::{
    interpreter_program_source, python_program_source, python_reads_stdin_program,
    shell_program_source, shell_startup_file_values, source_program_source, ProgramSource,
};

/// True if this stage names a program FILE for the interpreter to run, rather
/// than taking its program from stdin. Flags, redirections and a bare `-` are
/// not program files. It does not know which options take a value, so a
/// shell's argv is read with [`shell_program_source`] instead (backlog
/// de914866); this reading remains for code interpreters and a shell's `-c`.
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
    let mut acc = VerdictAcc::default();
    if let Some(deny) = acc.record(stdin_exec_verdict(cmd, depth, ctx)) {
        return deny;
    }
    // A reader inside a compound command (`(bash <(…))`, `{ bash <(…); }`,
    // `if …; then bash <(…); fi`, `while …; do … | bash; done`, backlog
    // 72872200): the stage's first word is `(`, `{`, `then` or `do`, not the
    // reader, so the readings below never found it. The same line is judged
    // once more with that syntax removed (see `decompounded`), IN ADDITION to
    // the text as written — this can only add verdicts. Not recursive: the
    // decompounded text is judged by `stdin_exec_verdict` directly.
    let flat = match decompounded(cmd) {
        Ok(flat) => flat,
        Err(why) => {
            if let Some(deny) = acc.record(Decision::ask(why)) {
                return deny;
            }
            None
        }
    };
    if let Some(flat) = flat {
        if let Some(deny) = acc.record(stdin_exec_verdict(&flat, depth, ctx)) {
            return deny;
        }
    }
    acc.finish()
}

/// `cmd` with its compound-command syntax removed — subshell parentheses,
/// brace groups, `if` / `then` / `do` / `done` … and `for` / `case` headers —
/// as [`super::compound_views`] reads it: each segment's simple command, joined
/// by the separator that followed it (`|` stays a pipe, `&&` / `||` become
/// `&&`, `;` and newlines stay). A separator that follows a segment with no
/// command (a closing `}` / `fi` / `done`) carries over to the next command,
/// and a `|` there wins (`{ echo a; echo x; } | bash` reads
/// `echo a ; echo x | bash`). That pipes only the group's LAST member, not the
/// whole stream the reader gets; the text as written keeps its own reading of
/// the group, so this pass only adds a judgement and never replaces one.
///
/// `Ok(None)` when no segment has any such syntax (the text is already flat);
/// `Err` (an Ask reason) when the segments and their views do not line up.
fn decompounded(cmd: &str) -> Result<Option<String>, String> {
    use super::SegmentSep;
    let segs = super::split_segments_with_separators(cmd);
    let views = super::compound_views(cmd);
    if segs.len() != views.len() {
        // `compound_views` is index-aligned with this split by construction;
        // if that ever breaks, the compound reading cannot be built, and
        // silently skipping it would let a reader inside a group through.
        return Err(format!(
            "blastguard could not line up the compound-command reading of `{}` with its \
segments, so it cannot tell which command inside a group or loop reads a program",
            cmd.trim()
        ));
    }
    let changed = segs
        .iter()
        .zip(&views)
        .any(|(s, v)| v.command.trim() != s.text.trim() || !v.data.trim().is_empty());
    if !changed {
        return Ok(None);
    }
    let mut out = String::new();
    let mut pending: Option<&str> = None;
    for (seg, view) in segs.iter().zip(&views) {
        let command = view.command.trim();
        if !command.is_empty() {
            if let Some(sep) = pending.take() {
                out.push_str(sep);
            }
            out.push_str(command);
        }
        let sep = match seg.sep_after {
            SegmentSep::Pipe => " | ",
            SegmentSep::Conditional => " && ",
            SegmentSep::Background => " & ",
            SegmentSep::Sequential if seg.ends_at_newline => "\n",
            SegmentSep::Sequential => " ; ",
            SegmentSep::EndOfLine => "",
        };
        if !out.is_empty() && !sep.is_empty() {
            pending = Some(match pending {
                Some(" | ") => " | ",
                _ => sep,
            });
        }
    }
    let flat = out.trim().to_string();
    Ok((!flat.is_empty() && flat != cmd.trim()).then_some(flat))
}

/// The readings of [`analyze_interpreter_stdin_exec`] over `cmd` as written.
fn stdin_exec_verdict(cmd: &str, depth: usize, ctx: &Ctx<'_>) -> Decision {
    let heredoc_open = opens_here_document(cmd);
    let mut acc = VerdictAcc::default();
    // What `echo`/`printf` write to a file another stage runs
    // (`echo 'rm -rf ~' > x.rs && sh x.rs`); see `echo_script_file`.
    let echoed = super::echo_script_file::echoed_script_verdict(cmd, depth, ctx);
    if let Some(deny) = acc.record(echoed) {
        return deny;
    }
    // A process substitution a shell/interpreter reads as its PROGRAM
    // (`bash < <(echo 'rm -rf ~')`, `sh <(printf …) arg`, `source <(…)`,
    // `bash <&3 3< <(…)`): the inner command's output is the program, exactly
    // as when it is piped in. Judged as that pipeline; see `procsub_programs`.
    // Read on `procsub_stages`, which keeps `<&3` / `2>&1` inside the stage.
    // An fd `exec` opens for the rest of the line (`exec 3< <(…); bash <&3`,
    // backlog 72872200) is read across the statement boundary; see
    // `exec_opened_fds`.
    let stages = procsub_stages(cmd);
    let exec_fds = exec_opened_fds(&stages);
    for stage in &stages {
        let procsubs = procsub_programs(stage, &exec_fds);
        for synthetic in &procsubs.pipelines {
            if let Some(deny) = acc.record(analyze_interpreter_stdin_exec(synthetic, depth, ctx)) {
                return deny;
            }
        }
        for why in procsubs.undetermined {
            if let Some(deny) = acc.record(Decision::ask(why)) {
                return deny;
            }
        }
    }
    let statements = split_statements_into_pipe_stages(cmd);
    // A reader hidden behind a subshell or a brace group (`… | (bash)`,
    // `… | { bash; }`, backlog 036067bf): every command of the group inherits
    // the pipe as its stdin, so each is judged as piped from the upstream
    // stage. See `group_reader_pipelines`. The descent terminates: each
    // synthetic pipeline holds one grouping layer fewer past its first stage
    // than the stage it came from.
    let (mut group_pipelines, mut group_undetermined) = group_reader_pipelines(&statements);
    // The same for `if` / `while` / `until` bodies and `eval` / `sh -c`
    // forwarders (backlog b3569da5); see `compound_readers`.
    let (more, why) = compound_readers::compound_and_forwarder_pipelines(&statements);
    group_pipelines.extend(more);
    group_undetermined.extend(why);
    for synthetic in &group_pipelines {
        if let Some(deny) = acc.record(analyze_interpreter_stdin_exec(synthetic, depth, ctx)) {
            return deny;
        }
    }
    for why in group_undetermined {
        if let Some(deny) = acc.record(Decision::ask(why)) {
            return deny;
        }
    }
    for statement in statements {
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
                is_shell(&head) || is_code_interpreter(&head) || is_source_head(&head)
            }) else {
                continue;
            };
            let rest = &tokens[idx + 1..];
            // `. /dev/stdin` / `source /dev/stdin` (backlog 036067bf): the
            // shell reading the line sources its own stdin, i.e. runs the
            // piped program exactly as `… | bash` does. Only a piped or
            // here-document stage reads that stream; a sourced FILE is not
            // this rule's, and an option `source` takes that is not modelled
            // leaves the program undetermined.
            let source_reader = is_source_head(&normalized_command(tokens[idx]));
            if source_reader {
                if !(i > 0 || heredoc_open) {
                    continue;
                }
                let argv = argv_words(rest);
                match source_program_source(&argv) {
                    ProgramSource::File(k) if is_stdin_path(argv[k]) => {}
                    ProgramSource::Undetermined(why) => {
                        if let Some(deny) = acc.record(Decision::ask(format!(
                            "`{}` is fed a program on stdin, and {why} — blastguard cannot tell \
whether the sourced file is that stdin",
                            stage.trim()
                        ))) {
                            return deny;
                        }
                        continue;
                    }
                    _ => continue,
                }
            }
            // `osascript` removing files through Finder / System Events, however
            // the program arrives — judged before the program-file test, since
            // `osascript -l JavaScript <<'E'` reads `JavaScript` as one.
            if normalized_command(tokens[idx]) == "osascript" && (i > 0 || heredoc_open) {
                let texts = stdin_program_texts(cmd, stage, &statement, i, heredoc_open);
                if let Some(deny) = super::interpreter::osascript_program_delete(&texts) {
                    return deny;
                }
            }
            // A SHELL's argv is read with its option grammar (backlog
            // de914866): the value of `-o` / `-O` / `--rcfile` and the
            // positionals after `-s` (`| bash -o pipefail`, `| sh -s a b`) are
            // not a script file — the shell still runs its stdin. Taking the
            // first non-dash word for the script let those pipes through
            // unjudged. An option shape the grammar does not model leaves the
            // program undetermined: the stdin program is judged AND the stage
            // is an Ask, never an Allow. `-c` keeps the generic reading.
            let has_program_file = if is_shell(&normalized_command(tokens[idx])) {
                match shell_program_source(&argv_words(rest)) {
                    ProgramSource::File(_) => true,
                    ProgramSource::Stdin => false,
                    ProgramSource::Inline => stage_has_program_operand(rest),
                    ProgramSource::Undetermined(why) => {
                        if i > 0 || heredoc_open {
                            if let Some(deny) = acc.record(Decision::ask(format!(
                                "`{}` is fed a program on stdin, and {why} — blastguard cannot \
tell whether the shell runs that stdin or a script file",
                                stage.trim()
                            ))) {
                                return deny;
                            }
                        }
                        false
                    }
                }
            } else if is_python_head(tokens[idx]) || is_node_head(tokens[idx]) {
                // `python3 -X dev` / `-W ignore` / `node -r m`: the option's
                // VALUE is not a program file (backlog f9861c4e), so read the
                // argv with the interpreter's option grammar. An option the
                // grammar does not model is Undetermined: the stdin program is
                // judged AND the stage is an Ask, never an Allow.
                let words = argv_words(rest);
                let source = if is_python_head(tokens[idx]) {
                    python_program_source(&words)
                } else {
                    node_program_source(&words)
                };
                match source {
                    // `-i` / `-m code`: stdin is still run as a program.
                    ProgramSource::File(_) | ProgramSource::Inline
                        if is_python_head(tokens[idx]) && python_reads_stdin_program(&words) =>
                    {
                        false
                    }
                    ProgramSource::File(_) => true,
                    ProgramSource::Stdin => false,
                    ProgramSource::Inline => stage_has_program_operand(rest),
                    ProgramSource::Undetermined(why) => {
                        if i > 0 || heredoc_open {
                            if let Some(deny) = acc.record(Decision::ask(format!(
                                "`{}` is fed a program on stdin, and {why} — blastguard cannot \
tell whether the interpreter runs that stdin or a script file",
                                stage.trim()
                            ))) {
                                return deny;
                            }
                        }
                        false
                    }
                }
            } else {
                stage_has_program_operand(rest)
            };
            let takes_stdin_program = source_reader
                || interpreter_stdin_operand(rest).is_some()
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
            let mut program_texts = stdin_program_texts(cmd, stage, &statement, i, heredoc_open);
            let shape = format!("`{}` takes its program from stdin", stage.trim());
            // A shell removes its program's line continuations before it
            // splits words: judge that reading too (backlog 040b959b).
            if source_reader || is_shell(&normalized_command(tokens[idx])) {
                let (readings, undetermined) =
                    super::readings::shell_program_readings(&program_texts);
                program_texts = readings;
                if let Some(deny) = undetermined.and_then(|ask| acc.record(ask)) {
                    return deny;
                }
            }
            let python = python_stdin_reading(cmd, tokens[idx], &program_texts, depth, ctx);
            // A Python reader decodes its literals (backlog 47b20647), and an
            // UNQUOTED here-document reaches it with the shell's backslash
            // processing applied: judge that reading of the body too.
            let python_reader = is_python_head(tokens[idx]);
            if python_reader && heredoc_open && !here_document_delimiter_is_quoted(cmd) {
                if let Some(read) = program_texts
                    .first()
                    .and_then(|b| super::readings::unquoted_here_document_reading(b))
                {
                    program_texts.push(read);
                }
            }
            if let Some(deny) = acc.record(interpreter_code_verdict(
                &shape,
                &program_texts,
                depth,
                ctx,
                !here_document_delimiter_is_quoted(cmd),
                if python_reader {
                    PythonReader::Stdin
                } else {
                    PythonReader::NotPython
                },
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
                if let PrintfStage::Undetermined(why) = evaluate_printf_stage(&statement[i - 1]) {
                    if let Some(deny) = acc.record(Decision::ask(format!(
                        "{shape}, and the printf stage piping into it (`{}`) cannot be \
evaluated statically ({why}) — its output is the program, so blastguard cannot read it",
                        statement[i - 1].trim()
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

/// `source` and its `.` spelling: the shell builtin that runs a file's text
/// as a program in the current shell.
fn is_source_head(head: &str) -> bool {
    matches!(head, "source" | ".")
}

/// A path that names the reading process's own stdin.
fn is_stdin_path(word: &str) -> bool {
    matches!(
        strip_wrapping_quotes(word),
        "/dev/stdin" | "/dev/fd/0" | "/proc/self/fd/0"
    )
}

/// The argv words among a command's tokens after its head: every token that
/// is not a redirection, nor the target word of a bare redirection operator
/// (`2> FILE`).
fn argv_words<'a>(rest: &[&'a str]) -> Vec<&'a str> {
    let mut out = Vec::new();
    let mut j = 0;
    while j < rest.len() {
        match parse_redirect(rest[j]) {
            Some((_, _, glued)) => j += if glued.is_empty() { 2 } else { 1 },
            None => {
                out.push(rest[j]);
                j += 1;
            }
        }
    }
    out
}

/// The `UPSTREAM | MEMBER` pipelines for every stage that is a subshell
/// `( … )` or a brace group `{ …; }` fed by a pipe (backlog 036067bf).
///
/// Every command inside the group inherits the pipe as its stdin, so a shell
/// there (`printf 'chmod 777 /etc/hosts' | (bash)`, `… | { bash; }`,
/// `… | { true; bash; }`) runs the piped text exactly as `… | bash` does —
/// but the stage's head word was `(bash)` or `{`, no reader, and the program
/// was never judged. Each member (an inner statement, its own pipe stages kept)
/// is returned piped from the group's upstream stage, which over-approximates
/// (a member after `head -1` sees only what is left of the stream) on the
/// restrictive side.
///
/// A subshell's text stays inside one stage (the split does not cut inside
/// parentheses); its inner list is split again. A brace group is NOT kept
/// together by the split: its first member is the rest of the `{` stage (with
/// the stages after it), and the following statements are members until the
/// statement whose first word is the matching `}`.
///
/// Returns `(pipelines, undetermined)`: a subshell whose `)` is not found, or a
/// brace group whose `}` is not found, is an Ask reason — what reads the pipe
/// cannot be determined.
fn group_reader_pipelines(statements: &[Vec<String>]) -> (Vec<String>, Vec<String>) {
    fn first_word(stage: &str) -> Option<&str> {
        stage.split_whitespace().next()
    }
    /// The stage text with every leading `{` word removed (a nested group's
    /// opener: its members are fed the same stream).
    fn strip_brace_openers(stage: &str) -> &str {
        let mut s = stage.trim_start();
        while let Some(rest) = s.strip_prefix('{') {
            if !rest.is_empty() && !rest.starts_with(char::is_whitespace) {
                break;
            }
            s = rest.trim_start();
        }
        s
    }
    fn joined(stages: &[String]) -> String {
        stages
            .iter()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" | ")
    }
    let mut pipelines: Vec<String> = Vec::new();
    let mut undetermined: Vec<String> = Vec::new();
    fn push(p: String, pipelines: &mut Vec<String>) {
        if !pipelines.contains(&p) {
            pipelines.push(p);
        }
    }
    for (s, statement) in statements.iter().enumerate() {
        for i in 1..statement.len() {
            let upstream = statement[i - 1].trim();
            let stage = statement[i].trim();
            if stage.starts_with('(') {
                let chars: Vec<char> = stage.chars().collect();
                let Some(end) = scan_balanced(&chars, 1, Stop::Paren, 0) else {
                    undetermined.push(format!(
                        "`{upstream}` is piped into the subshell `{stage}`, whose closing `)` \
blastguard cannot find — it cannot tell which command inside reads the piped program"
                    ));
                    continue;
                };
                let inner: String = chars[1..end].iter().collect();
                for member in split_statements_into_pipe_stages(&inner) {
                    let member = joined(&member);
                    if !member.is_empty() {
                        push(format!("{upstream} | {member}"), &mut pipelines);
                    }
                }
            } else if first_word(stage) == Some("{") {
                let mut members: Vec<String> = Vec::new();
                let mut first: Vec<String> = vec![strip_brace_openers(stage).to_string()];
                first.extend(statement[i + 1..].iter().cloned());
                members.push(joined(&first));
                // Nesting: every `{` word opening a statement's first stage
                // (or the `{` stage itself) is one level; a statement whose
                // first word is `}` closes one.
                let opened = stage.split_whitespace().take_while(|w| *w == "{").count();
                let mut level = opened;
                let mut closed = false;
                for later in &statements[s + 1..] {
                    let head = later.first().map(|st| st.trim()).unwrap_or("");
                    if first_word(head) == Some("}") {
                        level -= 1;
                        if level == 0 {
                            closed = true;
                            break;
                        }
                        continue;
                    }
                    level += head.split_whitespace().take_while(|w| *w == "{").count();
                    let mut stages = later.clone();
                    if let Some(st) = stages.first_mut() {
                        *st = strip_brace_openers(st).to_string();
                    }
                    members.push(joined(&stages));
                }
                if !closed {
                    undetermined.push(format!(
                        "`{upstream}` is piped into the brace group `{stage} …`, whose closing `}}` \
blastguard cannot find — it cannot tell which command inside reads the piped program"
                    ));
                }
                for member in members.into_iter().filter(|m| !m.is_empty()) {
                    push(format!("{upstream} | {member}"), &mut pipelines);
                }
            }
        }
    }
    (pipelines, undetermined)
}

/// `cmd` split into simple command stages at `;`, newlines, `|`, `|&`, `||`,
/// `&&` and a background `&` outside quotes and parentheses — but NOT at the
/// `&` of a redirection (`<&3`, `2>&1`, `&>f`), which belongs to its stage
/// (`split_statements_into_pipe_stages` splits there, which cut
/// `bash <&3 3< <(…)` into `bash <` and `3 3< <(…)`).
fn procsub_stages(cmd: &str) -> Vec<String> {
    let chars: Vec<char> = cmd.chars().collect();
    let mut stages = Vec::new();
    let mut cur = String::new();
    let (mut in_s, mut in_d) = (false, false);
    let mut depth: u32 = 0;
    let mut k = 0;
    while k < chars.len() {
        let c = chars[k];
        if c == '\\' && !in_s {
            cur.push(c);
            if let Some(&n) = chars.get(k + 1) {
                cur.push(n);
            }
            k += 2;
            continue;
        }
        if c == '\'' && !in_d {
            in_s = !in_s;
        } else if c == '"' && !in_s {
            in_d = !in_d;
        } else if !in_s && !in_d {
            match c {
                '(' => depth += 1,
                ')' => depth = depth.saturating_sub(1),
                _ => {}
            }
            let redirect_amp = c == '&'
                && (matches!(k.checked_sub(1).map(|p| chars[p]), Some('<' | '>'))
                    || chars.get(k + 1) == Some(&'>'));
            if depth == 0 && matches!(c, ';' | '\n' | '|' | '&') && !redirect_amp {
                stages.push(std::mem::take(&mut cur));
                k += 1;
                continue;
            }
        }
        cur.push(c);
        k += 1;
    }
    stages.push(cur);
    stages.retain(|s| !s.trim().is_empty());
    stages
}

/// What the process substitutions of one stage hand its reader as a PROGRAM.
#[derive(Debug, Default, PartialEq, Eq)]
struct ProcsubPrograms {
    /// `UPSTREAM | READER` pipelines, one per reading of a substitution that is
    /// (or may be) the program, each judged by [`analyze_interpreter_stdin_exec`].
    pipelines: Vec<String>,
    /// Why a substitution may be the program and blastguard cannot tell which
    /// one or cannot read it: each is an Ask.
    undetermined: Vec<String>,
}

/// The process substitutions of `stage` that its reader — a shell, a code
/// interpreter, or `source` / `.` (which run the file's text in the current
/// shell; judged as `sh`) — runs as its PROGRAM, as `UPSTREAM | READER`
/// pipelines the pipe rule judges (see [`procsub_inner_pipelines`]):
///
/// * the SCRIPT operand (`bash <(…) a b`, `bash -o pipefail -x <(…)`,
///   `source <(…) arg`): what follows it are `$1`…, which do not change which
///   program runs, so the pipeline is `UPSTREAM | READER` with no arguments;
/// * STDIN when the reader has no script operand (`bash < <(…)`,
///   `bash 0< <(…)`, `bash -s a b < <(…)`), including an fd the substitution is
///   opened on and stdin is duplicated from (`bash <&3 3< <(…)`, in either
///   order — the order-insensitive reading is the restrictive one);
/// * a shell's `--rcfile` / `--init-file` value (`bash --rcfile <(…) -i`,
///   `bash --init-file=<(…)`), the startup file an interactive bash sources,
///   judged whether or not `-i` is given ([`shell_startup_file_values`]),
///   in addition to the main program.
///
/// Apart from that startup file, nothing when the reader runs an inline program (`bash -c 'x' <(…)`,
/// `python3 -c …`, `perl -ne …`) or a script file that is not a substitution
/// (`bash script.sh <(…)`, `bash script.sh < <(…)`): those keep their
/// verdicts, as does every stage whose command is not such a reader
/// (`diff <(ls a) <(ls b)`, `cat <(echo hi)`). An option shape that is not
/// modelled before the operands (`bash --frobnicate <(…)`, `node -r m <(…)`)
/// means the script operand cannot be proved: every substitution that may be
/// the program is judged as one AND the stage is an Ask (never an Allow).
///
/// Stdin may also come from an fd an earlier `exec` opened on the line
/// (`exec 3< <(…); bash <&3`, `exec < <(…); bash`, backlog 72872200), given as
/// `exec_fds` ([`exec_opened_fds`]): a substitution there is judged as the
/// program exactly as an in-stage one is. An fd above 2 that stdin is
/// duplicated from and that nothing on the line opens (`bash <&3`), or that is
/// opened on a here-document / here-string, cannot be read: when the reader
/// takes its program from stdin that is an Ask, never an Allow. An fd opened on
/// a file (`exec 3< f; bash <&3`) is that file, read as `bash < f` is.
///
/// zsh's `=(…)` (a substitution handed over as a temporary FILE) is read as
/// `<(…)`: `zsh =(…)` runs the inner command's output just as `zsh <(…)` does.
/// It is read so for every reader, not only zsh (bash does not parse a word
/// starting `=(`; reading it as a substitution there only adds a judgement).
///
/// Every pipeline holds only the TEXT of a substitution (one nesting level
/// down), so the recursion through [`analyze_interpreter_stdin_exec`] ends.
fn procsub_programs(stage: &str, exec_fds: &[(u32, ExecFd)]) -> ProcsubPrograms {
    let mut out = ProcsubPrograms::default();
    let words = super::quote_aware_words(stage);
    let exec_stdin = exec_fds.iter().any(|(fd, _)| *fd == 0);
    if !exec_stdin
        && !words
            .iter()
            .any(|w| w.contains("<(") || w.contains("=(") || w.contains('&'))
    {
        return out;
    }
    let refs: Vec<&str> = words.iter().map(String::as_str).collect();
    let Some(r) = wrapped_command_heads(&refs).into_iter().find(|&idx| {
        let head = normalized_command(refs[idx]);
        is_shell(&head) || is_code_interpreter(&head) || head == "source" || head == "."
    }) else {
        return out;
    };
    let head = normalized_command(refs[r]);
    let reader = if head == "source" || head == "." {
        "sh"
    } else {
        refs[r]
    };
    // Split the arguments into argv words and input redirections.
    let mut argv: Vec<usize> = Vec::new();
    let mut fd_procsub: Vec<(u32, usize)> = Vec::new();
    let mut dup_into_stdin: Vec<u32> = Vec::new();
    // Every other fd this stage opens itself, and whether it is a readable
    // FILE (false: a here-document / here-string, whose text is not read here).
    let mut opened_in_stage: Vec<(u32, bool)> = Vec::new();
    let mut stdin_redirected = false;
    let mut j = r + 1;
    while j < refs.len() {
        let w = refs[j];
        let Some((fd, op, glued)) = parse_redirect(w) else {
            argv.push(j);
            j += 1;
            continue;
        };
        let target_idx = if glued.is_empty() {
            j += 1;
            Some(j).filter(|&t| t < refs.len())
        } else {
            None
        };
        let target = target_idx.map_or(glued, |t| refs[t]);
        let effective_fd = fd.unwrap_or(if op.starts_with('<') { 0 } else { 1 });
        if effective_fd == 0 && (op.starts_with('<') || op == ">&") {
            stdin_redirected = true;
        }
        match op {
            "<" | "<>" => {
                if let Some(t) = target_idx.filter(|&t| procsub_body(refs[t]).is_some()) {
                    fd_procsub.push((effective_fd, t));
                } else {
                    opened_in_stage.push((effective_fd, true));
                }
            }
            "<&" | ">&" if effective_fd == 0 => {
                if let Ok(n) = target.parse::<u32>() {
                    dup_into_stdin.push(n);
                }
            }
            "<<" | "<<-" | "<<<" => opened_in_stage.push((effective_fd, false)),
            _ => opened_in_stage.push((effective_fd, true)),
        }
        j += 1;
    }
    let stdin_procsubs: Vec<usize> = fd_procsub
        .iter()
        .filter(|(fd, _)| *fd == 0 || dup_into_stdin.contains(fd))
        .map(|&(_, idx)| idx)
        .collect();
    // Stdin taken from an fd the stage does not open itself: an `exec` earlier
    // on the line, or nothing at all (an inherited fd blastguard cannot see).
    let mut exec_stdin_bodies: Vec<String> = Vec::new();
    let mut unreadable_stdin: Vec<String> = Vec::new();
    let from_exec = |n: u32, bodies: &mut Vec<String>, unreadable: &mut Vec<String>| {
        let opened: Vec<&ExecFd> = exec_fds
            .iter()
            .filter(|(f, _)| *f == n)
            .map(|(_, e)| e)
            .collect();
        if opened.is_empty() && n > 2 {
            unreadable.push(format!(
                "its stdin is duplicated from fd {n}, which nothing on this line opens"
            ));
        }
        for e in opened {
            match e {
                ExecFd::Procsub(body) => bodies.push(body.clone()),
                ExecFd::File => {}
                ExecFd::Unreadable => unreadable.push(format!(
                    "its stdin is fd {n}, which `exec` opens on text blastguard does not read \
(a here-document, a here-string or another unresolved fd)"
                )),
            }
        }
    };
    for &n in &dup_into_stdin {
        if n == 0 || fd_procsub.iter().any(|(f, _)| *f == n) {
            continue;
        }
        if let Some(&(_, readable_file)) = opened_in_stage.iter().find(|(f, _)| *f == n) {
            if !readable_file {
                unreadable_stdin.push(format!(
                    "its stdin is duplicated from fd {n}, opened on a here-document or \
here-string blastguard does not read as this reader's program"
                ));
            }
            continue;
        }
        from_exec(n, &mut exec_stdin_bodies, &mut unreadable_stdin);
    }
    if !stdin_redirected {
        from_exec(0, &mut exec_stdin_bodies, &mut unreadable_stdin);
    }
    let operand_procsubs: Vec<usize> = argv
        .iter()
        .copied()
        .filter(|&idx| procsub_body(refs[idx]).is_some())
        .collect();
    let operands: Vec<&str> = argv.iter().map(|&idx| refs[idx]).collect();
    let source = if head == "source" || head == "." {
        source_program_source(&operands)
    } else if is_shell(&head) {
        shell_program_source(&operands)
    } else if is_python_head(&head) {
        python_program_source(&operands)
    } else {
        interpreter_program_source(&operands)
    };
    let programs: Vec<usize> = match &source {
        ProgramSource::File(k) => {
            let k = *k;
            let idx = argv[k];
            if procsub_body(refs[idx]).is_some() {
                vec![idx]
            } else {
                Vec::new()
            }
        }
        ProgramSource::Stdin => stdin_procsubs,
        ProgramSource::Inline => Vec::new(),
        ProgramSource::Undetermined(why) => {
            let mut all = operand_procsubs;
            all.extend(stdin_procsubs);
            if !all.is_empty() || !exec_stdin_bodies.is_empty() {
                out.undetermined.push(format!(
                    "`{}` reads a process substitution, and {why} — blastguard cannot prove which \
operand or input is the program `{reader}` runs, so it cannot tell whether the substitution's \
output is that program",
                    stage.trim()
                ));
            }
            all
        }
    };
    let mut inners: Vec<&str> = programs
        .iter()
        .filter_map(|&idx| procsub_body(refs[idx]))
        .collect();
    // Stdin from an `exec`-opened fd, or from an fd nothing opens: only when
    // the reader may take its program from stdin.
    if matches!(
        source,
        ProgramSource::Stdin | ProgramSource::Undetermined(_)
    ) {
        inners.extend(exec_stdin_bodies.iter().map(String::as_str));
        for why in unreadable_stdin {
            out.undetermined.push(format!(
                "`{}` may take its program from stdin, and {why} — blastguard cannot read the \
program `{reader}` runs",
                stage.trim()
            ));
        }
    }
    // A shell's `--rcfile` / `--init-file` value is a program too.
    if is_shell(&head) {
        inners.extend(
            shell_startup_file_values(&operands)
                .into_iter()
                .filter_map(procsub_body),
        );
    }
    for inner in inners {
        let (pipelines, undetermined) = procsub_inner_pipelines(inner, reader);
        out.pipelines.extend(pipelines);
        out.undetermined.extend(undetermined);
    }
    out
}

/// The command text of a process substitution word `<(…)` — or zsh's
/// `=(…)`, the same command output handed over as a temporary file — `None`
/// for any other word (a quoted `'<(x)'` is not one; neither is `<(x)y`).
fn procsub_body(word: &str) -> Option<&str> {
    let rest = word
        .strip_prefix("<(")
        .or_else(|| word.strip_prefix("=("))?;
    let chars: Vec<char> = word.chars().collect();
    let end = scan_balanced(&chars, 2, Stop::Paren, 0)?;
    if end + 1 != chars.len() {
        return None;
    }
    Some(rest.strip_suffix(')').unwrap_or(rest).trim())
}

/// A redirection word as `(fd, operator, glued target)`: `3<` is
/// `(Some(3), "<", "")`, `<&3` is `(None, "<&", "3")`, `2>/dev/null` is
/// `(Some(2), ">", "/dev/null")`. `None` for a word that is not a redirection,
/// including a process substitution `<(…)` / `>(…)` (an operand).
fn parse_redirect(word: &str) -> Option<(Option<u32>, &str, &str)> {
    if word.starts_with("<(") || word.starts_with(">(") {
        return None;
    }
    let digits = word.len() - word.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    let fd = word[..digits].parse::<u32>().ok();
    let rest = &word[digits..];
    const OPS: [&str; 12] = [
        "&>>", "<<<", "<<-", "&>", "<<", "<>", "<&", ">>", ">&", ">|", "<", ">",
    ];
    let op = OPS.iter().find(|op| rest.starts_with(**op))?;
    if fd.is_some() && op.starts_with('&') {
        return None;
    }
    Some((fd, op, &rest[op.len()..]))
}

/// The `UPSTREAM | READER` pipelines judging the output of process
/// substitution text `inner` as `reader`'s program, plus the reasons it cannot
/// be read (each an Ask).
///
/// * One simple command: `INNER | READER`, with a bare `echo` respelled by
///   [`quote_bare_echo_words`].
/// * A list of simple commands joined by `;`, `&&` or newlines
///   (`echo hi; echo there`): each member piped on its own (so the pipe rule
///   reads and judges each), AND their concatenated output — what the reader
///   actually receives, where `printf 'chmod 777 '; echo /etc/hosts` is ONE
///   command — as `printf '%s' 'TEXT' | READER`, once per combination of
///   printf dialect readings, with each `'` of TEXT spelled `'"'"'` (close the
///   single quote, a double-quoted `'`, reopen) so the printf operand's value
///   is exactly TEXT and no backslash is needed outside quotes. When that
///   output cannot be computed statically (a member that is not a plain
///   `echo` / evaluable `printf`, too many readings) it is an Ask.
/// * Anything else (a pipe, `||`, `&`, a subshell): `(INNER) | READER`, which
///   is not a literal emitter, so the pipe rule asks (any literal inside is
///   still judged).
fn procsub_inner_pipelines(inner: &str, reader: &str) -> (Vec<String>, Vec<String>) {
    if inner.is_empty() {
        return (Vec::new(), Vec::new());
    }
    let Some(members) = simple_list_members(inner) else {
        return (vec![format!("({inner}) | {reader}")], Vec::new());
    };
    if let [only] = members.as_slice() {
        return (
            vec![format!("{} | {reader}", quote_bare_echo_words(only))],
            Vec::new(),
        );
    }
    let mut pipelines: Vec<String> = members
        .iter()
        .map(|m| format!("{} | {reader}", quote_bare_echo_words(m)))
        .collect();
    let mut undetermined = Vec::new();
    match concatenated_outputs(&members) {
        Ok(texts) => {
            for text in texts {
                let quoted = text.replace('\'', "'\"'\"'");
                let synthetic = format!("printf '%s' '{quoted}' | {reader}");
                if !pipelines.contains(&synthetic) {
                    pipelines.push(synthetic);
                }
            }
        }
        Err(why) => undetermined.push(format!(
            "the process substitution `<({inner})` is the program `{reader}` runs, and the \
combined output of its commands cannot be computed statically ({why}) — that output is the \
program, so blastguard cannot read it"
        )),
    }
    (pipelines, undetermined)
}

/// The members of `inner` when it is a list of commands joined only by `;`,
/// `&&` or newlines outside quotes, each without a pipe, `||`, `&` or a
/// parenthesis (`None` otherwise). Empty members are dropped.
fn simple_list_members(inner: &str) -> Option<Vec<String>> {
    let chars: Vec<char> = inner.chars().collect();
    let mut members = Vec::new();
    let mut cur = String::new();
    let (mut in_s, mut in_d) = (false, false);
    let mut k = 0;
    while k < chars.len() {
        let c = chars[k];
        if c == '\\' && !in_s {
            cur.push(c);
            if let Some(&n) = chars.get(k + 1) {
                cur.push(n);
            }
            k += 2;
            continue;
        }
        if c == '\'' && !in_d {
            in_s = !in_s;
        } else if c == '"' && !in_s {
            in_d = !in_d;
        } else if !in_s && !in_d {
            if c == ';' || c == '\n' || (c == '&' && chars.get(k + 1) == Some(&'&')) {
                members.push(std::mem::take(&mut cur));
                k += if c == '&' { 2 } else { 1 };
                continue;
            }
            if matches!(c, '|' | '&' | '(' | ')') {
                return None;
            }
        }
        cur.push(c);
        k += 1;
    }
    if in_s || in_d {
        return None;
    }
    members.push(cur);
    let members: Vec<String> = members
        .into_iter()
        .map(|m| m.trim().to_string())
        .filter(|m| !m.is_empty())
        .collect();
    if members.is_empty() {
        None
    } else {
        Some(members)
    }
}

/// Every reading of what the list `members` writes to stdout, in order, when
/// each member is a plain `echo` ([`plain_echo_output`]) or a `printf` whose
/// output [`evaluate_printf_stage`] determines (one reading per combination of
/// its dialect outputs, at most 16). Otherwise `Err` naming why the output is
/// undetermined (the caller's Ask).
fn concatenated_outputs(members: &[String]) -> Result<Vec<String>, String> {
    const MAX_READINGS: usize = 16;
    let mut readings = vec![String::new()];
    for member in members {
        let outputs = match plain_echo_output(member) {
            Some(text) => vec![text],
            None => match evaluate_printf_stage(member) {
                PrintfStage::Outputs(texts) => texts,
                PrintfStage::Undetermined(why) => {
                    return Err(format!("the printf `{member}` cannot be evaluated ({why})"))
                }
                PrintfStage::NotPrintf => {
                    return Err(format!(
                        "`{member}` is not a plain `echo` or a `printf` whose output is static"
                    ))
                }
            },
        };
        let mut next = Vec::new();
        for prefix in &readings {
            for text in &outputs {
                let joined = format!("{prefix}{text}");
                if !next.contains(&joined) {
                    next.push(joined);
                }
            }
        }
        if next.len() > MAX_READINGS {
            return Err(format!(
                "it has more than {MAX_READINGS} printf dialect readings"
            ));
        }
        readings = next;
    }
    Ok(readings)
}

/// What `echo [-neE…] WORD…` writes when every WORD is static and holds no
/// backslash (so bash's and zsh's echo agree: no escape is decoded): plain
/// characters, single-quoted text, or double-quoted text without `$`, a
/// backtick, `\` or `!`. `None` for anything else, including a first word `-`
/// or `--` (zsh and bash disagree on it) and any redirection.
fn plain_echo_output(stage: &str) -> Option<String> {
    let words = super::quote_aware_words(stage);
    if words.first().map(String::as_str) != Some("echo") {
        return None;
    }
    let flags = echo_flag_count(&words[1..]);
    let no_newline = words[1..1 + flags].iter().any(|f| f.contains('n'));
    let rest = &words[1 + flags..];
    if rest.first().is_some_and(|w| w == "-" || w == "--") {
        return None;
    }
    let mut out = Vec::new();
    for word in rest {
        out.push(static_word(word)?);
    }
    let mut text = out.join(" ");
    if !no_newline {
        text.push('\n');
    }
    Some(text)
}

/// How many leading `words` are echo option clusters: `-` followed only by
/// `n`, `e`, `E` (`-n`, `-nE`, `-neE` …).
fn echo_flag_count<S: AsRef<str>>(words: &[S]) -> usize {
    words
        .iter()
        .take_while(|w| {
            w.as_ref()
                .strip_prefix('-')
                .is_some_and(|f| !f.is_empty() && f.chars().all(|c| matches!(c, 'n' | 'e' | 'E')))
        })
        .count()
}

/// The text of one shell word built only from plain characters, single-quoted
/// spans and double-quoted spans without `$`, a backtick, `\` or `!`; `None`
/// for anything else (expansion, escape, glob, tilde, operator, unterminated
/// quote, `#` comment).
fn static_word(word: &str) -> Option<String> {
    let mut out = String::new();
    let mut chars = word.chars();
    while let Some(c) = chars.next() {
        match c {
            '\'' | '"' => loop {
                let d = chars.next()?;
                if d == c {
                    break;
                }
                if d == '\\' || (c == '"' && matches!(d, '$' | '`' | '!')) {
                    return None;
                }
                out.push(d);
            },
            c if is_plain_word_char(c) => out.push(c),
            _ => return None,
        }
    }
    Some(out)
}

/// A character that means itself in an unquoted shell word (no quoting,
/// escape, expansion, glob, tilde, comment or operator role).
fn is_plain_word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || "._-/:=@%+,".contains(c)
}

/// `echo [-neE…] WORD…` with every WORD made only of plain characters
/// ([`is_plain_word_char`]) respelled as `echo [-neE…] 'WORD …'`: the same
/// output, in the quoted form the pipe rule reads as a literal program (an
/// unquoted `echo hi | bash` has no quoted text there and asks). The leading
/// option clusters are any `-` followed only by `n` / `e` / `E` (`-nE`, `-En`,
/// `-neE` too); a plain word holds no backslash, so `-e` decodes nothing. A
/// first word `-` / `--` (zsh and bash read it differently) or anything else
/// is returned unchanged, so it keeps whatever verdict the pipe rule gives.
fn quote_bare_echo_words(inner: &str) -> String {
    let tokens: Vec<&str> = inner.split_whitespace().collect();
    if tokens.first() != Some(&"echo") {
        return inner.to_string();
    }
    let flags = echo_flag_count(&tokens[1..]);
    let words = &tokens[1 + flags..];
    if words.is_empty()
        || words[0] == "-"
        || words[0] == "--"
        || !words.iter().all(|w| w.chars().all(is_plain_word_char))
    {
        return inner.to_string();
    }
    format!("{} '{}'", tokens[..1 + flags].join(" "), words.join(" "))
}

/// True when `head` is `python`, `python3` or `python3.N`.
fn is_python_head(head: &str) -> bool {
    normalized_command(head)
        .strip_prefix("python")
        .is_some_and(|v| {
            v.is_empty()
                || v == "3"
                || v.strip_prefix("3.")
                    .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
        })
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
    if !is_python_head(head) {
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
/// of THIS statement emits (a `printf` stage's evaluated outputs too), each
/// with its NUL readings ([`with_nul_readings`]). Shared by the stdin-program audit of
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
        // A `printf` stage emits its EVALUATED output (escapes, `%s` …), not
        // the quoted text: judge that too (backlog 00569f7d), once per
        // printf dialect whose output differs (zsh, bash, BSD and GNU
        // disagree on `\x`, `\c`, unknown escapes …). The verbatim literals
        // above stay judged; an unevaluable printf is the Ask that
        // `analyze_interpreter_stdin_exec` records from the same
        // `evaluate_printf_stage` call's `PrintfStage::Undetermined`.
        if let PrintfStage::Outputs(texts) = evaluate_printf_stage(&statement[i - 1]) {
            for text in texts {
                if !text.trim().is_empty() && !program_texts.contains(&text) {
                    program_texts.push(text);
                }
            }
        }
    }
    with_nul_readings(program_texts)
}

/// `texts` plus, for each text holding a NUL byte, the two ways a reader
/// handles it: bash and sh DROP NUL bytes from a program read on stdin
/// (observed 2026-11-14: `printf 'ec\0ho x\n' | /bin/bash` prints `x`), and
/// zsh ends the word there (`… | zsh` reports `command not found: ec`), read
/// here as a line break so the text before the NUL is judged on its own
/// (backlog 00569f7d). The texts as given stay judged.
fn with_nul_readings(mut texts: Vec<String>) -> Vec<String> {
    let extra: Vec<String> = texts
        .iter()
        .filter(|t| t.contains('\0'))
        .flat_map(|t| [t.replace('\0', ""), t.replace('\0', "\n")])
        .collect();
    for t in extra {
        if !t.trim().is_empty() && !texts.contains(&t) {
            texts.push(t);
        }
    }
    texts
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
        // The operand and, when it holds a line continuation, its reading
        // with them removed, which is what the shell runs (backlog 040b959b).
        let (operands, undetermined) =
            super::readings::shell_program_readings(std::slice::from_ref(&span.operand));
        if let Some(deny) = undetermined.and_then(|ask| acc.record(ask)) {
            return deny;
        }
        let verdict = interpreter_code_verdict(
            &shape,
            &operands,
            depth,
            ctx,
            !span.single_quoted,
            PythonReader::NotPython,
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
///
/// `source` is the string `rest` was whitespace-split from; every candidate is
/// read off it (see [`faithful_tail`]), so the payload keeps its newlines.
pub(super) fn dash_c_payloads(rest: &[&str], source: Option<&str>) -> Vec<String> {
    let Some(pos) = rest
        .iter()
        .position(|t| is_short_flag(t) && t.contains('c'))
    else {
        return Vec::new();
    };
    let mut out = payloads_after(rest, pos, source);
    // Glued short form `-c<payload>` (value in the SAME token, after `c`):
    // `payloads_after` only sees LATER tokens, so `flock /l -c'rm -rf /'` left
    // the payload unanalysed — the twin of the `--command=` attached-form
    // bypass (138607bc). By getopt convention an option that takes an argument
    // is last in a short bundle, so everything after the first `c` is its value.
    if let Some(cidx) = rest[pos].find('c') {
        let inline = &rest[pos][cidx + 1..];
        out.extend(inline_command_payloads(rest, pos, inline, source));
    }
    out
}

/// Byte offset of `tok` inside `source` when `tok` IS a slice of `source` (the
/// same bytes in memory, not merely equal text). `None` otherwise.
fn slice_offset(source: &str, tok: &str) -> Option<usize> {
    let off = tok.as_ptr().addr().checked_sub(source.as_ptr().addr())?;
    let within = source.get(off..off.checked_add(tok.len())?)?;
    std::ptr::eq(within.as_ptr(), tok.as_ptr()).then_some(off)
}

/// The command-line text from `rest[from]` to the end of `rest`, exactly as it
/// stands in `source` — the string `rest` was whitespace-split from. `None`
/// when there is no source, `from` is out of range, or the tokens are not
/// slices of it.
///
/// Backlog 2026-11-13 (CRITICAL fail-open): the payload extractors used to
/// rebuild a `-c`/`-e` program by joining the whitespace-split tokens with ONE
/// space, so every newline inside the quoted program became a space.
/// `python3 -c "import os # x<NL>os.chmod('/etc/…',0)"` was then judged as a
/// single line whose `#` comment swallowed the call, and was Allowed, while
/// the same program without the comment was denied. Reading the words off the
/// source keeps the exact argument bytes — newlines and runs of blanks — that
/// the shell hands the interpreter after unquoting ([`first_shell_word`]).
pub(super) fn faithful_tail<'s>(
    source: Option<&'s str>,
    rest: &[&str],
    from: usize,
) -> Option<&'s str> {
    let source = source?;
    let first = rest.get(from)?;
    let last = rest.last()?;
    let start = slice_offset(source, first)?;
    let end = slice_offset(source, last)?.checked_add(last.len())?;
    source.get(start..end)
}

/// `rest[from..]` as text: [`faithful_tail`] when it can be read, otherwise the
/// whitespace re-join, in which newlines and runs of blanks are lost (see
/// [`word_separators_lost`] for when that loss makes the reading undetermined).
fn tail_text(source: Option<&str>, rest: &[&str], from: usize) -> String {
    faithful_tail(source, rest, from).map_or_else(|| rest[from..].join(" "), str::to_string)
}

/// Whether reading the shell words from `rest[from]` on needs whitespace the
/// split discarded and [`faithful_tail`] cannot recover: `source` does not hold
/// the tokens, and some word spans more than one token or never closes its
/// quote. The re-join then turns a newline inside the word into a space, so a
/// program judged on it is not the program that runs (a `#` comment swallows
/// the line after it) — the reading is undetermined, and a caller must not
/// Allow on it.
pub(super) fn word_separators_lost(source: Option<&str>, rest: &[&str], from: usize) -> bool {
    if from >= rest.len() || faithful_tail(source, rest, from).is_some() {
        return false;
    }
    let mut at = from;
    while at < rest.len() {
        match shell_word_token_span(rest, at) {
            Some(1) => at += 1,
            _ => return true,
        }
    }
    false
}

/// `rest[from..]` as a command line to re-analyse, read off `source` so a
/// quoted program keeps its newlines ([`faithful_tail`]). When the tokens are
/// not slices of `source` the whitespace re-join is used, unless that re-join
/// loses whitespace inside a shell word ([`word_separators_lost`]): then the
/// text that would run cannot be rebuilt and the `Err` is an Ask naming `what`.
///
/// Backlog e4523bfd (spec blastguard-comment-collapse-residual R1): the
/// unknown-wrapper tail (`myrunner python3 -c "import os # x<NL>os.chmod(…)"`)
/// was re-joined with one space, so the `#` comment swallowed the call and the
/// tail analysed to Allow.
pub(super) fn reanalysis_text(
    source: Option<&str>,
    rest: &[&str],
    from: usize,
    what: &str,
) -> Result<String, Decision> {
    if word_separators_lost(source, rest, from) {
        return Err(Decision::ask(format!(
            "{what} could not be read with its line breaks intact — blastguard cannot tell \
what command line it is, so it refuses to guess"
        )));
    }
    Ok(tail_text(source, rest, from))
}

/// The `dash_c_payloads` candidate set for a command-string flag already located
/// at index `pos`. Split out so callers with a different spelling of the flag
/// (e.g. flock's long form `--command`) reuse the identical candidate logic
/// rather than duplicating it. Each candidate is read off `source` when it
/// holds the tokens ([`faithful_tail`]); `None` keeps the whitespace re-join.
pub(super) fn payloads_after(rest: &[&str], pos: usize, source: Option<&str>) -> Vec<String> {
    let mut out = Vec::new();
    let mut push_first_word = |from: usize| {
        if from >= rest.len() {
            return;
        }
        let text = tail_text(source, rest, from);
        if let Some(word) = first_shell_word(&text) {
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

    // Candidate 2: legacy join-then-peel, read off the source like candidate 1.
    let joined = tail_text(source, rest, pos + 1);
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
/// A program whose assignment value holds a space and is followed by a
/// NEWLINE, `python3 -c "x='recursive rm'
/// y=a[:1]"`: its command word was `rm'` until `command_candidates` learned to
/// skip a quoted assignment's whole word (`literal_assignment_span`, backlog
/// ce04adeb), back when the program reached this function with its newlines
/// collapsed into spaces.
/// Positions strictly INSIDE a word whose quotes all close are
/// therefore not candidates; the word's whole text still is (and is read by the
/// full program analysis: shell pass, literal descent, destructive calls).
///
/// Each word's text is read off `source` — the string `rest` was
/// whitespace-split from — so the program keeps its newlines and runs of
/// blanks ([`faithful_tail`], backlog 2026-11-13: a collapsed newline let a
/// `#` comment swallow the system-harm call after it). Without a source the
/// tokens are re-joined by a space as before; the caller must then treat the
/// reading as undetermined wherever [`word_separators_lost`] says so.
///
/// Every word BOUNDARY after the flag stays a candidate — a later
/// `-e 'system "rm -rf /"'` (perl/ruby/node accept several) is still judged.
/// Undetermined stays on the widening side: from the first word whose quote
/// never closes, every remaining position is a candidate exactly as before.
pub(super) fn interpreter_payloads_after(
    rest: &[&str],
    pos: usize,
    source: Option<&str>,
) -> Vec<String> {
    let mut keep: Vec<String> = Vec::new();
    let mut boundary = pos + 1;
    while boundary < rest.len() {
        let Some(span) = shell_word_token_span(rest, boundary) else {
            // Undetermined word end: every position, as before.
            return payloads_after(rest, pos, source);
        };
        if let Some(word) = first_shell_word(&tail_text(source, rest, boundary)) {
            if !word.is_empty() && !keep.contains(&word) {
                keep.push(word);
            }
        }
        boundary += span;
    }
    // The legacy join-then-peel candidate is kept: it is not a mid-word
    // fragment.
    let joined = tail_text(source, rest, pos + 1);
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
/// depth-0 Allow bypass this closes (138607bc). `inline` is a slice of
/// `rest[pos]`, so with `source` the spliced text is still read off it.
pub(super) fn inline_command_payloads(
    rest: &[&str],
    pos: usize,
    inline: &str,
    source: Option<&str>,
) -> Vec<String> {
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
    payloads_after(&synthetic, 0, source)
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
