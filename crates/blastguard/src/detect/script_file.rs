//! Running a TEXT script FILE is judged by the file's content (user ruling R3,
//! 2026-10-14; resumed as backlog ee0e0b85).
//!
//! Before this, every call was judged alone: a `Write` of `rm -rf ~` into
//! `x.sh` destroys nothing, and `bash x.sh` names nothing destructive, so the
//! pair reached Allow. Here the script the command runs is located and READ at
//! PreToolUse time — a relative path against the cwd the segment runs in — and
//! its text is judged the way the same text would be judged inline by that
//! interpreter.
//!
//! Shapes located ([`locate`]):
//!   * a POSIX shell with a script operand: `bash f.sh`, `sh -e f.sh x`,
//!     `zsh f` (a `-c`/`-s` invocation is not a script file and is left to the
//!     `-c` and stdin arms);
//!   * `source f.sh` / `. f.sh`;
//!   * `python3 f.py`, `node f.js`, `perl f.pl`, `ruby f.rb` with a script
//!     operand (an inline-eval flag, `-m`, or `-` is not one);
//!   * direct execution of a path (`./f.sh`, `sub/g.sh`, `/abs/f`): a text file
//!     is run by its shebang interpreter, or by `sh` when it has none. A binary
//!     (ELF / Mach-O magic) is OUT OF SCOPE and keeps its previous verdict.
//!
//! Verdicts ([`script_file_verdict`]):
//!   * the content judges Deny or Ask -> Ask carrying [`RECOMMEND_NO`] (ruling:
//!     content read from a file is an Ask recommending not running, not a hard
//!     Deny; the inline spelling is untouched and stays Deny);
//!   * the content cannot be judged -> Ask carrying [`RECOMMEND_NO`], NEVER
//!     Allow (CLAUDE.md §3): the file is missing, unreadable, not UTF-8, too
//!     large, its path is not a literal, its shebang names an interpreter
//!     blastguard does not read, or the script runs ANOTHER script (ruling:
//!     undeterminable even when the inner script is harmless);
//!   * the content judges Allow -> Allow.
//!
//! Not covered (left as before, stated so it is not mistaken for coverage):
//! interpreters other than the four above (`deno run`, `bun run`, `php`, …),
//! whose first operand is often a subcommand rather than a file; and what a
//! script does with its ARGUMENTS (`rm -rf "$1"` is an undeterminable target
//! and Asks, but blastguard does not substitute the call's arguments in).

use super::{
    command_candidates, depth_exhausted, detect_bash, interpreter_code_verdict,
    interpreter_inline_eval_pos, interpreter_value_flags, is_assignment, is_bare_redirect_op,
    is_redirect_token, is_shell, is_short_flag, normalized_command, recommend_against,
    split_segments, strip_version_suffix, unquote_literal_value, Ctx, COMPOUND_PREFIX_WORDS,
    MAX_SHELL_DEPTH,
};
use crate::model::Decision;
use std::io::Read as _;
use std::path::{Path, PathBuf};

/// Scripts larger than this are not read: judging them would not fit a
/// PreToolUse hook, and an unread script is an Ask, not an Allow.
const MAX_SCRIPT_BYTES: u64 = 1024 * 1024;

/// Which language the located script is read as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lang {
    Shell,
    /// One of [`judged_interpreter`]'s names, already version-stripped.
    Interp(&'static str),
}

/// How the script was reached, which decides what a missing file means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Via {
    /// Handed to an interpreter (`bash f`, `. f`, `python3 f`): the interpreter
    /// opens the path, so a missing file is undeterminable content.
    Interpreter(Lang),
    /// Executed as a path (`./f`): the language comes from the file itself.
    Direct,
}

/// A script operand this command runs.
struct Located<'t> {
    /// The command word that runs it, for the reason text.
    runner: String,
    via: Via,
    /// The operand as written (still quoted).
    raw: &'t str,
}

/// Interpreters whose script operand is located and whose program text
/// [`interpreter_code_verdict`] reads. Others are not located (see the module
/// doc's "Not covered").
fn judged_interpreter(cmd: &str) -> Option<&'static str> {
    match strip_version_suffix(cmd) {
        "python" => Some("python"),
        "python2" => Some("python2"),
        "python3" => Some("python3"),
        "node" => Some("node"),
        "nodejs" => Some("nodejs"),
        "perl" => Some("perl"),
        "ruby" => Some("ruby"),
        _ => None,
    }
}

