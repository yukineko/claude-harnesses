//! Programs that EXECUTE text they are handed, but are neither a shell
//! ([`super::is_shell`]) nor a code interpreter ([`super::is_code_interpreter`]).
//!
//! daf7611b: every one of these reached `Allow` unexamined, because no rule arm
//! matched their command word and the text they run was just an operand:
//!
//! * `awk 'BEGIN{system("rm -rf ~/x")}'` — awk's `system()`, and its `|`
//!   pipes to/from a command, hand a string to `sh -c`.
//! * `git -c core.pager="rm -rf ~/x" log` — a family of git config keys are
//!   command lines git runs through the shell (pager, editor, ssh command,
//!   `!`-aliases, credential helpers, diff/merge drivers, …).
//! * `env -S "rm -rf ~/x"` — `-S`/`--split-string` splits ONE operand into a
//!   command line and execs it. The operand is a quoted multi-word string, which
//!   is exactly what [`super::command_candidates`] refuses to read as a command
//!   word, so the only way to see it is to extract it here.
//!
//! The rule is the one the shell `-c` arm already follows: where the text is a
//! shell command, extract it and hand it to [`super::analyze_shell_payload`]
//! (so a recognisable `rm -rf` is a Deny); where the executed text is
//! assembled at run time and is not on the command line, that is "could not
//! analyse" and resolves to Ask (CLAUDE.md §3), never to Allow.

use super::{
    analyze_git, analyze_shell_payload, depth_exhausted, first_shell_word, inline_command_payloads,
    is_assignment, is_code_interpreter, is_shell, is_short_flag, payloads_after, quote_aware_words,
    unknown_verb_protected_ask, unquote_literal_value, Ctx, VerdictAcc, MAX_SHELL_DEPTH,
};
use crate::model::Decision;

// ---------------------------------------------------------------- awk -------

/// awk implementations. `busybox awk` reaches here through the exec-wrapper
/// candidate scan.
pub(super) fn is_awk(cmd: &str) -> bool {
    matches!(cmd, "awk" | "gawk" | "mawk" | "nawk")
}

/// Judge an awk invocation by the program text on its command line.
///
/// A program with no shell-reaching construct (`{print $1}`) runs nothing and
/// is `Allow`. A program that reaches the shell has every string literal
/// analysed as a shell command line (Deny wins); a shell-reaching site whose
/// command is not a lone literal is Ask, because what runs is computed at run
/// time. A program supplied only by `-f FILE` is not on the command line; it is
/// treated like `python3 script.py` (the script-file Allow of
/// [`super::analyze_code_interpreter`]).
pub(super) fn analyze_awk(cmd: &str, rest: &[&str], depth: usize, ctx: &Ctx<'_>) -> Decision {
    let mut acc = VerdictAcc::default();
    for program in awk_programs(rest) {
        if let Some(deny) = acc.record(awk_program_verdict(cmd, &program, depth, ctx)) {
            return deny;
        }
    }
    // awk is also an EDITOR (`awk -i inplace 1 .claude/settings.json`): its
    // file operands go through the same protected-path check every verb
    // without its own arm gets.
    if let Some(deny) = acc.record(unknown_verb_protected_ask(cmd, rest)) {
        return deny;
    }
    acc.finish()
}

/// The inline program texts of an awk invocation, unquoted.
///
/// Options are walked on quote-aware words so a quoted program containing
/// spaces is one word. Value-taking options in separate form (`-F :`, `-v x=1`,
/// `-f prog.awk`) consume their value; `-e`/`--source` (gawk) supply a program.
/// The first plain operand is the program unless `-f`/`-e` already supplied
/// one — after it come input files, which are data.
fn awk_programs(rest: &[&str]) -> Vec<String> {
    let words = quote_aware_words(&rest.join(" "));
    let unquote = |w: &str| first_shell_word(w).unwrap_or_default();
    let mut programs = Vec::new();
    let mut from_file = false;
    let mut i = 0;
    while i < words.len() {
        let w = words[i].as_str();
        if w == "--" {
            if !from_file && programs.is_empty() {
                if let Some(p) = words.get(i + 1) {
                    programs.push(unquote(p));
                }
            }
            break;
        }
        if w.starts_with('-') && w.len() > 1 {
            match w {
                "-f" | "--file" => {
                    from_file = true;
                    i += 2;
                    continue;
                }
                "-e" | "--source" => {
                    if let Some(p) = words.get(i + 1) {
                        programs.push(unquote(p));
                    }
                    i += 2;
                    continue;
                }
                "-F" | "-v" | "-W" | "-i" | "-l" | "--field-separator" | "--assign"
                | "--include" | "--load" => {
                    i += 2;
                    continue;
                }
                _ => {}
            }
            if let Some(p) = w.strip_prefix("--source=") {
                programs.push(unquote(p));
            } else if w.starts_with("--file=") || (w.starts_with("-f") && !w.starts_with("--")) {
                from_file = true;
            }
            i += 1;
            continue;
        }
        if !from_file && programs.is_empty() {
            programs.push(unquote(w));
        }
        break;
    }
    programs
}

