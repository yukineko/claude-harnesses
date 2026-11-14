//! The input fds an `exec` opens for the rest of a command line
//! (`exec 3< <(…); bash <&3`, backlog 72872200), and the fds it opens on
//! literal text, followed from statement to statement (`exec 3<<< '…';
//! bash <&3`, backlog e686a85e — `docs/specs/blastguard.md`, "exec が開く fd
//! の文をまたいだ追跡").

use std::collections::HashMap;

use super::super::budget::MAX_SHELL_DEPTH;
use super::super::compound::strip_unmatched_close_parens;
use super::super::heredoc::{
    here_document_layout, here_document_openers, segment_closes_here_document,
};
use super::super::segment::{split_segments_with_separators, SegmentSep};
use super::{
    interpreter_program_source, is_assignment, is_code_interpreter, is_node_head, is_python_head,
    is_shell, node_program_source, normalized_command, parse_redirect, procsub_body,
    python_program_source, shell_program_source, wrapped_command_heads, Ctx, Decision,
    ProgramSource, VerdictAcc,
};

/// What an fd `exec` opens for the rest of the line refers to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ExecFd {
    /// A process substitution (`exec 3< <(…)`): its command text.
    Procsub(String),
    /// A file, or a standard fd duplicated (`exec 3< f`, `exec 3<&0`): read as
    /// `READER < f` is.
    File,
    /// A here-document / here-string (`exec 3<<< '…'`, `exec 3<<'EOF'`). Its
    /// text is judged by [`cross_statement_fd_verdict`] when that pass
    /// resolved every mention of the fd on the line; otherwise it cannot be
    /// read.
    HereText,
    /// A duplicate of an fd above 2: text blastguard does not read as a
    /// program here.
    Unreadable,
}

/// The input fds every `exec` stage among `stages` opens for the rest of the
/// line (`exec 3< <(…)`, `exec < <(…)`, `exec 4< f`), in order. Only an
/// `exec` that runs no command (every word after it a redirection) is one: an
/// `exec CMD …` replaces the shell, and its redirections are CMD's. Output
/// redirections are not recorded (nothing reads a program from them).
pub(super) fn exec_opened_fds(stages: &[String]) -> Vec<(u32, ExecFd)> {
    let mut out = Vec::new();
    for stage in stages {
        let words = super::super::quote_aware_words(stage);
        if words.first().map(String::as_str) != Some("exec") {
            continue;
        }
        let mut j = 1;
        while j < words.len() {
            let Some((fd, op, glued)) = parse_redirect(&words[j]) else {
                break;
            };
            let target = if glued.is_empty() {
                j += 1;
                words.get(j).map_or("", String::as_str)
            } else {
                glued
            };
            j += 1;
            if !op.starts_with('<') {
                continue;
            }
            let fd = fd.unwrap_or(0);
            let opened = match op {
                "<" | "<>" => match procsub_body(target) {
                    Some(body) => ExecFd::Procsub(body.to_string()),
                    None => ExecFd::File,
                },
                "<&" => match target.parse::<u32>() {
                    Ok(n) if n <= 2 => ExecFd::File,
                    _ if target == "-" => continue,
                    _ => ExecFd::Unreadable,
                },
                "<<" | "<<-" | "<<<" => ExecFd::HereText,
                _ => ExecFd::Unreadable,
            };
            out.push((fd, opened));
        }
    }
    out
}

/// Literal text an `exec` opened an fd on.
#[derive(Debug, Clone, PartialEq, Eq)]
enum HereText {
    /// `N<<< WORD`: the operand word as written (quotes kept).
    String(String),
    /// `N<<'D'`: the operator and delimiter as written, the bare delimiter,
    /// and the body lines.
    Doc {
        opener: String,
        delimiter: String,
        body: String,
    },
}

impl HereText {
    /// The redirection that hands this text to a command as its stdin.
    fn stdin_redirect(&self) -> String {
        match self {
            HereText::String(word) => format!("<<< {word}"),
            HereText::Doc { opener, .. } => opener.clone(),
        }
    }

    /// What follows the command line: a here-document's body and closing line.
    fn trailer(&self) -> String {
        match self {
            HereText::String(_) => String::new(),
            HereText::Doc {
                delimiter, body, ..
            } if body.is_empty() => format!("\n{delimiter}"),
            HereText::Doc {
                delimiter, body, ..
            } => format!("\n{body}\n{delimiter}"),
        }
    }
}

/// What fd N refers to at one point of the line, for an fd some `exec` opens
/// on literal text. An fd absent from the map is the one the line inherited.
#[derive(Debug, Clone)]
enum FdState {
    /// Not literal text (a file, a substitution, a standard fd): read by
    /// [`super::procsub_programs`] as before.
    Other,
    /// Opened, in the modelled shape, on this text.
    Text(HereText),
    /// May hold any of these texts (or something else): what it holds cannot
    /// be determined (`why`). A reader of it is an Ask, and each text is still
    /// judged, so a literal harm is a Deny.
    Undetermined { texts: Vec<HereText>, why: String },
}

