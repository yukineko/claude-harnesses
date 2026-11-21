//! Relinking commands a segment runs that are not its own command word:
//! the commands of a `$( )` / `` ` ` `` / `<( )` in its words, and the shell
//! body of an interpreter escape it runs (`bash -c 'ln …'`,
//! `vim -c '!ln …'`). Backlog 00c6c2b8: `echo "$(ln -s /etc/x d)"; echo y >
//! d/hosts` was Allow because the segment's word scan ([`super::parse`])
//! only reads the segment's own command word, so the `ln` was never a relink.
//!
//! The AST evaluator ([`ast::analyze`]) lists every simple command that
//! runs, substitution bodies included; this module hands each one to
//! [`super::parse`] as a command of its own, so a relinker found there is
//! modelled exactly like a top-level one. An escape body is parsed and read
//! the same way, recursively ([`MAX_DEPTH`]).
//!
//! Which verbs a command names is read only from the AST's values (`l\n`
//! is `ln`), never from the text: the one text test, [`may_run_nested`],
//! only skips segments that hold no substitution and no escape-capable word.
//!
//! Undetermined (CLAUDE.md §3): when the AST cannot list the commands of a
//! segment that holds a substitution or an escape-capable word, an escape's
//! body cannot be determined (`bash -c "$X"`, an undetermined command word, a
//! vim ex command that is neither a plain `:!` escape nor a known inert
//! command, a wrapper that may run an interpreter), or the depth cap is hit,
//! the segment
//! is reported [`Nested::undetermined`]; the caller treats it as an
//! unmodelled relinker (an `Ask`), never as relinking nothing.

use super::super::ast::{self, Analysis, CwdCandidates, ProgramSource, SimpleCommand, Value};
use super::super::{normalized_command, CwdState};
use super::{is_relink_word, plain_words};

/// Escape bodies inside escape bodies are followed this deep; deeper is
/// undetermined.
const MAX_DEPTH: usize = 4;

/// One command found by [`collect`].
pub(super) struct Cmd {
    /// The command as one line for [`super::parse`].
    pub(super) text: String,
    /// `text` with its substitution words and escape body replaced by inert
    /// placeholders (see [`Nested::outer_reduced`]): its own relinkers are in
    /// [`Nested::commands`] too.
    pub(super) reduced: String,
    pub(super) cwd: CwdState,
}

/// What [`collect`] found in one segment.
#[derive(Default)]
pub(super) struct Nested {
    /// Commands that run before the segment's own command does (substitution
    /// bodies, and — when asked for — escape bodies).
    pub(super) commands: Vec<Cmd>,
    /// The segment's own command with every word this module reads itself
    /// (a word holding a substitution, an escape body) replaced by an inert
    /// placeholder; `None` when the segment is not one simple command. When
    /// the word scan reads the segment as an unmodelled relinker only because
    /// of such words, this reduced text parses as inert: the relinkers in
    /// them are modelled through [`Nested::commands`].
    pub(super) outer_reduced: Option<String>,
    /// Some command this segment runs could not be listed or read.
    pub(super) undetermined: bool,
}

/// Whether `text` may run a command other than its segments' own command
/// words: it holds a substitution opener (`$(`, backquote, `<(`, `>(`), or
/// a word that may name an escape-capable program (a shell, a vi-family
/// editor, a wrapper, or a word with an expansion that may name any of
/// them). Every command this module lists needs one of these in the text,
/// so a `false` here means [`collect`] would find nothing: it is a sound
/// over-approximation that only saves building an AST, never a reading of
/// which verbs the text names (the AST reads those, `l\n` included).
pub(super) fn may_run_nested(text: &str) -> bool {
    has_subst(text) || plain_words(text).into_iter().any(escape_capable_word)
}

/// A word that may be the command word of an escape [`escapes`] reads, or
/// of a wrapper that may run one.
fn escape_capable_word(word: &str) -> bool {
    if word.contains('$') {
        return true;
    }
    let name = normalized_command(word);
    is_shell(&name)
        || is_vi(&name)
        || WRAPPERS.split_whitespace().any(|w| w == name)
        || is_relink_word(word)
}