/// One lexeme of an awk program. Only what the shell-site scan needs.
#[derive(Debug, PartialEq)]
enum AwkLex {
    Str(String),
    Word(String),
    Op(String),
}

/// Lex an awk program: strings (with escapes resolved), identifiers, and
/// operators. Comments and regular-expression literals are skipped, so a `|`
/// inside `/foo|bar/` is not mistaken for a pipe. A newline is a statement
/// terminator and lexes as `;`.
fn lex_awk(p: &str) -> Vec<AwkLex> {
    let chars: Vec<char> = p.chars().collect();
    let mut out: Vec<AwkLex> = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\n' {
            out.push(AwkLex::Op(";".into()));
            i += 1;
        } else if c.is_whitespace() {
            i += 1;
        } else if c == '#' {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
        } else if c == '"' {
            let mut s = String::new();
            i += 1;
            while i < chars.len() && chars[i] != '"' {
                if chars[i] == '\\' && i + 1 < chars.len() {
                    match chars[i + 1] {
                        '"' => s.push('"'),
                        '\\' => s.push('\\'),
                        '/' => s.push('/'),
                        'n' => s.push('\n'),
                        't' => s.push('\t'),
                        other => {
                            s.push('\\');
                            s.push(other);
                        }
                    }
                    i += 2;
                } else {
                    s.push(chars[i]);
                    i += 1;
                }
            }
            i += 1; // closing quote (or end of an unterminated string)
            out.push(AwkLex::Str(s));
        } else if c == '/' && regex_may_start(out.last()) {
            i += 1;
            while i < chars.len() && chars[i] != '/' {
                if chars[i] == '\\' {
                    i += 1;
                }
                i += 1;
            }
            i += 1;
        } else if c.is_ascii_alphanumeric() || c == '_' {
            let start = i;
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            out.push(AwkLex::Word(chars[start..i].iter().collect()));
        } else if c == '|' {
            match chars.get(i + 1) {
                Some('|') => {
                    out.push(AwkLex::Op("||".into()));
                    i += 2;
                }
                Some('&') => {
                    out.push(AwkLex::Op("|&".into()));
                    i += 2;
                }
                _ => {
                    out.push(AwkLex::Op("|".into()));
                    i += 1;
                }
            }
        } else {
            out.push(AwkLex::Op(c.to_string()));
            i += 1;
        }
    }
    out
}

/// A `/` opens a regex unless it follows an operand (identifier, number,
/// string, `)` or `]`), where it is division.
fn regex_may_start(prev: Option<&AwkLex>) -> bool {
    match prev {
        None => true,
        Some(AwkLex::Op(op)) => op != ")" && op != "]" && op != "$",
        Some(_) => false,
    }
}

fn is_op(l: Option<&AwkLex>, ops: &[&str]) -> bool {
    matches!(l, Some(AwkLex::Op(op)) if ops.contains(&op.as_str()))
}

/// What the shell-site scan of one awk program found.
#[derive(Default)]
struct AwkShellSites {
    /// Number of `system(…)` calls and command pipes.
    sites: usize,
    /// The first site whose command is not a lone string literal.
    computed: Option<&'static str>,
}

