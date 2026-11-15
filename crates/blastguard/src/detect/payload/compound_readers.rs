//! Readers a pipe reaches through an `if` / `while` / `until` / `case` compound
//! (behind `!` / `time` too) or a
//! stdin-forwarding wrapper (`eval`, `<shell> -c`), backlog b3569da5.
//!
//! `printf 'chmod 777 /etc/hosts' | if true; then bash; fi` hands the piped
//! program to `bash` exactly as `… | bash` does: every command of the compound
//! inherits the pipe as its stdin. The compound-free reading
//! (`decompounded`) piped only the CONDITION (`… | true ; bash`), so the body's
//! reader was never judged. Likewise `… | eval bash` and `… | sh -c 'bash'`
//! run a command that inherits the pipe; the stage's head (`eval` / `sh -c`)
//! is not the reader. Both are returned as `UPSTREAM | MEMBER` pipelines for
//! `analyze_interpreter_stdin_exec` to judge, the same shape
//! `group_reader_pipelines` returns for subshells and brace groups. Each
//! pipeline holds one compound or forwarder layer fewer than the stage it came
//! from, so the descent terminates.

use super::super::{
    is_shell, normalized_command, split_statements_into_pipe_stages, wrapped_command_heads,
};
use super::{
    argv_words, dash_c_payloads, faithful_tail, shell_program_source, strip_wrapping_quotes,
    ProgramSource,
};

/// Keywords that open a compound whose body this module reads.
fn opens_read_compound(word: &str) -> bool {
    matches!(word, "if" | "while" | "until")
}

/// Keywords that open a compound which a later `fi` / `done` / `esac` closes.
fn opens_any_compound(word: &str) -> bool {
    opens_read_compound(word) || matches!(word, "for" | "select" | "case")
}

fn closes_compound(word: &str) -> bool {
    matches!(word, "fi" | "done" | "esac")
}

/// `stage` with its leading compound syntax removed (`if`, `while`, `until`,
/// `then`, `do`, `else`, `elif`, `{`, a `case WORD in` header and a `case`
/// item's pattern word, and the pipeline prefixes `!` and `time` with its
/// options), and how many compounds those words open (a `for` / `select`
/// header opens one too, but its text is kept: it is not a command, and the
/// compound-free reading drops it).
///
/// An opener behind `!` / `time` (`! if …; fi`) or inside a `case` item
/// (`x) if …; fi`) is counted like any other: missing it let the inner `fi`
/// close the OUTER compound, and the reader after it went unjudged. `Err` (an
/// Ask reason) for `coproc`, whose compound's extent this reading does not
/// model, and for a `case` whose header is not `case WORD in`.
fn strip_compound_prefix(stage: &str) -> Result<(&str, usize), String> {
    let mut s = stage.trim_start();
    let mut opened = 0;
    let mut after_time = false;
    loop {
        let word_end = s.find(char::is_whitespace).unwrap_or(s.len());
        let word = &s[..word_end];
        let time_option = after_time && word.starts_with('-') && word.len() > 1;
        after_time = word == "time" || time_option;
        if word == "case" {
            opened += 1;
            s = after_case_header(s).ok_or_else(|| {
                format!(
                    "`{}` opens a `case` whose `case WORD in` header blastguard cannot read — it \
cannot tell where the enclosing compound ends",
                    stage.trim()
                )
            })?;
            continue;
        }
        if opens_read_compound(word) {
            opened += 1;
        } else if word == "coproc" {
            return Err(format!(
                "`{}` starts a `coproc`, whose extent blastguard does not model — it cannot tell \
where the enclosing compound ends",
                stage.trim()
            ));
        } else if !(time_option
            || is_case_pattern(word)
            || matches!(word, "then" | "do" | "else" | "elif" | "{" | "!" | "time"))
        {
            if opens_any_compound(word) {
                opened += 1;
            }
            return Ok((s, opened));
        }
        s = s[word_end..].trim_start();
    }
}

/// A `case` item's pattern word (`x)`, `*)`): it ends in `)` and holds no
/// `(`, so it is not a subshell (`(bash)`). Outside a `case` such a word in
/// command position is a syntax error, so skipping it there hides nothing.
fn is_case_pattern(word: &str) -> bool {
    word.len() > 1 && word.ends_with(')') && !word.contains('(')
}

/// The text of `case WORD in …` after its third word, which must be `in`.
/// `None` when the header is not three plain words ending in `in`.
fn after_case_header(text: &str) -> Option<&str> {
    let mut s = text.trim_start();
    for expected in ["case", "", "in"] {
        let end = s.find(char::is_whitespace).unwrap_or(s.len());
        let word = &s[..end];
        if word.is_empty() || (!expected.is_empty() && word != expected) {
            return None;
        }
        s = s[end..].trim_start();
    }
    Some(s)
}

/// How many compounds the pipe stages after a statement's first open: a `|`
/// puts the next word in command position too (`echo y | if …; fi`), and an
/// opener there that went uncounted would let its `fi` close the enclosing
/// compound early. Only command-position words count — an argument
/// (`echo if`) or a quoted word never opens a compound. A stage whose prefix
/// cannot be read (`coproc`) records an Ask reason.
fn later_stage_openers(stages: &[String], undetermined: &mut Vec<String>) -> usize {
    stages
        .iter()
        .map(|st| match strip_compound_prefix(st) {
            Ok((_, opened)) => opened,
            Err(why) => {
                undetermined.push(why);
                0
            }
        })
        .sum()
}

fn joined(stages: &[String]) -> String {
    stages
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" | ")
}

fn push(p: String, pipelines: &mut Vec<String>) {
    if !pipelines.contains(&p) {
        pipelines.push(p);
    }
}

