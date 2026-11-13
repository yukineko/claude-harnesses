//! Calls in an interpreter program that hand a command or a system file to
//! the OS (backlog 3c750df2). Read from the program's CODE, not from its
//! string literals: a string is a shell command line only when it is the
//! argument of a call that runs one. Kept apart from `interpreter.rs` for the
//! file-size cap; private to that module.

use super::super::{
    analyze_shell_payload, first_shell_word, normalized_command, quote_aware_words,
    wrapped_command_heads, Ctx, Decision,
};

/// Calls whose first argument is a shell command LINE (Python `os.system`,
/// `os.popen`, `subprocess.getoutput`, …; matched on the last dotted
/// component, so `from os import system` is covered).
const SHELL_LINE_CALLS: &[&str] = &["system", "popen", "getoutput", "getstatusoutput"];

/// `subprocess` functions: a shell line with `shell=True`, an argv list
/// otherwise.
const SUBPROCESS_CALLS: &[&str] = &["run", "call", "check_call", "check_output", "Popen"];

/// `os.exec*` / `os.spawn*` / `os.posix_spawn*`: `(path, argv…)` or
/// `(mode, path, argv…)`. Each one runs a program with an argv the program
/// spells, never through a shell.
const EXEC_CALLS: &[&str] = &[
    "execv",
    "execve",
    "execl",
    "execle",
    "execlp",
    "execlpe",
    "execvp",
    "execvpe",
    "spawnl",
    "spawnle",
    "spawnlp",
    "spawnlpe",
    "spawnv",
    "spawnve",
    "spawnvp",
    "spawnvpe",
    "posix_spawn",
    "posix_spawnp",
];

/// Calls that remove, move, truncate or re-permission the paths they are
/// given (last dotted component; `replace` only as `os.replace`, since
/// `str.replace` is everywhere).
const PATH_MUTATING_CALLS: &[&str] = &[
    "rename",
    "renames",
    "renameSync",
    "remove",
    "removedirs",
    "unlink",
    "unlinkSync",
    "rmdir",
    "rmdirSync",
    "rmtree",
    "rm_rf",
    "rm_r",
    "rm",
    "rmSync",
    "move",
    "mv",
    "truncate",
    "truncateSync",
    "chmod",
    "chmodSync",
    "lchmod",
    "chown",
    "chownSync",
    "lchown",
];

/// `pathlib` methods that write, remove, move or re-permission the path the
/// RECEIVER names (backlog 569b9b9d): `Path('/etc/hosts').chmod(0o777)`,
/// `pathlib.Path('/etc/hosts').write_text('x')`. Judged together with
/// [`PATH_MUTATING_CALLS`] (which already holds `chmod`, `unlink`, `rename`,
/// `rmdir`, …) only when the method is called directly on a `Path(…)`
/// constructor call ([`PATH_RECEIVERS`]); the receiver's string literals are
/// then the path the method acts on.
const PATH_METHOD_WRITES: &[&str] = &[
    "replace",
    "write_text",
    "write_bytes",
    "touch",
    "symlink_to",
    "hardlink_to",
];

/// Constructor names (last dotted component) whose call result is a path
/// object: `Path(…)`, `pathlib.Path(…)`, `PosixPath(…)`, `WindowsPath(…)`.
const PATH_RECEIVERS: &[&str] = &["Path", "PosixPath", "WindowsPath"];

/// Copy calls that WRITE the destination they are given (backlog 569b9b9d):
/// every string literal after the first top-level argument (the source, which
/// is only read) is a destination operand. Matched on the last dotted
/// component, except `copy`, which is matched only as `shutil.copy` or the
/// bare `copy` of `from shutil import copy` (`x.copy()` is everywhere).
const COPY_DEST_CALLS: &[&str] = &[
    "copy2",
    "copyfile",
    "copytree",
    "copyFile",
    "copyFileSync",
    "cpSync",
];

/// Shell commands that harm a system file named by ANY operand.
const ANY_OPERAND_MUTATORS: &[&str] = &[
    "rm", "rmdir", "unlink", "mv", "shred", "truncate", "chmod", "chown", "chgrp", "srm",
];

/// Shell commands that harm a system file named by their LAST operand (the
/// destination they overwrite); their sources are only read.
const DEST_OPERAND_MUTATORS: &[&str] = &["cp", "ln", "install"];