fn scan_awk_shell_sites(lex: &[AwkLex]) -> AwkShellSites {
    let mut found = AwkShellSites::default();
    let note_computed = |found: &mut AwkShellSites, what: &'static str| {
        if found.computed.is_none() {
            found.computed = Some(what);
        }
    };
    for (i, l) in lex.iter().enumerate() {
        match l {
            AwkLex::Word(w) if w == "system" && is_op(lex.get(i + 1), &["("]) => {
                found.sites += 1;
                // The argument is a lone literal iff `( "…" )`.
                let lone =
                    matches!(lex.get(i + 2), Some(AwkLex::Str(_))) && is_op(lex.get(i + 3), &[")"]);
                if !lone {
                    note_computed(&mut found, "system() with a computed argument");
                }
            }
            AwkLex::Op(op) if op == "|" || op == "|&" => {
                found.sites += 1;
                if matches!(lex.get(i + 1), Some(AwkLex::Word(w)) if w == "getline") {
                    // `"cmd" | getline`: the command is the operand on the left,
                    // and must be a whole literal, not the tail of a
                    // concatenation (`"ls " dir | getline`).
                    let lone = i >= 1
                        && matches!(lex.get(i - 1), Some(AwkLex::Str(_)))
                        && (i < 2 || is_op(lex.get(i - 2), &["(", "{", "}", ";"]));
                    if !lone {
                        note_computed(&mut found, "a computed command piped into getline");
                    }
                } else {
                    // `print … | "cmd"`: the command is on the right and must
                    // be a whole literal. What is PRINTED into it is data the
                    // command reads; when that command is itself a shell or
                    // interpreter, the printed data is the program, and it is
                    // whatever the awk expression computes.
                    match lex.get(i + 1) {
                        Some(AwkLex::Str(cmdline))
                            if lex.get(i + 2).is_none()
                                || is_op(lex.get(i + 2), &[";", "}", ")"]) =>
                        {
                            let head = first_shell_word(cmdline).unwrap_or_default();
                            let head = super::basename(&head);
                            if is_shell(head) || is_code_interpreter(head) {
                                note_computed(
                                    &mut found,
                                    "output piped into a shell/interpreter, which runs it",
                                );
                            }
                        }
                        _ => note_computed(&mut found, "output piped into a computed command"),
                    }
                }
            }
            _ => {}
        }
    }
    found
}

fn awk_program_verdict(cmd: &str, program: &str, depth: usize, ctx: &Ctx<'_>) -> Decision {
    let lex = lex_awk(program);
    let sites = scan_awk_shell_sites(&lex);
    if sites.sites == 0 {
        return Decision::Allow;
    }
    if depth >= MAX_SHELL_DEPTH {
        return depth_exhausted();
    }
    let mut acc = VerdictAcc::default();
    // Every literal, not only the site arguments: `x = "rm -rf ~/x"; system(x)`
    // carries its command in a literal that is not the call's argument.
    for l in &lex {
        if let AwkLex::Str(s) = l {
            if let Some(deny) = acc.record(analyze_shell_payload(s, depth, ctx)) {
                return deny;
            }
        }
    }
    if let Some(what) = sites.computed {
        let _ = acc.record(Decision::ask(format!(
            "`{cmd}` runs a shell command from its program ({what}) — the command is assembled \
when awk runs, so blastguard cannot see what would actually execute"
        )));
    }
    acc.finish()
}

// ---------------------------------------------------------------- git -------

/// How a git config key's value is executed.
enum GitKeyKind {
    /// A shell command line (git runs it via `sh -c`). `bool_ok` keys also
    /// accept a boolean, which runs nothing (`pager.log=false`).
    Shell { bool_ok: bool },
    /// `alias.<name>`: `!cmd` is a shell line, anything else is a git command.
    Alias,
    /// `include.path` / `includeIf.<cond>.path`: pulls in a config FILE, which
    /// can set any key above; its contents are not on the command line.
    Include,
}

fn git_key_kind(key: &str) -> Option<GitKeyKind> {
    let shell = Some(GitKeyKind::Shell { bool_ok: false });
    match key {
        "core.pager"
        | "core.editor"
        | "core.sshcommand"
        | "core.askpass"
        | "core.gitproxy"
        | "sequence.editor"
        | "diff.external"
        | "gpg.program"
        | "sendemail.tocmd"
        | "sendemail.cccmd"
        | "uploadpack.packobjectshook"
        | "credential.helper" => return shell,
        "core.fsmonitor" => return Some(GitKeyKind::Shell { bool_ok: true }),
        "include.path" => return Some(GitKeyKind::Include),
        _ => {}
    }
    let parts: Vec<&str> = key.split('.').collect();
    let (first, last) = (parts.first().copied()?, parts.last().copied()?);
    match (first, parts.len(), last) {
        ("pager", 2, _) => Some(GitKeyKind::Shell { bool_ok: true }),
        ("alias", 2, _) => Some(GitKeyKind::Alias),
        ("includeif", n, "path") if n >= 3 => Some(GitKeyKind::Include),
        (_, n, _) if n < 3 => None,
        ("credential", _, "helper")
        | ("diff", _, "textconv" | "command")
        | ("merge", _, "driver")
        | ("filter", _, "clean" | "smudge" | "process")
        | ("gpg", _, "program")
        | ("difftool" | "mergetool" | "browser" | "man", _, "cmd")
        | ("remote", _, "uploadpack" | "receivepack") => shell,
        _ => None,
    }
}