impl FdState {
    fn texts(&self) -> Vec<HereText> {
        match self {
            FdState::Other => Vec::new(),
            FdState::Text(h) => vec![h.clone()],
            FdState::Undetermined { texts, .. } => texts.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Frame {
    Brace,
    Subshell,
    Function,
    Block,
}

/// One segment of the line, its compound syntax removed, with where it sits.
struct Stage {
    /// The simple command's words.
    words: Vec<String>,
    /// The segment as written.
    raw: String,
    /// Inside a subshell `( … )` or a function body.
    isolated: bool,
    /// Inside `if` / `while` / `until` / `for`, or the right side of `&&` / `||`.
    conditional: bool,
    /// Runs in its own process: a pipeline member or a background job.
    forked: bool,
    /// Its stdin is a pipe.
    piped_in: bool,
    /// The structure of the line could not be followed up to here.
    poisoned: Option<String>,
    /// Which pipeline (run of `|`-joined segments) it belongs to.
    pipeline: usize,
    /// A here-document body line: data or a program another command reads,
    /// never a command of this shell (`words` is empty).
    body: bool,
}

/// The segments of `cmd` (here-document bodies left out), each with its
/// compound syntax removed and the context it runs in. Structure this does
/// not follow (`case`, `coproc`, an arithmetic `((`, an unbalanced group)
/// poisons every later stage: what an fd holds there is undetermined.
fn structured_stages(cmd: &str) -> Vec<Stage> {
    let segs = split_segments_with_separators(cmd);
    let (is_body, _) = here_document_layout(&segs);
    let mut stack: Vec<Frame> = Vec::new();
    let mut poisoned: Option<String> = None;
    let mut fn_pending = false;
    let mut pipeline = 0usize;
    let mut out = Vec::new();
    for (i, seg) in segs.iter().enumerate() {
        let prev = i.checked_sub(1).map(|p| segs[p].sep_after);
        if prev != Some(SegmentSep::Pipe) {
            pipeline += 1;
        }
        if is_body.get(i).copied().unwrap_or(false) {
            out.push(Stage {
                words: Vec::new(),
                raw: seg.text.clone(),
                isolated: false,
                conditional: false,
                forked: false,
                piped_in: false,
                poisoned: poisoned.clone(),
                pipeline,
                body: true,
            });
            continue;
        }
        let mut words = super::super::quote_aware_words(&seg.text);
        let mut k = 0;
        let poison = |why: &str, poisoned: &mut Option<String>| {
            if poisoned.is_none() {
                *poisoned = Some(why.to_string());
            }
        };
        while let Some(w) = words.get(k).cloned() {
            if let Some(rest) = w.strip_prefix('(') {
                if rest.starts_with('(') {
                    poison("an arithmetic `((`", &mut poisoned);
                    break;
                }
                stack.push(if fn_pending {
                    Frame::Function
                } else {
                    Frame::Subshell
                });
                fn_pending = false;
                if rest.is_empty() {
                    k += 1;
                } else {
                    words[k] = rest.to_string();
                }
                continue;
            }
            match w.as_str() {
                "{" => {
                    stack.push(if fn_pending {
                        Frame::Function
                    } else {
                        Frame::Brace
                    });
                    fn_pending = false;
                    k += 1;
                }
                "}" => {
                    if !matches!(stack.pop(), Some(Frame::Brace | Frame::Function)) {
                        poison("an unbalanced `}`", &mut poisoned);
                    }
                    k += 1;
                }
                "if" | "while" | "until" => {
                    stack.push(Frame::Block);
                    k += 1;
                }
                "then" | "do" | "else" | "elif" | "!" => k += 1,
                "fi" | "done" => {
                    if stack.pop() != Some(Frame::Block) {
                        poison("an unbalanced `fi` / `done`", &mut poisoned);
                    }
                    k += 1;
                }
                "for" => {
                    // The header is a word list, not a command.
                    stack.push(Frame::Block);
                    k = words.len();
                }
                "function" => {
                    fn_pending = true;
                    k += 2;
                    if words.get(k).map(String::as_str) == Some("()") {
                        k += 1;
                    }
                }
                "case" | "esac" | "select" | "coproc" => {
                    poison("a `case` / `select` / `coproc`", &mut poisoned);
                    k = words.len();
                }
                _ if is_function_name(w.strip_suffix("()").unwrap_or("")) => {
                    fn_pending = true;
                    k += 1;
                }
                _ if is_function_name(&w) && words.get(k + 1).map(String::as_str) == Some("()") => {
                    fn_pending = true;
                    k += 2;
                }
                _ => break,
            }
        }
        let k = k.min(words.len());
        if fn_pending && k < words.len() {
            poison("a function body that is not a group", &mut poisoned);
        }
        let (command, closes, open) = strip_unmatched_close_parens(&words[k..].join(" "));
        let isolated = stack
            .iter()
            .any(|f| matches!(f, Frame::Subshell | Frame::Function));
        let conditional = stack.contains(&Frame::Block) || prev == Some(SegmentSep::Conditional);
        let forked = prev == Some(SegmentSep::Pipe)
            || matches!(seg.sep_after, SegmentSep::Pipe | SegmentSep::Background);
        out.push(Stage {
            words: super::super::quote_aware_words(&command),
            raw: seg.text.clone(),
            isolated,
            conditional,
            forked,
            piped_in: prev == Some(SegmentSep::Pipe),
            poisoned: poisoned.clone(),
            pipeline,
            body: false,
        });
        for _ in 0..closes {
            if !matches!(stack.pop(), Some(Frame::Subshell | Frame::Function)) {
                poison("an unbalanced `)`", &mut poisoned);
            }
        }
        if open > 0 {
            poison("a parenthesis left open", &mut poisoned);
        }
    }
    out
}

/// A shell function name (`f`, `my_fn`, `a-b`): no quotes, no expansion.
fn is_function_name(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ':'))
}

/// The redirections of an `exec` that runs no command (`None` when a word
/// after `exec` is not a redirection, or there is none), as
/// `(fd, operator, target)`.
fn exec_only_redirects(words: &[String]) -> Option<Vec<(Option<u32>, String, String)>> {
    let mut out = Vec::new();
    let mut j = 0;
    while j < words.len() {
        let (fd, op, glued) = parse_redirect(&words[j])?;
        let target = if glued.is_empty() {
            j += 1;
            words.get(j)?.clone()
        } else {
            glued.to_string()
        };
        out.push((fd, op.to_string(), target));
        j += 1;
    }
    (!out.is_empty()).then_some(out)
}

/// The index of `exec` in `words` after any assignment / `command` / `builtin`
/// prefix, and whether there was such a prefix.
fn exec_word(words: &[String]) -> Option<(usize, bool)> {
    let mut i = 0;
    while i < words.len()
        && (is_assignment(&words[i]) || matches!(words[i].as_str(), "command" | "builtin"))
    {
        i += 1;
    }
    (words.get(i).map(String::as_str) == Some("exec")).then_some((i, i > 0))
}

/// The fds above 0 some `exec` on the line opens on a here-string or a
/// here-document, closed under `exec N<&M` (fd 0 is left to
/// [`super::procsub_programs`]).
fn here_text_fds(stages: &[Stage]) -> Vec<u32> {
    let mut out: Vec<u32> = Vec::new();
    let mut dups: Vec<(u32, u32)> = Vec::new();
    for st in stages {
        let Some((at, _)) = exec_word(&st.words) else {
            continue;
        };
        let Some(redirects) = exec_only_redirects(&st.words[at + 1..]) else {
            continue;
        };
        for (fd, op, target) in redirects {
            let n = fd.unwrap_or(if op.starts_with('<') { 0 } else { 1 });
            if matches!(op.as_str(), "<<<" | "<<" | "<<-") && n > 0 && !out.contains(&n) {
                out.push(n);
            }
            if matches!(op.as_str(), "<&" | ">&") {
                if let Ok(m) = target.parse::<u32>() {
                    dups.push((n, m));
                }
            }
        }
    }
    loop {
        let before = out.len();
        for &(n, m) in &dups {
            if out.contains(&m) && !out.contains(&n) && n > 0 {
                out.push(n);
            }
        }
        if out.len() == before {
            return out;
        }
    }
}

/// The body of the here-document a segment `opener_raw` opens, located on the
/// physical lines of `cmd`: `None` unless exactly one line holds that segment
/// and opens exactly one here-document, and a closing line follows.
fn here_document_body(cmd: &str, opener_raw: &str) -> Option<(String, String)> {
    let openers = here_document_openers(opener_raw);
    let [opener] = openers.as_slice() else {
        return None;
    };
    let needle = opener_raw.trim();
    let lines: Vec<&str> = cmd.split('\n').collect();
    let hits: Vec<usize> = (0..lines.len())
        .filter(|&i| lines[i].contains(needle) && here_document_openers(lines[i]).len() == 1)
        .collect();
    let [at] = hits.as_slice() else {
        return None;
    };
    let mut body: Vec<&str> = Vec::new();
    for line in &lines[at + 1..] {
        if segment_closes_here_document(line, opener) {
            return Some((opener.delimiter.clone(), body.join("\n")));
        }
        body.push(line);
    }
    None
}

/// How a stage mentions fd N.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RefKind {
    /// `<&N` / `0<&N`: stdin is a copy of N.
    StdinDup,
    /// `M<&N` / `>&N` for M other than 0: another fd is a copy of N.
    OtherDup,
    /// A `/dev/fd/N` or `/proc/self/fd/N` operand.
    FdPath,
}