/// Commands that run another command named in their operands (as
/// `ast::prog::WRAPPERS`).
const WRAPPERS: &str = "env sudo doas su xargs exec command builtin nohup nice ionice \
    timeout time stdbuf chrt taskset setsid caffeinate arch unbuffer flock";

/// The commands segment `command` (run in `cwd`) runs before its own
/// command word does. `with_escapes`: also read the escape body of the
/// segment's own command (false for the segment holding the write itself,
/// whose redirections are opened before its command runs).
pub(super) fn collect(command: &str, cwd: &CwdState, with_escapes: bool) -> Nested {
    let mut out = Nested::default();
    if !may_run_nested(command) {
        return out;
    }
    let script = match ast::analyze(command) {
        Analysis::Known(script) => script,
        Analysis::Undetermined(_) => {
            // The commands of its substitutions / escapes cannot be listed.
            // A segment with neither (only a plain relink word, e.g. a loop
            // fragment `do ln -s … d`) is read by the word scan alone.
            out.undetermined = has_subst(command)
                || plain_words(command)
                    .into_iter()
                    .any(|w| escape_capable_word(w) && !is_relink_word(w));
            return out;
        }
    };
    let cmds = &script.commands;
    let nested_in_other = |c: &SimpleCommand| {
        // The container is a command of its own (`echo "$(…)"`) or an
        // assignment-only command (`X=$(…)`); a compound command's
        // redirection carrier (no argv, no assignment) is not.
        cmds.iter().any(|o| {
            (!o.argv.is_empty() || !o.assignments.is_empty())
                && o.span != c.span
                && o.span.start <= c.span.start
                && c.span.end <= o.span.end
        })
    };
    let mut top: Vec<&SimpleCommand> = Vec::new();
    for c in cmds.iter().filter(|c| !c.argv.is_empty()) {
        if nested_in_other(c) {
            out.commands.push(Cmd {
                text: render(c, false),
                reduced: reduced(c, false),
                cwd: cwd_of(c, cwd),
            });
            escapes(c, cwd, 0, &mut out);
        } else {
            top.push(c);
            if with_escapes {
                escapes(c, cwd, 0, &mut out);
            }
        }
    }
    if let [outer] = top.as_slice() {
        out.outer_reduced = Some(reduced(outer, false));
    }
    out
}

/// The cwd `c` runs in, given the segment runs in `seg`.
fn cwd_of(c: &SimpleCommand, seg: &CwdState) -> CwdState {
    match (&c.cwd_candidates, &c.cwd) {
        (CwdCandidates::One, Value::Literal(p)) => CwdState::Known(p.clone()),
        // One cwd the text does not name: the segment's own.
        (CwdCandidates::One, Value::Undetermined(_)) => seg.clone(),
        _ => CwdState::Unknown,
    }
}