fn is_git_bool(v: &str) -> bool {
    matches!(
        v.trim().to_ascii_lowercase().as_str(),
        "" | "true" | "false" | "yes" | "no" | "on" | "off" | "1" | "0"
    )
}

/// Judge the command-valued config set by git's GLOBAL `-c` /
/// `--config-env` options for the duration of this one command.
///
/// `git -c core.pager="rm -rf ~/x" log` runs the pager through `sh -c`; the
/// value is analysed as that shell command line. `--config-env key=VAR` takes
/// the value from the environment, which is not on the command line, so a
/// command-valued key set that way is Ask.
pub(super) fn git_exec_config_verdict(rest: &[&str], depth: usize, ctx: &Ctx<'_>) -> Decision {
    const VALUE_SEPARATE: &[&str] = &[
        "-C",
        "--git-dir",
        "--work-tree",
        "--namespace",
        "--super-prefix",
    ];
    let words = quote_aware_words(&rest.join(" "));
    let mut acc = VerdictAcc::default();
    let mut i = 0;
    while i < words.len() {
        let w = words[i].as_str();
        if w == "--" || !w.starts_with('-') {
            break;
        }
        let (assignment, from_env, step) = if w == "-c" {
            (words.get(i + 1).map(String::as_str), false, 2)
        } else if w == "--config-env" {
            (words.get(i + 1).map(String::as_str), true, 2)
        } else if VALUE_SEPARATE.contains(&w) {
            (None, false, 2)
        } else if let Some(v) = w.strip_prefix("--config-env=") {
            (Some(v), true, 1)
        } else if let Some(v) = w.strip_prefix("-c").filter(|v| !v.is_empty()) {
            (Some(v), false, 1)
        } else {
            (None, false, 1)
        };
        if let Some(a) = assignment {
            let a = first_shell_word(a).unwrap_or_default();
            if let Some(deny) = acc.record(judge_git_config(&a, from_env, depth, ctx)) {
                return deny;
            }
        }
        i += step;
    }
    acc.finish()
}

fn judge_git_config(assignment: &str, from_env: bool, depth: usize, ctx: &Ctx<'_>) -> Decision {
    // `-c key` with no `=` sets the boolean `true`: nothing to run.
    let Some((key, value)) = assignment.split_once('=') else {
        return Decision::Allow;
    };
    let key = key.trim().to_ascii_lowercase();
    let Some(kind) = git_key_kind(&key) else {
        return Decision::Allow;
    };
    if from_env && !matches!(kind, GitKeyKind::Include) {
        return Decision::ask(format!(
            "`git --config-env {key}=…` takes a command git will run from an environment \
variable — its value only exists at run time, so blastguard cannot see what would execute"
        ));
    }
    if depth >= MAX_SHELL_DEPTH {
        return depth_exhausted();
    }
    match kind {
        GitKeyKind::Include => Decision::ask(format!(
            "`git -c {key}=…` loads a config file for this command, and that file can set a \
pager, editor, alias or hook that git will run — its contents are not on the command line, so \
blastguard cannot see what would execute"
        )),
        GitKeyKind::Shell { bool_ok } => {
            if bool_ok && is_git_bool(value) {
                return Decision::Allow;
            }
            // `credential.helper=!cmd` is a shell line after the `!`; a bare
            // name is `git credential-<name>`, a path is that program.
            let line = value.strip_prefix('!').unwrap_or(value);
            analyze_shell_payload(line, depth, ctx)
        }
        GitKeyKind::Alias => match value.strip_prefix('!') {
            Some(line) => analyze_shell_payload(line, depth, ctx),
            // A non-shell alias expands to a git command line, which the
            // `git` arm judges like any other.
            None => {
                let tokens: Vec<&str> = value.split_whitespace().collect();
                analyze_git(&tokens, ctx)
            }
        },
    }
}

// ------------------------------------------------------- exec env vars ------