struct FdRef {
    fd: u32,
    kind: RefKind,
    at: usize,
    span: usize,
}

/// `N` of a `/dev/fd/N` / `/proc/self/fd/N` word.
fn fd_path(word: &str) -> Option<u32> {
    let n = word
        .strip_prefix("/dev/fd/")
        .or_else(|| word.strip_prefix("/proc/self/fd/"))?;
    (!n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
        .then(|| n.parse().ok())
        .flatten()
}

/// The mentions of fd `n` in `raw`, counted on the text: `<&N` / `>&N`
/// (spaces allowed after `&`) and `/fd/N`, each with N not followed by a digit.
/// Compared against the mentions the word reading recognised, so a mention in
/// a shape it does not read (quoted, inside a substitution, glued to other
/// text) is not silently passed over.
fn raw_fd_mentions(raw: &str, n: u32) -> usize {
    let chars: Vec<char> = raw.chars().collect();
    let digits_at = |mut k: usize| -> Option<u32> {
        let start = k;
        while k < chars.len() && chars[k].is_ascii_digit() {
            k += 1;
        }
        (k > start)
            .then(|| chars[start..k].iter().collect::<String>().parse().ok())
            .flatten()
    };
    let mut count = 0;
    for i in 0..chars.len() {
        if chars[i] == '&' && i > 0 && matches!(chars[i - 1], '<' | '>') {
            let mut k = i + 1;
            while k < chars.len() && chars[k] == ' ' {
                k += 1;
            }
            if digits_at(k) == Some(n) {
                count += 1;
            }
        }
        if chars[i..].starts_with(&['/', 'f', 'd', '/']) && digits_at(i + 4) == Some(n) {
            count += 1;
        }
    }
    count
}

/// The reader heads (shells, code interpreters; `source` / `.` read as `sh`)
/// of the stages of one pipeline.
fn pipeline_readers(stages: &[&Stage]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for st in stages {
        let refs: Vec<&str> = st.words.iter().map(String::as_str).collect();
        for idx in wrapped_command_heads(&refs) {
            let head = normalized_command(refs[idx]);
            let reader = if head == "source" || head == "." {
                "sh".to_string()
            } else if is_shell(&head) || is_code_interpreter(&head) {
                head
            } else {
                continue;
            };
            if !out.contains(&reader) {
                out.push(reader);
            }
        }
    }
    out
}

/// Whether the reader of `st` runs the word at `at` as its script file
/// (read with the reader's option grammar, as the pipe rule reads it), and
/// the spelling that makes it read the script from stdin instead (`-s` for a
/// shell, `-` for a code interpreter). `None` for any other reader or
/// position (`bash x.sh /dev/fd/3`, `source /dev/fd/3`).
fn stdin_script_spelling(st: &Stage, at: usize) -> Option<&'static str> {
    let refs: Vec<&str> = st.words.iter().map(String::as_str).collect();
    let r = wrapped_command_heads(&refs).into_iter().find(|&idx| {
        let head = normalized_command(refs[idx]);
        is_shell(&head) || is_code_interpreter(&head)
    })?;
    let head = normalized_command(refs[r]);
    // The argv words after the reader (redirections and their targets left
    // out), with their indices in `refs`.
    let mut argv: Vec<usize> = Vec::new();
    let mut j = r + 1;
    while j < refs.len() {
        if let Some((_, _, glued)) = parse_redirect(refs[j]) {
            j += if glued.is_empty() { 2 } else { 1 };
            continue;
        }
        argv.push(j);
        j += 1;
    }
    let operands: Vec<&str> = argv.iter().map(|&k| refs[k]).collect();
    let source = if is_shell(&head) {
        shell_program_source(&operands)
    } else if is_python_head(&head) {
        python_program_source(&operands)
    } else if is_node_head(&head) {
        node_program_source(&operands)
    } else {
        interpreter_program_source(&operands)
    };
    match source {
        ProgramSource::File(k) if argv.get(k) == Some(&at) => {
            Some(if is_shell(&head) { "-s" } else { "-" })
        }
        _ => None,
    }
}