/// The verdict on the OS-facing calls in one layer-0 program text.
///
/// * A path-mutating call ([`PATH_MUTATING_CALLS`], `os.replace`) with a
///   string-literal argument in a system directory
///   ([`crate::scope::is_inside_system_dir`]) is a system-harm Deny
///   (`os.rename('/etc/hosts', '/tmp/x')`).
/// * An exec-style call ([`EXEC_CALLS`]) or an argv-list `subprocess` call is
///   re-assembled into the command it runs (`os.execv('/bin/rm', ['rm',
///   '/etc/passwd'])` runs `/bin/rm /etc/passwd`) and judged like a shell
///   line.
/// * The literal first argument of a shell-line call ([`SHELL_LINE_CALLS`],
///   `subprocess.*(…, shell=True)`) IS a shell command line: it is judged by
///   the full shell analyser, Asks included — so `os.system('$CMD')`, whose
///   command word only exists at run time, is not Allow — and by the
///   system-file mutator rule ([`ANY_OPERAND_MUTATORS`],
///   [`DEST_OPERAND_MUTATORS`]: `os.system('mv /etc/hosts /tmp/x')`).
///
/// A string literal that is not such an argument is data and is not judged
/// here (`note = "os.system('$CMD')"`, `s.replace("`$CMD`", "")`). A call
/// whose argument is not a literal (a variable) is not judged here either; it
/// stays with the rest of the program audit, as before. For a program Python
/// reads, the path arguments of path-mutating calls are also RESOLVED, not
/// matched token by token (`'/' 'etc/hosts'`), by
/// `python_semantics::path_mutation_verdict` (backlog f6dbff5b), which also
/// resolves the receiver of a pathlib write method and the destination of
/// `Path.replace` / `Path.rename` (backlog 17d985f1).
pub(super) fn program_call_verdict(payload: &str, depth: usize, ctx: &Ctx<'_>) -> Decision {
    program_calls(payload, depth, ctx, false)
}

/// [`program_call_verdict`], except that an Ask on the re-assembled argv of an
/// exec-style or argv-list `subprocess` call is kept, not dropped
/// (`subprocess.check_output(["git", "clean", "-fdx"])` reaches the git
/// rules' Ask). Used by the Python-file judge
/// ([`super::python_file_body_verdict`]), whose program is the whole file.
pub(super) fn program_call_verdict_keeping_argv_asks(
    payload: &str,
    depth: usize,
    ctx: &Ctx<'_>,
) -> Decision {
    program_calls(payload, depth, ctx, true)
}