/// The first operand of a shell invocation that is a script file, or `None`
/// when the shell reads its program from `-c` or stdin.
fn shell_script_operand<'t>(rest: &[&'t str]) -> Option<&'t str> {
    let mut i = 0;
    while i < rest.len() {
        let tok = rest[i];
        if is_bare_redirect_op(tok) {
            i += 2;
            continue;
        }
        if is_redirect_token(tok) {
            i += 1;
            continue;
        }
        if tok == "--" {
            return rest.get(i + 1).copied();
        }
        if matches!(tok, "-o" | "+o" | "-O" | "+O" | "--rcfile" | "--init-file") {
            i += 2;
            continue;
        }
        if tok.starts_with("--") {
            i += 1;
            continue;
        }
        if (tok.starts_with('-') || tok.starts_with('+')) && tok.len() > 1 {
            // `-c` supplies the program inline, `-s` reads it from stdin: no
            // script file either way.
            if tok[1..].contains('c') || tok[1..].contains('s') {
                return None;
            }
            i += 1;
            continue;
        }
        if tok == "-" {
            return None;
        }
        return Some(tok);
    }
    None
}

/// The script operand of a [`judged_interpreter`] invocation, or `None` for an
/// inline-eval flag, `-m`, `-` (stdin) or no operand at all.
fn interpreter_script_operand<'t>(cmd: &str, rest: &[&'t str]) -> Option<&'t str> {
    if interpreter_inline_eval_pos(cmd, rest).is_some() {
        return None;
    }
    let value_flags = interpreter_value_flags(cmd);
    let python = strip_version_suffix(cmd).starts_with("python");
    let mut i = 0;
    while i < rest.len() {
        let tok = rest[i];
        if is_bare_redirect_op(tok) {
            i += 2;
            continue;
        }
        if is_redirect_token(tok) {
            i += 1;
            continue;
        }
        if tok == "-" {
            return None;
        }
        if tok == "--" {
            return rest.get(i + 1).copied();
        }
        if value_flags.contains(&tok) {
            i += 2;
            continue;
        }
        if tok.starts_with("--") {
            i += 1;
            continue;
        }
        if is_short_flag(tok) {
            if python && tok[1..].contains('m') {
                return None;
            }
            i += 1;
            continue;
        }
        return Some(tok);
    }
    None
}

/// The script the command word at `tokens[idx]` runs, if it runs one.
fn locate<'t>(tokens: &[&'t str], idx: usize) -> Option<Located<'t>> {
    let raw = tokens[idx];
    let cmd = normalized_command(raw);
    let rest = &tokens[idx + 1..];
    if is_shell(&cmd) {
        return shell_script_operand(rest).map(|op| Located {
            runner: cmd.clone(),
            via: Via::Interpreter(Lang::Shell),
            raw: op,
        });
    }
    if matches!(cmd.as_str(), "source" | ".") {
        return rest
            .iter()
            .find(|t| !is_redirect_token(t))
            .map(|op| Located {
                runner: cmd.clone(),
                via: Via::Interpreter(Lang::Shell),
                raw: op,
            });
    }
    if let Some(name) = judged_interpreter(&cmd) {
        return interpreter_script_operand(&cmd, rest).map(|op| Located {
            runner: cmd.clone(),
            via: Via::Interpreter(Lang::Interp(name)),
            raw: op,
        });
    }
    // A word carrying substitution punctuation is not a path being executed:
    // `X=$(cygpath /a/b)` splits into `/a/b)`, and an expansion-valued command
    // word (`"$BIN" …`) is judged by the unresolvable-command-word arms.
    if raw.contains('/') && !raw.starts_with('#') && !raw.contains(['$', '(', ')', '`']) {
        return Some(Located {
            runner: raw.to_string(),
            via: Via::Direct,
            raw,
        });
    }
    None
}

/// What reading the located file produced.
enum ScriptRead {
    /// The path does not exist.
    Missing,
    /// It exists but is not a regular file (a directory, a device).
    NotAFile,
    /// A binary executable (ELF / Mach-O magic): out of scope.
    Binary,
    /// UTF-8 text.
    Text(String),
    /// Could not be read, or is not UTF-8 text, or is too large.
    Undetermined(String),
}

fn has_binary_magic(head: &[u8]) -> bool {
    matches!(
        head.get(..4),
        Some(
            [0x7f, b'E', b'L', b'F']
                | [0xfe, 0xed, 0xfa, 0xce]
                | [0xfe, 0xed, 0xfa, 0xcf]
                | [0xce, 0xfa, 0xed, 0xfe]
                | [0xcf, 0xfa, 0xed, 0xfe]
                | [0xca, 0xfe, 0xba, 0xbe]
        )
    )
}