/// The texts `exec` opens a NAMED fd on (`exec {fd}<<< '…'`, bash's
/// allocated fd): its number is not on the line, so a read through an fd
/// spelled with an expansion (`<&$fd`, `/dev/fd/$fd`) may be any of them.
fn named_fd_texts(cmd: &str, stages: &[Stage]) -> Vec<HereText> {
    let mut out = Vec::new();
    for st in stages {
        let Some((at, _)) = exec_word(&st.words) else {
            continue;
        };
        let mut j = at + 1;
        while j < st.words.len() {
            let w = &st.words[j];
            j += 1;
            let Some(after) = w
                .strip_prefix('{')
                .and_then(|r| r.split_once('}'))
                .filter(|(name, _)| is_function_name(name))
                .map(|(_, after)| after)
            else {
                continue;
            };
            let Some((None, op, glued)) = parse_redirect(after) else {
                continue;
            };
            let target = if glued.is_empty() {
                let t = st.words.get(j).cloned().unwrap_or_default();
                j += 1;
                t
            } else {
                glued.to_string()
            };
            match op {
                "<<<" => out.push(HereText::String(target)),
                "<<" | "<<-" => {
                    if let Some((delimiter, body)) = here_document_body(cmd, &st.raw) {
                        out.push(HereText::Doc {
                            opener: format!("{op}{target}"),
                            delimiter,
                            body,
                        });
                    }
                }
                _ => {}
            }
        }
    }
    out
}