fn program_calls(payload: &str, depth: usize, ctx: &Ctx<'_>, argv_asks: bool) -> Decision {
    let toks = lex(payload);
    let mut pending: Option<Decision> = None;
    for call in calls(&toks) {
        let last = call.name.rsplit('.').next().unwrap_or(&call.name);
        let strings = call.strings();
        if PATH_MUTATING_CALLS.contains(&last) || call.name == "os.replace" {
            if let Some(p) = strings
                .iter()
                .find(|s| crate::scope::is_inside_system_dir(s))
            {
                return Decision::deny_system_harm(format!(
                    "the program calls `{}` on `{p}`, a file in a system directory — removing, \
moving or re-permissioning an OS file can break the machine itself, so blastguard refuses it \
(user ruling 2026-11-07: a command harmful to the system is denied through any wrapper)",
                    call.name
                ));
            }
        }
        if call.on_path_receiver()
            && (PATH_MUTATING_CALLS.contains(&last) || PATH_METHOD_WRITES.contains(&last))
        {
            if let Some(p) = call
                .receiver_strings()
                .into_iter()
                .find(|s| crate::scope::is_inside_system_dir(s))
            {
                return Decision::deny_system_harm(format!(
                    "the program calls `.{last}()` on the path `{p}`, a file in a system \
directory — writing, removing, moving or re-permissioning an OS file can break the machine \
itself, so blastguard refuses it (user ruling 2026-11-07: a command harmful to the system is \
denied through any wrapper)"
                ));
            }
        }
        let copy_call =
            COPY_DEST_CALLS.contains(&last) || call.name == "shutil.copy" || call.name == "copy";
        if copy_call {
            if let Some(p) = call
                .strings_after_first_arg()
                .into_iter()
                .find(|s| crate::scope::is_inside_system_dir(s))
            {
                return Decision::deny_system_harm(format!(
                    "the program calls `{}` with the destination `{p}`, a file in a system \
directory — overwriting or creating an OS file can break the machine itself, so blastguard \
refuses it (user ruling 2026-11-07: a command harmful to the system is denied through any \
wrapper)",
                    call.name
                ));
            }
        }
        let shell_line = SHELL_LINE_CALLS.contains(&last)
            || (SUBPROCESS_CALLS.contains(&last) && call.shell_true());
        let verdict = if shell_line {
            match call.first_arg_string() {
                Some(line) => shell_line_verdict(&call.name, line, depth, ctx),
                None => Decision::Allow,
            }
        } else if EXEC_CALLS.contains(&last) {
            // `(path, argv0, args…)`, after an optional mode (a non-literal);
            // argv0 is replaced by the path that really runs.
            let words: Vec<&str> = match strings.as_slice() {
                [path, _argv0, args @ ..] => {
                    std::iter::once(*path).chain(args.iter().copied()).collect()
                }
                other => other.to_vec(),
            };
            argv_verdict(&call.name, &words, depth, ctx, argv_asks)
        } else if SUBPROCESS_CALLS.contains(&last) && call.name.starts_with("subprocess.") {
            argv_verdict(&call.name, &strings, depth, ctx, argv_asks)
        } else {
            Decision::Allow
        };
        match verdict {
            d @ Decision::Deny(_) => return d,
            a @ Decision::Ask(_) => {
                pending.get_or_insert(a);
            }
            Decision::Allow => {}
        }
    }
    pending.unwrap_or(Decision::Allow)
}

/// The first call in `payload` that hands the OS a command the program
/// COMPUTES, which [`program_call_verdict`] leaves unjudged (its argument is
/// not a literal): a shell-line call (`os.system`, `os.popen`, `system`,
/// `popen`, `subprocess.*` / `commands.*` `getoutput`/`getstatusoutput`, a
/// `subprocess.*` call with `shell=True`) whose first argument is not one
/// string literal, an argv-list `subprocess.*` call whose program (first
/// element) is not a literal, or an exec-style call ([`EXEC_CALLS`]) with no
/// string literal at all. `platform.system()` and other unrelated methods named
/// like these are not matched: the name must be bare or `os.` / `subprocess.`
/// / `commands.` qualified. Used by the Python-file judge
/// ([`super::python_file_body_verdict`]), where it is an Ask.
pub(super) fn computed_shell_call(payload: &str) -> Option<String> {
    let toks = lex(payload);
    for call in calls(&toks) {
        let (module, last) = match call.name.rsplit_once('.') {
            Some((m, l)) => (m, l),
            None => ("", call.name.as_str()),
        };
        if !matches!(module, "" | "os" | "subprocess" | "commands") {
            continue;
        }
        let literal_head = matches!(
            call.args,
            [Tok::Str(_), ..] | [Tok::Punct('[' | '('), Tok::Str(_), ..]
        );
        let computed = if SHELL_LINE_CALLS.contains(&last)
            || (module == "subprocess" && SUBPROCESS_CALLS.contains(&last) && call.shell_true())
        {
            call.first_arg_string().is_none()
        } else if module == "subprocess" && SUBPROCESS_CALLS.contains(&last) {
            !literal_head
        } else if EXEC_CALLS.contains(&last) && module != "subprocess" {
            call.strings().is_empty()
        } else {
            false
        };
        if computed {
            return Some(call.name.clone());
        }
    }
    None
}

/// The verdict of one top-level segment, with the "command word is an
/// expansion" Ask dropped when the segment is a BODY line of a quoted-delimiter
/// here-document read by Python ([`super::super::python_program_body_segments`]:
/// `python3 - <<'PY'` … `PY`) (backlog 3c750df2, f6).
///
/// The shell does not parse that body at all (quoted delimiter: no expansion,
/// no substitution), so a `$CMD` there is never a command word the shell runs.
/// Whether the PROGRAM runs it is the program audit's question
/// ([`super::interpreter_code_verdict`], which reads the same body): a literal
/// handed to `os.system` / `subprocess(…, shell=True)` is judged there as the
/// shell line it is ([`program_call_verdict`]), so `os.system('$CMD')` is
/// still an Ask, while `s.replace("`$CMD`", "")` is data. Any other verdict,
/// and every segment that is not such a body line, is returned unchanged.
pub(in crate::detect) fn python_body_segment_verdict(
    line: &str,
    seg_idx: usize,
    verdict: Decision,
) -> Decision {
    if matches!(&verdict, Decision::Ask(r) if r.contains(super::super::EXPANSION_HEAD_TAIL))
        && super::super::python_program_body_segments(line).get(seg_idx) == Some(&true)
    {
        return Decision::Allow;
    }
    verdict
}