/// Environment variables whose VALUE a program runs as a command line: git's
/// pager / editors / ssh / askpass / proxy / external diff, the generic
/// `PAGER`/`MANPAGER`/`EDITOR`/`VISUAL` that man, crontab, less and friends
/// honour, and the same mechanism in sudo, the shell's `fc`, gh, kubectl,
/// systemd, git web--browse and less's input preprocessor. `GIT_SSH`,
/// `*_ASKPASS` and `BROWSER` name a program rather than a shell line; judging
/// the value as a shell line is a superset of that.
const EXEC_ENV_VARS: &[&str] = &[
    "GIT_PAGER",
    "GIT_EDITOR",
    "GIT_SEQUENCE_EDITOR",
    "GIT_SSH_COMMAND",
    "GIT_SSH",
    "GIT_EXTERNAL_DIFF",
    "GIT_ASKPASS",
    "SSH_ASKPASS",
    "GIT_PROXY_COMMAND",
    "PAGER",
    "MANPAGER",
    "EDITOR",
    "VISUAL",
    "SUDO_ASKPASS",
    "SUDO_EDITOR",
    "FCEDIT",
    "GH_PAGER",
    "GH_EDITOR",
    "KUBE_EDITOR",
    "SYSTEMD_PAGER",
    "BROWSER",
    "LESSOPEN",
    "LESSCLOSE",
];

/// Environment variables that make a program LOAD code or configuration from a
/// file or directory: git's global/system config and exec/template dirs (each
/// can set or supply something git runs), the startup file non-interactive
/// bash (`BASH_ENV`) and an interactive POSIX sh (`ENV`) source, and the
/// dynamic loader's preload lists. What runs is in the file, not on the
/// command line, so a non-trivial value is Ask (the `include.path` answer).
const LOAD_ENV_VARS: &[&str] = &[
    "GIT_CONFIG_GLOBAL",
    "GIT_CONFIG_SYSTEM",
    "GIT_EXEC_PATH",
    "GIT_TEMPLATE_DIR",
    "BASH_ENV",
    "ENV",
    "LD_PRELOAD",
    "DYLD_INSERT_LIBRARIES",
];

/// Builtins / programs whose ARGUMENTS are assignments, so a quoted spelling
/// (`env 'GIT_PAGER=…'`, `export "GIT_PAGER=…"`, `env G"IT_PAGER=…"`) still
/// assigns. In command-prefix position bash requires the name and `=` to be
/// unquoted, so a quoted prefix word is a command name, not an assignment.
const ASSIGNING_COMMANDS: &[&str] = &["env", "export", "declare", "typeset", "local", "readonly"];

/// Commands that EXPORT a bare name (`export GIT_PAGER`), handing the
/// variable's current value — assigned however — to every later program.
const EXPORTING_COMMANDS: &[&str] = &["export", "declare", "typeset", "readonly"];

fn is_watched_env_name(name: &str) -> bool {
    EXEC_ENV_VARS.contains(&name)
        || LOAD_ENV_VARS.contains(&name)
        || name == "GIT_CONFIG_PARAMETERS"
        || name.starts_with("GIT_CONFIG_KEY_")
        || name.starts_with("GIT_CONFIG_VALUE_")
}

/// True when `v` is text the shell still expands at run time (a parameter,
/// command or arithmetic expansion) — its value is not what is written.
fn expands_at_run_time(v: &str) -> bool {
    v.contains('$') || v.contains('`')
}