/// Each of `texts` handed inline to each reader of the pipeline `stages`
/// (`READER <<< '…'`): what judges a text whose reading this pass cannot
/// follow, so a literal harm is still a Deny. With no reader in the pipeline
/// the text is read as `sh` would run it when `default_sh` is set.
fn probe_synthetics(texts: &[HereText], stages: &[&Stage], default_sh: bool) -> Vec<String> {
    let mut readers = pipeline_readers(stages);
    if readers.is_empty() && default_sh {
        readers.push("sh".to_string());
    }
    let mut out = Vec::new();
    for text in texts {
        for reader in &readers {
            out.push(format!(
                "{reader} {}{}",
                text.stdin_redirect(),
                text.trailer()
            ));
        }
    }
    out
}

/// A dup or fd-path mention whose fd is spelled with an expansion
/// (`<&$fd`, `<&"$fd"`, `/dev/fd/$fd`).
fn mentions_dynamic_fd(words: &[&str]) -> bool {
    let mut j = 0;
    while j < words.len() {
        if let Some((_, op, glued)) = parse_redirect(words[j]) {
            let target = if glued.is_empty() {
                words.get(j + 1).copied().unwrap_or("")
            } else {
                glued
            };
            if matches!(op, "<&" | ">&") && target.contains('$') {
                return true;
            }
            j += if glued.is_empty() { 2 } else { 1 };
            continue;
        }
        let w = words[j];
        if w.contains('$') && (w.contains("/dev/fd/") || w.contains("/proc/self/fd/")) {
            return true;
        }
        j += 1;
    }
    false
}

/// The cross-statement fd verdict of `cmd` (backlog e686a85e) and the fds
/// whose every mention it resolved.
pub(super) struct FdTracking {
    pub(super) verdict: Decision,
    /// Fds whose here-text `exec`s [`super::procsub_programs`] may leave to this
    /// pass: every mention of them on the line was read here.
    pub(super) resolved: Vec<u32>,
}