/// A shell command line handed to `call`.
fn shell_line_verdict(call: &str, line: &str, depth: usize, ctx: &Ctx<'_>) -> Decision {
    for simple in split_simple_commands(line) {
        let words: Vec<String> = quote_aware_words(&simple)
            .iter()
            .map(|w| first_shell_word(w).unwrap_or_else(|| w.clone()))
            .collect();
        let refs: Vec<&str> = words.iter().map(String::as_str).collect();
        if let Some(deny) = system_file_mutation(call, &refs) {
            return deny;
        }
    }
    match analyze_shell_payload(line, depth, ctx) {
        Decision::Allow => Decision::Allow,
        Decision::Deny(r) => Decision::Deny(r),
        Decision::Ask(r) => Decision::ask(format!(
            "the program hands `{line}` to a shell through `{call}`: {r}"
        )),
    }
}

/// An argv a call executes directly. Its Ask is kept only when `keep_ask`.
fn argv_verdict(
    call: &str,
    words: &[&str],
    depth: usize,
    ctx: &Ctx<'_>,
    keep_ask: bool,
) -> Decision {
    if words.is_empty() {
        return Decision::Allow;
    }
    if let Some(deny) = system_file_mutation(call, words) {
        return deny;
    }
    let line = words
        .iter()
        .map(|w| shell_quote(w))
        .collect::<Vec<_>>()
        .join(" ");
    match analyze_shell_payload(&line, depth, ctx) {
        Decision::Deny(r) => Decision::Deny(r),
        Decision::Ask(r) if keep_ask => {
            Decision::ask(format!("the program runs `{line}` through `{call}`: {r}"))
        }
        // Otherwise only a Deny is taken from the re-joined argv, as for the
        // argv-list `subprocess` re-join in the program audit: the join is a
        // model of the exec, not a line a shell parses.
        _ => Decision::Allow,
    }
}

/// System-harm Deny when the command `words` run is a file mutator with a
/// system-directory operand. Every head candidate behind a wrapper chain is
/// tried (deny if ANY).
fn system_file_mutation(call: &str, words: &[&str]) -> Option<Decision> {
    for head in wrapped_command_heads(words) {
        let cmd = normalized_command(words[head]);
        let mut operands: Vec<&str> = Vec::new();
        let mut end_of_flags = false;
        for w in &words[head + 1..] {
            if !end_of_flags && *w == "--" {
                end_of_flags = true;
            } else if end_of_flags || !w.starts_with('-') {
                operands.push(w);
            }
        }
        let judged: &[&str] = if ANY_OPERAND_MUTATORS.contains(&cmd.as_str()) {
            &operands
        } else if DEST_OPERAND_MUTATORS.contains(&cmd.as_str()) {
            operands.last().map(std::slice::from_ref).unwrap_or(&[])
        } else {
            &[]
        };
        if let Some(p) = judged
            .iter()
            .find(|p| crate::scope::is_inside_system_dir(p))
        {
            return Some(Decision::deny_system_harm(format!(
                "the program runs `{cmd}` on `{p}` through `{call}` — a file in a system \
directory; changing or removing an OS file can break the machine itself, so blastguard refuses \
it (user ruling 2026-11-07: a command harmful to the system is denied through any wrapper)"
            )));
        }
    }
    None
}

/// `line` split at unquoted `;`, `&`, `|` and newlines. Only a model for the
/// mutator rule: the full shell analyser judges `line` itself as well.
fn split_simple_commands(line: &str) -> Vec<String> {
    let mut out = vec![String::new()];
    let (mut in_s, mut in_d) = (false, false);
    for c in line.chars() {
        match c {
            '\'' if !in_d => in_s = !in_s,
            '"' if !in_s => in_d = !in_d,
            ';' | '&' | '|' | '\n' if !in_s && !in_d => {
                out.push(String::new());
                continue;
            }
            _ => {}
        }
        if let Some(cur) = out.last_mut() {
            cur.push(c);
        }
    }
    out.retain(|s| !s.trim().is_empty());
    out
}