/// Read `path` with every failure kept distinct from "absent": only `NotFound`
/// is [`ScriptRead::Missing`]; every other error is [`ScriptRead::Undetermined`].
fn read_script(path: &Path) -> ScriptRead {
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return ScriptRead::Missing,
        Err(e) => return ScriptRead::Undetermined(format!("cannot stat it: {e}")),
    };
    if !meta.is_file() {
        return ScriptRead::NotAFile;
    }
    let mut bytes = Vec::new();
    let read = std::fs::File::open(path)
        .and_then(|f| f.take(MAX_SCRIPT_BYTES + 1).read_to_end(&mut bytes));
    if let Err(e) = read {
        return ScriptRead::Undetermined(format!("cannot read it: {e}"));
    }
    // Magic BEFORE size: a large compiled binary is out of scope, not an
    // over-large script.
    if has_binary_magic(&bytes) {
        return ScriptRead::Binary;
    }
    if bytes.len() as u64 > MAX_SCRIPT_BYTES {
        return ScriptRead::Undetermined(format!(
            "it is larger than blastguard reads ({MAX_SCRIPT_BYTES} bytes)"
        ));
    }
    match String::from_utf8(bytes) {
        Ok(text) => ScriptRead::Text(text),
        Err(_) => ScriptRead::Undetermined("it is not UTF-8 text".to_string()),
    }
}

/// The language a directly executed text file runs in: its shebang
/// interpreter, or the shell when it has none. `Err` names a shebang
/// interpreter blastguard does not read.
fn shebang_lang(text: &str) -> Result<Lang, String> {
    let Some(line) = text.lines().next().and_then(|l| l.strip_prefix("#!")) else {
        return Ok(Lang::Shell);
    };
    let mut words = line.split_whitespace();
    let Some(first) = words.next() else {
        return Ok(Lang::Shell);
    };
    let mut interp = Path::new(first)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(first)
        .to_string();
    if interp == "env" {
        match words.find(|w| !w.starts_with('-')) {
            Some(w) => interp = w.to_string(),
            None => return Err(format!("`{line}`")),
        }
    }
    if is_shell(&interp) {
        return Ok(Lang::Shell);
    }
    judged_interpreter(&interp)
        .map(Lang::Interp)
        .ok_or_else(|| format!("`{interp}`"))
}

/// An Ask about a script that cannot be judged.
fn undeterminable(runner: &str, shown: &str, why: &str) -> Decision {
    recommend_against(Decision::ask(format!(
        "`{runner}` runs the script `{shown}`, and {why} — blastguard cannot judge what it would \
do"
    )))
}

/// True when shell `text` itself runs another script (ruling: undeterminable
/// even when the inner script is harmless). Comment lines are skipped.
fn runs_another_script(text: &str, base: Option<&str>) -> Option<String> {
    for seg in split_segments(text) {
        let tokens = command_tokens(&seg);
        if tokens.first().is_none_or(|t| t.starts_with('#')) {
            continue;
        }
        let (positions, _) = script_positions(&tokens);
        for idx in positions {
            let Some(found) = locate(&tokens, idx) else {
                continue;
            };
            if found.via == Via::Direct {
                // A binary on a path (`/bin/echo`) is a program, not a script.
                // A MISSING path whose word carries substitution punctuation
                // (`$(cygpath /x)` splits into `/x)`) is a fragment of an
                // expression, not a program; a missing plain path still counts.
                let read = unquote_literal_value(found.raw)
                    .and_then(|p| crate::reversible::resolve(&p, base))
                    .map(|p| read_script(&p));
                let fragment = found.raw.contains(['$', '(', ')', '`']);
                match read {
                    Some(ScriptRead::Binary | ScriptRead::NotAFile) => continue,
                    Some(ScriptRead::Missing) if fragment => continue,
                    _ => {}
                }
            }
            return Some(seg.trim().to_string());
        }
    }
    None
}

/// The whitespace words of `seg` with leading compound keywords (`then`,
/// `do`, `{`, …) removed, so the word after them is judged as the command.
fn command_tokens(seg: &str) -> Vec<&str> {
    let mut tokens: Vec<&str> = seg.split_whitespace().collect();
    let lead = tokens
        .iter()
        .take_while(|t| COMPOUND_PREFIX_WORDS.contains(t))
        .count();
    tokens.drain(..lead);
    tokens
}

/// The positions that may run a script, and whether there is exactly one
/// (the segment's own command word). `source`/`.` are exec wrappers to
/// [`command_candidates`], which would hand back only the positions BEHIND
/// them — the file operand, judged as a directly executed path by its
/// shebang. But a sourced file is always read by the current shell, so the
/// `source` word itself is the position.
fn script_positions(tokens: &[&str]) -> (Vec<usize>, bool) {
    let first = tokens
        .iter()
        .position(|t| !t.is_empty() && !is_assignment(t));
    if let Some(f) = first {
        if matches!(normalized_command(tokens[f]).as_str(), "source" | ".") {
            return (vec![f], true);
        }
    }
    let candidates = command_candidates(tokens);
    // Several candidates mean an exec wrapper widened the scan to every later
    // position.
    let primary = candidates.len() == 1;
    (candidates, primary)
}