/// The `UPSTREAM | MEMBER` pipelines for every piped stage that opens an
/// `if` / `while` / `until` / `case` compound (behind `!` / `time` too), or runs a
/// command through `eval` or a shell's `-c`. Returns `(pipelines,
/// undetermined)`.
///
/// A compound's members are its statements up to the matching closer (to the
/// end of the line when no closer is found), each with its leading keywords
/// removed (the condition and every branch: all
/// inherit the pipe, an over-approximation on the restrictive side). A
/// forwarder's member is each statement of the command text it runs — every
/// `-c` candidate of [`dash_c_payloads`], and for `eval` the argument text as
/// written and with one layer of wrapping quotes removed.
///
/// Keywords are counted in command position only — at the start of each
/// statement (`;`, `&&`, `||`, newline) and pipe stage (`|`), and after
/// `then` / `do` / `else` / `elif` / `{` / `!` / `time` / a `case` pattern
/// (see [`strip_compound_prefix`]); a word in argument position (`echo if`)
/// or inside quotes is not a keyword to the shell either. A `coproc` or an
/// unreadable `case` header, whose extent is not modelled, is an Ask reason.
pub(super) fn compound_and_forwarder_pipelines(
    statements: &[Vec<String>],
) -> (Vec<String>, Vec<String>) {
    let mut pipelines: Vec<String> = Vec::new();
    let mut undetermined: Vec<String> = Vec::new();
    for (s, statement) in statements.iter().enumerate() {
        for i in 1..statement.len() {
            let upstream = statement[i - 1].trim();
            let stage = statement[i].as_str();
            // The stage opens a compound this module reads when its leading
            // words (behind `!` / `time` too) open one and the text left is
            // not a `for` / `select` header (the compound-free reading
            // flattens those). A `case` header is removed by the strip, so
            // the member is shorter than the stage and the descent ends.
            let (body, opened) = match strip_compound_prefix(stage) {
                Ok(read) => read,
                Err(why) => {
                    undetermined.push(why);
                    continue;
                }
            };
            let rest_word = body.split_whitespace().next().unwrap_or("");
            // A `for` / `select` header is not a command, so it is no member
            // (a `P | for x in a` member would recurse onto itself), but its
            // body inherits the pipe: the later statements are read like an
            // `if` body, so a closed nested compound before the reader
            // (`P | for x in a; do if …; fi; bash; done`) no longer hides it
            // from the compound-free reading, which pipes only its first command.
            let loop_header = matches!(rest_word, "for" | "select");
            if opened > 0 && (loop_header || !opens_any_compound(rest_word)) {
                let mut members: Vec<String> = Vec::new();
                let mut head: Vec<String> = if loop_header {
                    Vec::new()
                } else {
                    vec![body.to_string()]
                };
                head.extend(statement[i + 1..].iter().cloned());
                members.push(joined(&head));
                let mut level =
                    opened + later_stage_openers(&statement[i + 1..], &mut undetermined);
                for later in &statements[s + 1..] {
                    let lead = later.first().map(|st| st.trim()).unwrap_or("");
                    let lead_word = lead.split_whitespace().next().unwrap_or("");
                    if closes_compound(lead_word) {
                        level -= 1;
                        if level == 0 {
                            break;
                        }
                        // `fi | if …` opens again after the closer.
                        level += later_stage_openers(&later[1..], &mut undetermined);
                        continue;
                    }
                    if lead_word == "}" {
                        level += later_stage_openers(&later[1..], &mut undetermined);
                        continue;
                    }
                    let (body, opened) = match strip_compound_prefix(lead) {
                        Ok(read) => read,
                        Err(why) => {
                            undetermined.push(why);
                            (lead, 0)
                        }
                    };
                    level += opened + later_stage_openers(&later[1..], &mut undetermined);
                    let mut stages = later.clone();
                    if let Some(st) = stages.first_mut() {
                        *st = body.to_string();
                    }
                    members.push(joined(&stages));
                }
                // An unclosed compound hides nothing: every statement to the
                // end of the line is already a member (and a member of a
                // split-up group arrives here without its closer, see
                // `group_reader_pipelines`).
                for member in members.into_iter().filter(|m| !m.is_empty()) {
                    push(format!("{upstream} | {member}"), &mut pipelines);
                }
                continue;
            }
            for text in forwarded_texts(stage) {
                for member in split_statements_into_pipe_stages(&text) {
                    let member = joined(&member);
                    if !member.is_empty() {
                        push(format!("{upstream} | {member}"), &mut pipelines);
                    }
                }
            }
        }
    }
    (pipelines, undetermined)
}

/// The command texts `stage` runs with its own stdin when its head (behind any
/// exec-wrapper) is `eval` or a shell given `-c`. Empty otherwise.
fn forwarded_texts(stage: &str) -> Vec<String> {
    let tokens: Vec<&str> = stage.split_whitespace().collect();
    let mut out: Vec<String> = Vec::new();
    for idx in wrapped_command_heads(&tokens) {
        let head = normalized_command(tokens[idx]);
        let rest = &tokens[idx + 1..];
        if head == "eval" {
            let text = faithful_tail(Some(stage), &tokens, idx + 1)
                .map_or_else(|| rest.join(" "), str::to_string);
            let peeled = strip_wrapping_quotes(&text).to_string();
            if peeled != text {
                out.push(peeled);
            }
            out.push(text);
        } else if is_shell(&head)
            && shell_program_source(&argv_words(rest)) == ProgramSource::Inline
        {
            out.extend(dash_c_payloads(rest, Some(stage)));
        }
    }
    out.retain(|t| !t.trim().is_empty());
    out
}