/// `w` as one shell word.
fn shell_quote(w: &str) -> String {
    if !w.is_empty()
        && w.chars()
            .all(|c| c.is_ascii_alphanumeric() || "_/.,:=@%+-".contains(c))
    {
        w.to_string()
    } else {
        format!("'{}'", w.replace('\'', "'\\''"))
    }
}

/// One lexical item of a program, read with Python's rules (which cover the
/// call shapes of the other interpreters closely enough for this purpose: a
/// miss leaves the program to the rest of the audit, never to Allow).
#[derive(Debug, PartialEq)]
enum Tok {
    /// A string literal's contents (prefix and quotes removed).
    Str(String),
    /// A dotted name: `os.system`, `subprocess.run`, `True`.
    Name(String),
    Punct(char),
}

/// Lex `src`: `#` comments dropped, `'…'` / `"…"` / triple-quoted strings
/// (with an optional `r`/`b`/`u`/`f` prefix) as [`Tok::Str`], dotted
/// identifiers joined. An unterminated string runs to the end of the text.
fn lex(src: &str) -> Vec<Tok> {
    let chars: Vec<char> = src.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '#' {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if c == '\'' || c == '"' {
            let triple = chars.get(i + 1) == Some(&c) && chars.get(i + 2) == Some(&c);
            let q = if triple { 3 } else { 1 };
            i += q;
            let mut s = String::new();
            while i < chars.len() {
                if chars[i] == '\\' {
                    if let Some(&e) = chars.get(i + 1) {
                        s.push(e);
                    }
                    i += 2;
                    continue;
                }
                if chars[i] == c
                    && (!triple || (chars.get(i + 1) == Some(&c) && chars.get(i + 2) == Some(&c)))
                {
                    i += q;
                    break;
                }
                s.push(chars[i]);
                i += 1;
            }
            out.push(Tok::Str(s));
            continue;
        }
        if c.is_alphanumeric() || c == '_' {
            let start = i;
            while i < chars.len()
                && (chars[i].is_alphanumeric() || chars[i] == '_' || chars[i] == '.')
            {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            // A string prefix (`r'…'`, `f"…"`, `rb'…'`) belongs to the string.
            if matches!(chars.get(i), Some('\'' | '"'))
                && word.len() <= 2
                && word.chars().all(|p| "rRbBuUfF".contains(p))
            {
                continue;
            }
            out.push(Tok::Name(word.trim_end_matches('.').to_string()));
            continue;
        }
        if !c.is_whitespace() {
            out.push(Tok::Punct(c));
        }
        i += 1;
    }
    out
}

/// A call site: a [`Tok::Name`] directly followed by `(`, with the tokens up
/// to its matching `)` (to the end of the text when unbalanced).
struct Call<'a> {
    name: String,
    args: &'a [Tok],
    /// For a method called directly on a call result (`Path('/x').chmod(…)`):
    /// the receiver call's name (`Path`) and its argument tokens. `None` for
    /// every other call.
    receiver: Option<(&'a str, &'a [Tok])>,
}