/// Judge every command-valued environment variable assigned anywhere in `seg`
/// (one segment of `line`).
///
/// 675eb991: leading `NAME=value` words are skipped on the way to the command
/// word, so `GIT_PAGER='rm -rf ~/x' git log` was ALLOW while its twin
/// `git -c core.pager='rm -rf ~/x' log` was a Deny. The scan is
/// position-independent on purpose: the same assignment reaches the program
/// as a command prefix, as an `env NAME=…` operand, through `export NAME=…`
/// earlier on the line, or behind `sudo`/`nohup`. A word in operand position
/// that merely LOOKS like an assignment (`echo PAGER=less`) is judged too —
/// that can only add a verdict, never remove one.
///
/// * A command-valued variable ([`EXEC_ENV_VARS`]): the value is judged as the
///   shell line it is ([`analyze_shell_payload`]); a value that is only known
///   at run time (`"$X"`) is Ask through that same function. A value whose
///   quoting this module cannot resolve to one literal is Ask.
/// * A code/config-loading variable ([`LOAD_ENV_VARS`]): Ask unless the value
///   is empty or `/dev/null`. `ENV` is Ask only for a path-shaped or run-time
///   value: `ENV=production npm start` is overwhelmingly a plain setting, and
///   sh reads `ENV` only when interactive.
/// * `GIT_CONFIG_PARAMETERS`: git's serialised `-c` list (`'k=v' 'k2=v2'`),
///   each entry judged exactly as `git -c k=v` is; an unparseable list, or one
///   that still expands at run time, is Ask.
/// * `GIT_CONFIG_KEY_<n>`/`GIT_CONFIG_VALUE_<n>`: paired by `<n>` within the
///   segment and judged as `git -c key=value`. A key that is not a literal is
///   Ask; a command-valued key whose value is not a literal on this segment is
///   Ask; a value whose key is not on this segment is Ask (the key comes from
///   the environment and may be command-valued).
/// * Quoted assignment words (`env 'GIT_PAGER=…'`, `export "GIT_PAGER=…"`) are
///   read as assignments once an [`ASSIGNING_COMMANDS`] word precedes them.
/// * A watched variable assigned WITHOUT `=` — `printf -v NAME`, `read NAME`,
///   `${NAME:=…}` / `${NAME=…}` — or exported by bare name (`export NAME`)
///   on a line that never assigns it with a literal `NAME=…`: its value is not
///   on the command line, so Ask.
pub(super) fn env_exec_var_verdict(seg: &str, line: &str, depth: usize, ctx: &Ctx<'_>) -> Decision {
    let mut acc = VerdictAcc::default();
    let mut config_keys: Vec<(String, Option<String>)> = Vec::new();
    let mut config_values: Vec<(String, Option<String>)> = Vec::new();
    let words = quote_aware_words(seg);
    let mut assigning_context = false;
    let mut exporting_context = false;
    for (i, word) in words.iter().enumerate() {
        let plain = unquote_literal_value(word);
        let head = plain.as_deref().unwrap_or(word.as_str());
        let head_base = head.rsplit('/').next().unwrap_or(head);
        if let Some(d) = non_equals_assignment(&words, i) {
            if let Some(deny) = acc.record(d) {
                return deny;
            }
        }
        if exporting_context && is_watched_env_name(head) && !line_assigns_literally(line, head) {
            if let Some(deny) = acc.record(Decision::ask(format!(
                "`{head}` is exported here, and its value is not assigned on this command line \
— a program may run or load it, and blastguard cannot see what it is"
            ))) {
                return deny;
            }
        }
        let parsed: Option<(String, Option<String>, String)> =
            if let Some((name, raw_value)) = env_assignment(word) {
                Some((
                    name.to_string(),
                    unquote_literal_value(raw_value),
                    raw_value.to_string(),
                ))
            } else if assigning_context {
                plain
                    .as_deref()
                    .and_then(env_assignment)
                    .map(|(n, v)| (n.to_string(), Some(v.to_string()), v.to_string()))
            } else {
                None
            };
        if ASSIGNING_COMMANDS.contains(&head_base) {
            assigning_context = true;
        }
        if EXPORTING_COMMANDS.contains(&head_base) {
            exporting_context = true;
        }
        let Some((name, value, raw_value)) = parsed else {
            continue;
        };
        let name = name.as_str();
        let verdict = if EXEC_ENV_VARS.contains(&name) {
            match value {
                Some(v) if v.trim().is_empty() => Decision::Allow,
                Some(v) => {
                    // less's input preprocessor: a leading `|` (or `||`) marks
                    // the value as a command whose output less reads.
                    let payload = if name.starts_with("LESS") {
                        v.trim_start_matches('|').to_string()
                    } else {
                        v
                    };
                    analyze_shell_payload(&payload, depth, ctx)
                }
                None => Decision::ask(format!(
                    "`{name}` is a command line a program runs, and its value `{raw_value}` \
cannot be resolved to one literal — blastguard cannot see what would execute"
                )),
            }
        } else if LOAD_ENV_VARS.contains(&name) {
            match value.as_deref().map(str::trim) {
                Some("" | "/dev/null") => Decision::Allow,
                Some(v) if name == "ENV" && !v.contains('/') && !expands_at_run_time(v) => {
                    Decision::Allow
                }
                _ => Decision::ask(format!(
                    "`{name}={raw_value}` makes a program load code or configuration from a \
file — what would run is in that file, not on the command line, so blastguard cannot see it"
                )),
            }
        } else if name == "GIT_CONFIG_PARAMETERS" {
            git_config_parameters_verdict(value.as_deref(), depth, ctx)
        } else if let Some(n) = name.strip_prefix("GIT_CONFIG_KEY_") {
            config_keys.push((n.to_string(), value));
            continue;
        } else if let Some(n) = name.strip_prefix("GIT_CONFIG_VALUE_") {
            config_values.push((n.to_string(), value));
            continue;
        } else {
            continue;
        };
        if let Some(deny) = acc.record(verdict) {
            return deny;
        }
    }
    for (n, key) in &config_keys {
        let key = match key {
            Some(k) if !expands_at_run_time(k) => k,
            _ => {
                if let Some(deny) = acc.record(Decision::ask(format!(
                    "`GIT_CONFIG_KEY_{n}` names a git config key blastguard cannot resolve to \
one literal — it cannot tell whether git would run its value as a command"
                ))) {
                    return deny;
                }
                continue;
            }
        };
        let value = config_values
            .iter()
            .rev()
            .find(|(m, _)| m == n)
            .and_then(|(_, v)| v.clone())
            .filter(|v| !expands_at_run_time(v));
        let verdict = match value {
            Some(v) => judge_git_config(&format!("{key}={v}"), false, depth, ctx),
            // Value unresolvable, or not on this segment at all (it comes from
            // the environment): the `--config-env` case, judged the same way —
            // but reported in this variable's own terms.
            None => match judge_git_config(&format!("{key}="), true, depth, ctx) {
                Decision::Allow => Decision::Allow,
                Decision::Deny(r) => Decision::Deny(r),
                _ => Decision::ask(format!(
                    "`GIT_CONFIG_KEY_{n}={key}` sets a git config key whose value git may run, \
and `GIT_CONFIG_VALUE_{n}` is not a literal on this command line — blastguard cannot see what \
would execute"
                )),
            },
        };
        if let Some(deny) = acc.record(verdict) {
            return deny;
        }
    }
    for (n, _) in &config_values {
        if config_keys.iter().any(|(m, _)| m == n) {
            continue;
        }
        if let Some(deny) = acc.record(Decision::ask(format!(
            "`GIT_CONFIG_VALUE_{n}` is set without `GIT_CONFIG_KEY_{n}` on this command line — \
the key comes from the environment, and git may run this value as a command"
        ))) {
            return deny;
        }
    }
    acc.finish()
}