/// The script-file verdict for one segment of a command line: Ask or Allow,
/// never Deny (the ruling makes file content an Ask), and Allow when the
/// segment runs no script file or [`Ctx::script_files`] is off.
pub(super) fn segment_verdict(seg: &str, depth: usize, ctx: &Ctx<'_>) -> Decision {
    if !ctx.script_files {
        return Decision::Allow;
    }
    let tokens = command_tokens(seg);
    let (positions, primary) = script_positions(&tokens);
    for idx in positions {
        match script_file_verdict(&tokens, idx, primary, depth, ctx) {
            None | Some(Decision::Allow) => {}
            Some(other) => return other,
        }
    }
    Decision::Allow
}

/// The verdict for the script run by the command word at `tokens[idx]`, or
/// `None` when that word runs no script file (or a binary, which is out of
/// scope). `primary` is false for a position [`command_candidates`] widened to
/// behind an exec wrapper: there a PATH is as likely a wrapper operand
/// (`flock /tmp/l …`) as a program, so a path at such a position is judged
/// only when it resolves to a readable text file. An interpreter's script
/// operand (`sudo bash x.sh`) is judged at every position.
fn script_file_verdict(
    tokens: &[&str],
    idx: usize,
    primary: bool,
    depth: usize,
    ctx: &Ctx<'_>,
) -> Option<Decision> {
    let found = locate(tokens, idx)?;
    let optional = found.via == Via::Direct && !primary;
    let runner = found.runner.as_str();
    let shown = found.raw;
    let literal = match unquote_literal_value(found.raw) {
        Some(p)
            if !p.is_empty() && !p.contains(['$', '`', '*', '?', '[']) && !p.starts_with('~') =>
        {
            p
        }
        _ if optional => return None,
        _ => {
            return Some(undeterminable(
                runner,
                shown,
                "its path is not a literal blastguard can resolve before it runs",
            ))
        }
    };
    let Some(path): Option<PathBuf> = crate::reversible::resolve(&literal, ctx.raw_base.as_deref())
    else {
        if optional {
            return None;
        }
        return Some(undeterminable(
            runner,
            shown,
            "the directory it is relative to is not known",
        ));
    };
    let text = match read_script(&path) {
        ScriptRead::Text(t) => t,
        ScriptRead::Binary => return None,
        ScriptRead::NotAFile if found.via == Via::Direct => return None,
        // A file that EXISTS but cannot be read still asks, wrapper position
        // or not; only absence is taken as "not a program here".
        ScriptRead::Missing if optional => return None,
        ScriptRead::NotAFile => {
            return Some(undeterminable(runner, shown, "it is not a regular file"))
        }
        ScriptRead::Missing => {
            return Some(undeterminable(
                runner,
                shown,
                "it does not exist at the time of judging",
            ))
        }
        ScriptRead::Undetermined(why) => return Some(undeterminable(runner, shown, &why)),
    };
    let lang = match found.via {
        Via::Interpreter(lang) => lang,
        Via::Direct => match shebang_lang(&text) {
            Ok(lang) => lang,
            Err(interp) => {
                return Some(undeterminable(
                    runner,
                    shown,
                    &format!("its shebang names {interp}, whose language blastguard does not read"),
                ))
            }
        },
    };
    if depth >= MAX_SHELL_DEPTH {
        return Some(recommend_against(depth_exhausted()));
    }
    let content_verdict = match lang {
        Lang::Shell => {
            if let Some(inner) = runs_another_script(&text, ctx.raw_base.as_deref()) {
                return Some(undeterminable(
                    runner,
                    shown,
                    &format!("it runs another script (`{inner}`)"),
                ));
            }
            detect_bash(&text, depth + 1, &ctx.for_script_text(&text))
        }
        Lang::Interp(name) => interpreter_code_verdict(
            &format!("the `{name}` program in `{shown}`"),
            &[text],
            depth + 1,
            ctx,
            false,
        ),
    };
    match content_verdict {
        Decision::Allow => Some(Decision::Allow),
        Decision::Deny(why) | Decision::Ask(why) => {
            // `recommend_against` does not append the marker twice when the
            // content's own Ask already carries it.
            Some(recommend_against(Decision::ask(format!(
                "`{runner}` runs the script `{shown}`, whose content: {why}"
            ))))
        }
    }
}