impl Call<'_> {
    /// The method is called directly on a path constructor ([`PATH_RECEIVERS`]).
    fn on_path_receiver(&self) -> bool {
        self.receiver.is_some_and(|(name, _)| {
            PATH_RECEIVERS.contains(&name.rsplit('.').next().unwrap_or(name))
        })
    }

    /// Every string literal among the receiver call's arguments.
    fn receiver_strings(&self) -> Vec<&str> {
        self.receiver
            .map(|(_, args)| str_toks(args))
            .unwrap_or_default()
    }

    /// Every string literal after the first top-level `,` of the arguments
    /// (nested lists included). A first argument that is not a literal is
    /// skipped all the same: the literals after it are the later operands.
    fn strings_after_first_arg(&self) -> Vec<&str> {
        let mut depth = 0usize;
        let mut split = self.args.len();
        for (k, t) in self.args.iter().enumerate() {
            match t {
                Tok::Punct('(' | '[' | '{') => depth += 1,
                Tok::Punct(')' | ']' | '}') => depth = depth.saturating_sub(1),
                Tok::Punct(',') if depth == 0 => {
                    split = k;
                    break;
                }
                _ => {}
            }
        }
        str_toks(&self.args[split..])
    }

    /// Every string literal among the arguments, nested lists included.
    fn strings(&self) -> Vec<&str> {
        self.args
            .iter()
            .filter_map(|t| match t {
                Tok::Str(s) => Some(s.as_str()),
                _ => None,
            })
            .collect()
    }

    /// The first argument, when it is one string literal.
    fn first_arg_string(&self) -> Option<&str> {
        match self.args {
            [Tok::Str(s), Tok::Punct(',' | ')'), ..] | [Tok::Str(s)] => Some(s.as_str()),
            _ => None,
        }
    }

    /// `shell=True` among the arguments.
    fn shell_true(&self) -> bool {
        self.args.windows(3).any(|w| {
            w[0] == Tok::Name("shell".to_string())
                && w[1] == Tok::Punct('=')
                && w[2] == Tok::Name("True".to_string())
        })
    }
}

fn calls(toks: &[Tok]) -> Vec<Call<'_>> {
    let mut out = Vec::new();
    for (i, t) in toks.iter().enumerate() {
        let Tok::Name(name) = t else { continue };
        if toks.get(i + 1) != Some(&Tok::Punct('(')) {
            continue;
        }
        let start = i + 2;
        let mut depth = 1usize;
        let mut end = toks.len();
        for (k, t) in toks.iter().enumerate().skip(start) {
            match t {
                Tok::Punct('(' | '[' | '{') => depth += 1,
                Tok::Punct(')' | ']' | '}') => {
                    depth -= 1;
                    if depth == 0 {
                        end = k;
                        break;
                    }
                }
                _ => {}
            }
        }
        out.push(Call {
            name: name.clone(),
            args: &toks[start.min(toks.len())..end],
            receiver: receiver_call(toks, i),
        });
    }
    out
}

/// Every [`Tok::Str`] in `toks`.
fn str_toks(toks: &[Tok]) -> Vec<&str> {
    toks.iter()
        .filter_map(|t| match t {
            Tok::Str(s) => Some(s.as_str()),
            _ => None,
        })
        .collect()
}

/// When the call whose name is at `toks[at]` is a method on a call result —
/// `NAME ( … ) . method (` — that receiver call's name and argument tokens.
fn receiver_call(toks: &[Tok], at: usize) -> Option<(&str, &[Tok])> {
    if at < 2
        || toks.get(at - 1) != Some(&Tok::Punct('.'))
        || toks.get(at - 2) != Some(&Tok::Punct(')'))
    {
        return None;
    }
    let close = at - 2;
    let mut depth = 0usize;
    let mut k = close;
    loop {
        match toks[k] {
            Tok::Punct(')' | ']' | '}') => depth += 1,
            Tok::Punct('(' | '[' | '{') => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            _ => {}
        }
        k = k.checked_sub(1)?;
    }
    if toks.get(k) != Some(&Tok::Punct('(')) {
        return None;
    }
    let Tok::Name(name) = toks.get(k.checked_sub(1)?)? else {
        return None;
    };
    Some((name.as_str(), &toks[k + 1..close]))
}

#[cfg(test)]
mod tests {
    use super::{calls, lex, Tok};

    #[test]
    fn strings_are_not_calls() {
        let toks = lex("note = \"os.system('$CMD')\"\ns=s.replace(\"a `$CMD` b\",\"c\")");
        let names: Vec<String> = calls(&toks).into_iter().map(|c| c.name).collect();
        assert_eq!(names, vec!["s.replace".to_string()]);
    }

    #[test]
    fn first_argument_literal_and_shell_flag() {
        let toks = lex("subprocess.run('rm -rf ~', shell=True)  # x os.system('y')");
        let cs = calls(&toks);
        assert_eq!(cs.len(), 1);
        assert_eq!(cs[0].first_arg_string(), Some("rm -rf ~"));
        assert!(cs[0].shell_true());
        assert_eq!(lex("r'a\\'b'"), vec![Tok::Str("a'b".to_string())]);
    }
}