/// `(NAME, raw value)` when `word` is an unquoted `NAME=…`/`NAME+=…`
/// assignment word.
fn env_assignment(word: &str) -> Option<(&str, &str)> {
    let eq = word.find('=')?;
    let name = word[..eq].strip_suffix('+').unwrap_or(&word[..eq]);
    if !is_assignment(&format!("{name}=")) {
        return None;
    }
    Some((name, &word[eq + 1..]))
}

/// True when `line` contains a literal `NAME=`/`NAME+=` assignment word (whose
/// value [`env_exec_var_verdict`] judges where it stands).
fn line_assigns_literally(line: &str, name: &str) -> bool {
    quote_aware_words(line).iter().any(|w| {
        let plain = unquote_literal_value(w).unwrap_or_else(|| w.clone());
        env_assignment(&plain).is_some_and(|(n, _)| n == name)
    })
}

/// An Ask when `words[i]` assigns a watched variable WITHOUT a `NAME=` word:
/// `printf -v NAME …` / `printf -vNAME …`, `read … NAME`, or a `${NAME:=…}` /
/// `${NAME=…}` default-assignment expansion anywhere in the word.
fn non_equals_assignment(words: &[String], i: usize) -> Option<Decision> {
    let word = words[i].as_str();
    let ask = |name: &str, how: &str| {
        Some(Decision::ask(format!(
            "`{name}` is assigned by {how}, so its value is not on the command line — a program \
may run or load it, and blastguard cannot see what it is"
        )))
    };
    let mut rest = word;
    while let Some(pos) = rest.find("${") {
        let body = &rest[pos + 2..];
        let end = body
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .unwrap_or(body.len());
        let (name, after) = body.split_at(end);
        if is_watched_env_name(name) && (after.starts_with(":=") || after.starts_with('=')) {
            return ask(name, "a `${NAME:=…}` expansion");
        }
        rest = body;
    }
    let base_at = |j: usize| -> String {
        let w = &words[j];
        let p = unquote_literal_value(w).unwrap_or_else(|| w.clone());
        p.rsplit('/').next().unwrap_or(&p).to_string()
    };
    let plain = unquote_literal_value(word).unwrap_or_else(|| word.to_string());
    // `printf -v NAME` / `printf -vNAME`.
    if (0..i).any(|j| base_at(j) == "printf") {
        let named = if i > 0 && words[i - 1] == "-v" {
            Some(plain.as_str())
        } else {
            plain.strip_prefix("-v").filter(|n| !n.is_empty())
        };
        if let Some(n) = named.filter(|n| is_watched_env_name(n)) {
            return ask(n, "`printf -v`");
        }
    }
    // `read [opts] NAME…`: any later operand that is a watched name.
    if (0..i).any(|j| base_at(j) == "read") && is_watched_env_name(&plain) {
        return ask(&plain, "`read`");
    }
    None
}