/// Follows the fds an `exec` opens on a here-string (`exec 3<<< '…'`) or a
/// QUOTED here-document (`exec 3<<'EOF'`) through the statements of `cmd`,
/// in the order they are written (`;`, newline, `&&`, `||`), and judges each
/// later command that reads such an fd (`bash <&3`, `python3 <&3`,
/// `bash /dev/fd/3`) as the same command with the text given inline
/// (`bash <<< '…'`), through the whole of [`super::super::detect_bash`]: the
/// verdict is the inline one. A reassignment of N (`exec 3<<< …` again) and a
/// close (`exec 3<&-`) are applied in order, and each fd is followed on its
/// own. A brace group `{ … }` is read through.
///
/// Everything else is undetermined and is an Ask (a Deny in a subagent),
/// never an Allow; every text the fd may hold is still judged as above, so a
/// literal harm is a Deny in main too: an `exec` in a subshell, a function
/// body, a pipeline, behind `&&` / `||` or inside `if` / a loop, an unquoted
/// here-document, an `N<&M` chain (M ≥ 3), a read after a close, a read of
/// an fd nothing before it opens, a reader inside a subshell or a function
/// body, a reader whose other stdin redirections compete with the fd, a
/// mention of the fd in a shape this does not read (quoted, inside a
/// substitution or a `-c` / `eval` string, on a here-document body line), and
/// a read through an fd spelled with an expansion (`<&$fd`, `<&$((1+2))`,
/// `/dev/fd/$fd`) on a line where an `exec` opens a numbered or named fd
/// (`exec {fd}<<< '…'`) on literal text: every text opened so far is judged.
/// Where no reader is
/// visible, a text is judged as `sh` would run it. fd 0 (`exec <<< '…'`) is
/// not followed here.
pub(super) fn cross_statement_fd_verdict(cmd: &str, depth: usize, ctx: &Ctx<'_>) -> FdTracking {
    let none = FdTracking {
        verdict: Decision::Allow,
        resolved: Vec::new(),
    };
    if !cmd.contains("exec") || !cmd.contains("<<") {
        return none;
    }
    let stages = structured_stages(cmd);
    let tracked = here_text_fds(&stages);
    let named = named_fd_texts(cmd, &stages);
    if tracked.is_empty() && named.is_empty() {
        return none;
    }
    let mut unresolved: Vec<u32> = Vec::new();
    let mut acc = VerdictAcc::default();
    let mut fds: HashMap<u32, FdState> = HashMap::new();
    let judge = |text: &str, acc: &mut VerdictAcc| -> Option<Decision> {
        if depth >= MAX_SHELL_DEPTH {
            return acc.record(super::super::budget::depth_exhausted());
        }
        acc.record(super::super::detect_bash(text, depth + 1, ctx))
    };
    for (si, st) in stages.iter().enumerate() {
        // A here-document body line that mentions the fd: a program another
        // command runs from that body (`bash <<'E'`, then `bash <&3`) inherits
        // the fd, and blastguard cannot tell data from program there. Each
        // text the fd may hold is judged as `sh` would run it, and the fd is
        // left unresolved (an Ask).
        if st.body {
            for &n in &tracked {
                if raw_fd_mentions(&st.raw, n) == 0 {
                    continue;
                }
                if !unresolved.contains(&n) {
                    unresolved.push(n);
                }
                let texts = fds.get(&n).map(FdState::texts).unwrap_or_default();
                for synthetic in probe_synthetics(&texts, &[], true) {
                    if let Some(deny) = judge(&synthetic, &mut acc) {
                        return FdTracking {
                            verdict: deny,
                            resolved: Vec::new(),
                        };
                    }
                }
                if let Some(deny) = acc.record(Decision::ask(format!(
                    "the here-document line `{}` mentions fd {n}, which an `exec` on this line \
opens on literal text — blastguard cannot tell whether a program read from that here-document \
runs that text",
                    st.raw.trim()
                ))) {
                    return FdTracking {
                        verdict: deny,
                        resolved: Vec::new(),
                    };
                }
            }
            continue;
        }
        let words: Vec<&str> = st.words.iter().map(String::as_str).collect();
        // An `exec` that opens, duplicates or closes fds.
        if let Some((at, prefixed)) = exec_word(&st.words) {
            if let Some(redirects) = exec_only_redirects(&st.words[at + 1..]) {
                for &n in &tracked {
                    let seen = redirects
                        .iter()
                        .filter(|(_, op, t)| {
                            matches!(op.as_str(), "<&" | ">&") && t.parse::<u32>() == Ok(n)
                        })
                        .count();
                    if raw_fd_mentions(&st.raw, n) != seen && !unresolved.contains(&n) {
                        unresolved.push(n);
                    }
                }
                let unmodelled = if let Some(why) = &st.poisoned {
                    Some(format!(
                        "after {why}, whose structure blastguard does not follow"
                    ))
                } else if st.isolated {
                    Some("inside a subshell or a function body".to_string())
                } else if st.conditional {
                    Some("conditionally (behind `&&` / `||`, or inside `if` or a loop)".to_string())
                } else if st.forked {
                    Some("in a pipeline or a background job".to_string())
                } else if prefixed {
                    Some("behind an assignment or `command` / `builtin`".to_string())
                } else {
                    None
                };
                for (fd, op, target) in redirects {
                    let n = fd.unwrap_or(if op.starts_with('<') { 0 } else { 1 });
                    let mut targets = vec![n];
                    if op.starts_with('&') {
                        targets = vec![1, 2];
                    }
                    for n in targets {
                        let old = fds.get(&n).cloned();
                        let new = exec_redirect_state(cmd, st, n, &op, &target, old.as_ref(), &fds);
                        let state = match (&unmodelled, new) {
                            (None, Some(new)) => Some(new),
                            (None, None) => old.clone(),
                            (Some(why), new) => {
                                let mut texts =
                                    old.as_ref().map(FdState::texts).unwrap_or_default();
                                for t in new.as_ref().map(FdState::texts).unwrap_or_default() {
                                    if !texts.contains(&t) {
                                        texts.push(t);
                                    }
                                }
                                Some(FdState::Undetermined {
                                    texts,
                                    why: format!("fd {n} is (re)opened by an `exec` {why}"),
                                })
                            }
                        };
                        match state {
                            Some(s) => {
                                fds.insert(n, s);
                            }
                            None => {
                                fds.remove(&n);
                            }
                        }
                    }
                }
                continue;
            }
        }
        // Any other command: its mentions of a tracked fd.
        let mut reopened: Vec<u32> = Vec::new();
        let mut stdin_redirects = 0usize;
        let mut refs: Vec<FdRef> = Vec::new();
        let mut j = 0;
        while j < words.len() {
            if let Some((fd, op, glued)) = parse_redirect(words[j]) {
                let span = if glued.is_empty() { 2 } else { 1 };
                let target = if glued.is_empty() {
                    words.get(j + 1).copied().unwrap_or("")
                } else {
                    glued
                };
                let eff = fd.unwrap_or(if op.starts_with('<') { 0 } else { 1 });
                if let Some(f) = fd {
                    reopened.push(f);
                }
                if eff == 0 && (op.starts_with('<') || op == ">&") {
                    stdin_redirects += 1;
                }
                if matches!(op, "<&" | ">&") {
                    if let Ok(m) = target.parse::<u32>() {
                        refs.push(FdRef {
                            fd: m,
                            kind: if eff == 0 {
                                RefKind::StdinDup
                            } else {
                                RefKind::OtherDup
                            },
                            at: j,
                            span,
                        });
                    }
                }
                j += span;
                continue;
            }
            if let Some(m) = fd_path(words[j]) {
                refs.push(FdRef {
                    fd: m,
                    kind: RefKind::FdPath,
                    at: j,
                    span: 1,
                });
            }
            j += 1;
        }
        let pipeline: Vec<(usize, &Stage)> = stages
            .iter()
            .enumerate()
            .filter(|(_, s)| s.pipeline == st.pipeline)
            .collect();
        let pipeline_stages: Vec<&Stage> = pipeline.iter().map(|(_, s)| *s).collect();
        // A read through an fd spelled with an expansion (`<&$fd`,
        // `<&$((1+2))`, `/dev/fd/$fd`) may be any fd an `exec` opened on
        // literal text before it, numbered or named: each such text is
        // judged, and the read is an Ask.
        if mentions_dynamic_fd(&words) {
            let mut texts: Vec<HereText> = named.clone();
            let mut open: Vec<(&u32, &FdState)> = fds.iter().collect();
            open.sort_by_key(|(n, _)| **n);
            for (_, state) in open {
                for t in state.texts() {
                    if !texts.contains(&t) {
                        texts.push(t);
                    }
                }
            }
            for synthetic in probe_synthetics(&texts, &pipeline_stages, true) {
                if let Some(deny) = judge(&synthetic, &mut acc) {
                    return FdTracking {
                        verdict: deny,
                        resolved: Vec::new(),
                    };
                }
            }
            if let Some(deny) = acc.record(Decision::ask(format!(
                "`{}` reads an fd spelled with an expansion, and an `exec` on this line opens an \
fd (`N<<<` / `N<<` / `{{NAME}}<<<`) on literal text — blastguard cannot tell which text it reads",
                st.raw.trim()
            ))) {
                return FdTracking {
                    verdict: deny,
                    resolved: Vec::new(),
                };
            }
        }
        for &n in &tracked {
            let seen = refs.iter().filter(|r| r.fd == n).count();
            if raw_fd_mentions(&st.raw, n) != seen {
                if !unresolved.contains(&n) {
                    unresolved.push(n);
                }
                // Each text the fd may hold, as a shell (or this pipeline's
                // reader) would run it: a literal harm stays a Deny.
                let texts = fds.get(&n).map(FdState::texts).unwrap_or_default();
                for synthetic in probe_synthetics(&texts, &pipeline_stages, true) {
                    if let Some(deny) = judge(&synthetic, &mut acc) {
                        return FdTracking {
                            verdict: deny,
                            resolved: Vec::new(),
                        };
                    }
                }
                if let Some(deny) = acc.record(Decision::ask(format!(
                    "`{}` mentions fd {n}, which an `exec` on this line opens on literal text, in \
a form blastguard does not read — it cannot tell whether a shell or interpreter runs that text",
                    st.raw.trim()
                ))) {
                    return FdTracking {
                        verdict: deny,
                        resolved: Vec::new(),
                    };
                }
            }
        }
        for r in &refs {
            if !tracked.contains(&r.fd) || reopened.contains(&r.fd) {
                continue;
            }
            let n = r.fd;
            let (texts, mut why) = match fds.get(&n) {
                None => (
                    Vec::new(),
                    Some(format!(
                        "nothing before it on this line opens fd {n} (it is the inherited fd)"
                    )),
                ),
                Some(FdState::Other) => continue,
                Some(FdState::Text(h)) => (vec![h.clone()], None),
                Some(FdState::Undetermined { texts, why }) => (texts.clone(), Some(why.clone())),
            };
            if why.is_none() {
                why = if let Some(p) = &st.poisoned {
                    Some(format!(
                        "the reader comes after {p}, whose structure blastguard does not follow"
                    ))
                } else if st.isolated {
                    Some("the reader is inside a subshell or a function body".to_string())
                } else {
                    None
                };
            }
            let script = match r.kind {
                RefKind::StdinDup if stdin_redirects == 1 => Some(None),
                RefKind::FdPath if stdin_redirects == 0 && !st.piped_in => {
                    stdin_script_spelling(st, r.at).map(Some)
                }
                _ => None,
            };
            for text in &texts {
                let synthetics: Vec<String> = match script {
                    Some(spelling) => {
                        // The pipeline as written, with this mention replaced
                        // by the text given inline.
                        let mut members: Vec<String> = Vec::new();
                        for (pi, p) in &pipeline {
                            if *pi != si {
                                members.push(p.words.join(" "));
                                continue;
                            }
                            let mut w: Vec<String> = p.words.clone();
                            match spelling {
                                None => {
                                    w.splice(r.at..r.at + r.span, [text.stdin_redirect()]);
                                }
                                Some(s) => {
                                    w[r.at] = s.to_string();
                                    w.push(text.stdin_redirect());
                                }
                            }
                            members.push(w.join(" "));
                        }
                        vec![format!("{}{}", members.join(" | "), text.trailer())]
                    }
                    None => probe_synthetics(std::slice::from_ref(text), &pipeline_stages, false),
                };
                for synthetic in synthetics {
                    if let Some(deny) = judge(&synthetic, &mut acc) {
                        return FdTracking {
                            verdict: deny,
                            resolved: Vec::new(),
                        };
                    }
                }
            }
            let why = match (why, script) {
                (Some(why), _) => Some(why),
                (None, None) => Some(format!(
                    "`{}` reads fd {n} in a shape blastguard does not follow (a copy into \
another fd, a competing stdin redirection, or an fd path that is not the script)",
                    st.raw.trim()
                )),
                (None, Some(_)) => None,
            };
            if let Some(why) = why {
                if let Some(deny) = acc.record(Decision::ask(format!(
                    "`{}` reads fd {n}, which an `exec` on this line opens on literal text, and \
{why} — blastguard cannot tell which text it reads",
                    st.raw.trim()
                ))) {
                    return FdTracking {
                        verdict: deny,
                        resolved: Vec::new(),
                    };
                }
            }
        }
    }
    FdTracking {
        verdict: acc.finish(),
        resolved: tracked
            .into_iter()
            .filter(|n| !unresolved.contains(n))
            .collect(),
    }
}