/// `c` as one command line for [`super::parse`]. A literal word is quoted as
/// needed; an undetermined one keeps its spelling (`spelled`), or — for a
/// command from an escape body, whose spelling the line-level resolver does
/// not know — becomes an unresolvable placeholder, so it stays undetermined.
fn render(c: &SimpleCommand, from_body: bool) -> String {
    c.argv
        .iter()
        .map(|w| match &w.value {
            Value::Literal(v) => quote(v),
            Value::Undetermined(_) if from_body => "\"$__blastguard_undetermined\"".to_string(),
            Value::Undetermined(_) => w.raw.clone(),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// [`render`], with every word that holds a substitution or is an escape
/// body replaced by an inert placeholder.
fn reduced(c: &SimpleCommand, from_body: bool) -> String {
    let bodies = escape_body_words(c);
    c.argv
        .iter()
        .enumerate()
        .map(|(i, w)| {
            if bodies.contains(&i) {
                "__blastguard_escape_body".to_string()
            } else if w.quoting != ast::Quoting::Single && has_subst(&w.raw) {
                "\"$__blastguard_substitution\"".to_string()
            } else {
                match &w.value {
                    Value::Literal(v) => quote(v),
                    Value::Undetermined(_) if from_body => {
                        "\"$__blastguard_undetermined\"".to_string()
                    }
                    Value::Undetermined(_) => w.raw.clone(),
                }
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn has_subst(s: &str) -> bool {
    s.contains("$(") || s.contains('`') || s.contains("<(") || s.contains(">(")
}

fn quote(v: &str) -> String {
    if !v.is_empty()
        && v.chars()
            .all(|c| c.is_ascii_alphanumeric() || "_./,:+@%^-=".contains(c))
    {
        v.to_string()
    } else {
        format!("'{}'", v.replace('\'', "'\\''"))
    }
}

fn basename(s: &str) -> &str {
    s.rsplit('/').next().unwrap_or(s)
}

/// The basename of `c`'s command word (`""` for an empty command, which runs
/// nothing). `Err` when the word is undetermined: it may name any program,
/// an escape included.
fn literal_head(c: &SimpleCommand) -> Result<&str, HeadUndetermined> {
    match c.argv.first().map(|w| &w.value) {
        None => Ok(""),
        Some(Value::Literal(s)) => Ok(basename(s)),
        Some(Value::Undetermined(_)) => Err(HeadUndetermined),
    }
}

/// [`literal_head`]: the command word is not determined by the text.
struct HeadUndetermined;

fn is_shell(base: &str) -> bool {
    matches!(base, "bash" | "sh" | "zsh" | "dash" | "ksh")
}

/// vi-family editors whose `-c` / `--cmd` / `+` arguments are ex commands.
fn is_vi(base: &str) -> bool {
    matches!(
        base,
        "vim" | "vi" | "nvim" | "ex" | "view" | "vimdiff" | "gvim" | "mvim" | "nvi" | "elvis"
    )
}

/// The argv indices of `c` whose words are ex commands, with their ex
/// command (a `+{cmd}` word without its `+`).
fn ex_args(c: &SimpleCommand) -> Vec<(usize, Value)> {
    let mut out = Vec::new();
    let mut i = 1;
    while let Some(w) = c.argv.get(i) {
        match &w.value {
            Value::Literal(s) if s == "-c" || s == "--cmd" => {
                if let Some(v) = c.argv.get(i + 1) {
                    out.push((i + 1, v.value.clone()));
                }
                i += 2;
                continue;
            }
            Value::Literal(s) if s.len() > 1 && s.starts_with('+') => {
                out.push((i, Value::Literal(s[1..].to_string())))
            }
            // An option word we cannot read may be `-c`.
            Value::Undetermined(_) => out.push((i, w.value.clone())),
            Value::Literal(_) => {}
        }
        i += 1;
    }
    out
}

/// The argv indices of `c` that are escape bodies this module reads.
fn escape_body_words(c: &SimpleCommand) -> Vec<usize> {
    // An undetermined command word: no word is read as an escape body here,
    // so the reduced text keeps every word (the word scan's reading stands).
    let Ok(base) = literal_head(c) else {
        return Vec::new();
    };
    if is_shell(base) {
        if let Some(ProgramSource::InlineLiteral(body)) = ast::program_source(c) {
            return c
                .argv
                .iter()
                .enumerate()
                .skip(1)
                .filter(|(_, w)| w.value == Value::Literal(body.clone()))
                .map(|(i, _)| i)
                .collect();
        }
        return Vec::new();
    }
    if is_vi(base) {
        return ex_args(c).into_iter().map(|(i, _)| i).collect();
    }
    Vec::new()
}

/// What one vi ex command (`-c` / `+` argument) runs.
enum Ex {
    /// Runs no shell command that could relink anything.
    Inert,
    /// `:!body` (modifiers such as `silent` stripped): the shell runs `body`.
    Shell(String),
    Undetermined,
}

fn ex_command(arg: &str) -> Ex {
    let mut s = arg.trim_start_matches(|c: char| c == ':' || c.is_whitespace());
    // `:sil[ent][!] {cmd}` runs {cmd} quietly.
    loop {
        let word_end = s
            .find(|c: char| !c.is_ascii_alphabetic())
            .unwrap_or(s.len());
        let word = &s[..word_end];
        if word.len() >= 3 && "silent".starts_with(word) {
            let rest = s[word_end..].strip_prefix('!').unwrap_or(&s[word_end..]);
            s = rest.trim_start_matches(|c: char| c == ':' || c.is_whitespace());
            continue;
        }
        break;
    }
    if let Some(body) = s.strip_prefix('!') {
        // vim expands `%`, `#` and `!` in the body before the shell sees it.
        if body.contains(['%', '#', '!']) {
            return Ex::Undetermined;
        }
        return Ex::Shell(body.to_string());
    }
    // Only ex commands known to run nothing are inert; any other (`:r !ln`,
    // `:call system(…)`, `:normal`, a `|` chain, …) is not read here.
    if ex_known_inert(s) {
        Ex::Inert
    } else {
        Ex::Undetermined
    }
}

/// An ex command known to run no shell command: quitting / writing without
/// a `!` filter, an option or display setting, a line number or a plain
/// search. Anything else is undetermined.
fn ex_known_inert(s: &str) -> bool {
    let s = s.trim();
    if s.contains(['|', '!', '`']) {
        return false;
    }
    if s.is_empty() || s.chars().all(|c| c.is_ascii_digit()) || s.starts_with(['/', '?']) {
        return true;
    }
    let (word, rest) = s.split_once(char::is_whitespace).unwrap_or((s, ""));
    let no_arg = [
        "q",
        "qa",
        "qall",
        "quit",
        "wq",
        "wqa",
        "wqall",
        "x",
        "xa",
        "xit",
        "w",
        "write",
        "noh",
        "nohlsearch",
    ];
    let with_arg = [
        "set",
        "se",
        "setl",
        "setlocal",
        "syntax",
        "syn",
        "filetype",
        "colorscheme",
        "colo",
    ];
    (no_arg.contains(&word) && rest.trim().is_empty())
        || (with_arg.contains(&word) && !rest.contains(['=', '&', '$']))
}

/// Add the commands of `c`'s escape body (run in `seg`'s cwd at `depth`).
fn escapes(c: &SimpleCommand, seg: &CwdState, depth: usize, out: &mut Nested) {
    let base = match literal_head(c) {
        Ok(b) => b,
        // `$X -c 'ln …'` may be an escape whose body cannot be read.
        Err(HeadUndetermined) => {
            out.undetermined = true;
            return;
        }
    };
    let cwd = cwd_of(c, seg);
    if is_shell(base) {
        match ast::program_source(c) {
            Some(ProgramSource::InlineLiteral(body)) => body_commands(&body, &cwd, depth, out),
            Some(ProgramSource::Undetermined(_)) => out.undetermined = true,
            // A script file or stdin program: not text on this line.
            _ => {}
        }
        return;
    }
    if is_vi(base) {
        for (_, v) in ex_args(c) {
            match v {
                Value::Literal(arg) => match ex_command(&arg) {
                    Ex::Inert => {}
                    Ex::Shell(body) => body_commands(&body, &cwd, depth, out),
                    Ex::Undetermined => out.undetermined = true,
                },
                Value::Undetermined(_) => out.undetermined = true,
            }
        }
        return;
    }
    // A wrapper that may run an interpreter (`sudo bash -c …`).
    if matches!(ast::program_source(c), Some(ProgramSource::Undetermined(_))) {
        out.undetermined = true;
    }
}

/// Every command of the shell program `body`, run in `cwd`.
fn body_commands(body: &str, cwd: &CwdState, depth: usize, out: &mut Nested) {
    if depth >= MAX_DEPTH {
        out.undetermined = true;
        return;
    }
    let Analysis::Known(script) = ast::analyze(body) else {
        out.undetermined = true;
        return;
    };
    for c in script.commands.iter().filter(|c| !c.argv.is_empty()) {
        out.commands.push(Cmd {
            text: render(c, true),
            reduced: reduced(c, true),
            cwd: cwd_of(c, cwd),
        });
        escapes(c, cwd, depth + 1, out);
    }
}