/// Judge git's serialised `-c` list as carried by `GIT_CONFIG_PARAMETERS`.
/// `list` is the value already resolved to a literal, or `None` when it could
/// not be.
fn git_config_parameters_verdict(list: Option<&str>, depth: usize, ctx: &Ctx<'_>) -> Decision {
    let unparseable = || {
        Decision::ask(
            "`GIT_CONFIG_PARAMETERS` sets git config for this command, and blastguard cannot \
parse its value into literal `key=value` entries — it cannot tell whether git would run one as a \
command",
        )
    };
    let Some(list) = list else {
        return unparseable();
    };
    if expands_at_run_time(list) {
        return unparseable();
    }
    let mut acc = VerdictAcc::default();
    for entry in quote_aware_words(list) {
        let Some(assignment) = unquote_literal_value(&entry) else {
            return unparseable();
        };
        if let Some(deny) = acc.record(judge_git_config(&assignment, false, depth, ctx)) {
            return deny;
        }
    }
    acc.finish()
}

// ---------------------------------------------------------------- env -S ----

/// Command lines handed to `env -S` / `--split-string`, which splits its one
/// operand into argv and execs it.
///
/// Walks env's own options only: assignments and flags, with `-u NAME`,
/// `-C DIR` and `-P PATH` consuming their value; the first plain word is the
/// command and ends env's options. Bundled short flags are read getopt-style:
/// once a value-taking letter appears the rest of the token is its value.
pub(super) fn env_split_string_payloads(rest: &[&str]) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < rest.len() {
        let t = rest[i];
        if t.is_empty() || is_assignment(t) {
            i += 1;
            continue;
        }
        match t {
            "-S" | "--split-string" => {
                out.extend(payloads_after(rest, i));
                break;
            }
            "-u" | "--unset" | "-C" | "--chdir" | "-P" => {
                i += 2;
                continue;
            }
            "--" => break,
            _ => {}
        }
        if let Some(v) = t.strip_prefix("--split-string=") {
            out.extend(inline_command_payloads(rest, i, v));
            break;
        }
        if t.starts_with("--") {
            i += 1;
            continue;
        }
        if is_short_flag(t) && t.len() > 1 {
            for (k, c) in t[1..].char_indices() {
                if matches!(c, 'u' | 'C' | 'P') {
                    break;
                }
                if c == 'S' {
                    let inline = &t[1 + k + 1..];
                    if inline.is_empty() {
                        out.extend(payloads_after(rest, i));
                    } else {
                        out.extend(inline_command_payloads(rest, i, inline));
                    }
                    return out;
                }
            }
            i += 1;
            continue;
        }
        break;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn awk_program_extraction_skips_option_values() {
        assert_eq!(
            awk_programs(&["-F", "'|'", "'{print", "$1}'"]),
            vec!["{print $1}"]
        );
        assert_eq!(awk_programs(&["-F'|'", "'{print}'", "f"]), vec!["{print}"]);
        assert!(awk_programs(&["-f", "prog.awk", "data"]).is_empty());
    }

    #[test]
    fn awk_regex_pipe_is_not_a_shell_site() {
        let lex = lex_awk("/foo|bar/ {print $1 || $2}");
        assert_eq!(scan_awk_shell_sites(&lex).sites, 0);
    }

    #[test]
    fn awk_literal_and_computed_sites() {
        let lit = scan_awk_shell_sites(&lex_awk(r#"BEGIN{system("date")}"#));
        assert_eq!((lit.sites, lit.computed), (1, None));
        let comp = scan_awk_shell_sites(&lex_awk(r#"{system("rm " $1)}"#));
        assert!(comp.computed.is_some());
        let gl = scan_awk_shell_sites(&lex_awk(r#"BEGIN{while(("ls" | getline l) > 0) print l}"#));
        assert_eq!((gl.sites, gl.computed), (1, None));
        let sh = scan_awk_shell_sites(&lex_awk(r#"{print $1 | "sh"}"#));
        assert!(sh.computed.is_some());
        let sort = scan_awk_shell_sites(&lex_awk(r#"{print $1 | "sort"}"#));
        assert_eq!((sort.sites, sort.computed), (1, None));
    }

    #[test]
    fn env_split_string_extraction() {
        assert!(env_split_string_payloads(&["FOO=1", "ls"]).is_empty());
        assert!(env_split_string_payloads(&["-u", "S", "ls"]).is_empty());
        assert!(env_split_string_payloads(&["-S", "\"rm", "-rf", "~/x\""])
            .iter()
            .any(|p| p == "rm -rf ~/x"));
        assert!(env_split_string_payloads(&["-iS'rm", "-rf", "~/x'"])
            .iter()
            .any(|p| p == "rm -rf ~/x"));
    }
}