/// What fd `n` refers to after `exec`'s redirection `op target`, in the
/// modelled shape; `None` for the inherited fd (a close of one).
fn exec_redirect_state(
    cmd: &str,
    st: &Stage,
    n: u32,
    op: &str,
    target: &str,
    old: Option<&FdState>,
    fds: &HashMap<u32, FdState>,
) -> Option<FdState> {
    match op {
        "<<<" => Some(FdState::Text(HereText::String(target.to_string()))),
        "<<" | "<<-" => {
            let quoted = target.contains(['\'', '"', '\\']);
            match here_document_body(cmd, &st.raw) {
                Some((delimiter, body)) => {
                    let doc = HereText::Doc {
                        opener: format!("{op}{target}"),
                        delimiter,
                        body,
                    };
                    Some(if quoted {
                        FdState::Text(doc)
                    } else {
                        FdState::Undetermined {
                            texts: vec![doc],
                            why: format!(
                                "fd {n} is opened on an unquoted here-document, whose body the \
shell expands at run time"
                            ),
                        }
                    })
                }
                None => Some(FdState::Undetermined {
                    texts: Vec::new(),
                    why: format!(
                        "blastguard could not locate the body of the here-document fd {n} is \
opened on"
                    ),
                }),
            }
        }
        "<&" | ">&" if target == "-" => match old {
            None => None,
            Some(FdState::Other) => Some(FdState::Other),
            Some(s) => Some(FdState::Undetermined {
                texts: s.texts(),
                why: format!("fd {n} was closed (`exec {n}<&-`) before it is read"),
            }),
        },
        "<&" | ">&" => match target.parse::<u32>() {
            Ok(m) if m <= 2 => Some(FdState::Other),
            Ok(m) => match fds.get(&m) {
                Some(FdState::Other) => Some(FdState::Other),
                other => Some(FdState::Undetermined {
                    texts: other.map(FdState::texts).unwrap_or_default(),
                    why: format!(
                        "fd {n} is a copy of fd {m} (`exec {n}<&{m}`), a chain blastguard does \
not follow"
                    ),
                }),
            },
            Err(_) => Some(FdState::Undetermined {
                texts: Vec::new(),
                why: format!("fd {n} is a copy of `{target}`, which blastguard does not resolve"),
            }),
        },
        _ => Some(FdState::Other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_mentions_count_dups_and_fd_paths() {
        assert_eq!(raw_fd_mentions("bash <&3", 3), 1);
        assert_eq!(raw_fd_mentions("bash <& 3", 3), 1);
        assert_eq!(raw_fd_mentions("bash /dev/fd/3 <&33", 3), 1);
        assert_eq!(raw_fd_mentions("echo '<&3' >&3", 3), 2);
        assert_eq!(raw_fd_mentions("bash 3<&-", 3), 0);
    }

    #[test]
    fn here_text_fds_follow_dup_chains() {
        let stages = structured_stages("exec 3<<< 'x'; exec 4<&3; exec 5< f; bash <&4");
        assert_eq!(here_text_fds(&stages), vec![3, 4]);
    }

    #[test]
    fn structure_marks_subshell_function_and_conditional() {
        let stages = structured_stages(
            "( exec 3<<< a ); f() { bash <&3; }; true && exec 3<<< b; { exec 3<<< c; }",
        );
        let flags: Vec<(String, bool, bool)> = stages
            .iter()
            .map(|s| (s.words.join(" "), s.isolated, s.conditional))
            .collect();
        assert_eq!(
            flags,
            vec![
                ("exec 3<<< a".to_string(), true, false),
                ("bash <&3".to_string(), true, false),
                ("".to_string(), false, false),
                ("true".to_string(), false, false),
                ("exec 3<<< b".to_string(), false, true),
                ("exec 3<<< c".to_string(), false, false),
                ("".to_string(), false, false),
            ]
        );
    }
}
