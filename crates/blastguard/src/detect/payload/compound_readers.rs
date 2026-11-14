//! Readers a pipe reaches through an `if` / `while` / `until` compound or a
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
/// `then`, `do`, `else`, `elif`, `{`), and how many compounds those words
/// open (a `for` / `select` / `case` header opens one too, but its text is
/// kept: it is not a command, and the compound-free reading drops it).
fn strip_compound_prefix(stage: &str) -> (&str, usize) {
    let mut s = stage.trim_start();
    let mut opened = 0;
    loop {
        let word_end = s.find(char::is_whitespace).unwrap_or(s.len());
        let word = &s[..word_end];
        if opens_read_compound(word) {
            opened += 1;
        } else if !matches!(word, "then" | "do" | "else" | "elif" | "{") {
            if opens_any_compound(word) {
                opened += 1;
            }
            return (s, opened);
        }
        s = s[word_end..].trim_start();
    }
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
/// `if` / `while` / `until` compound, or runs a command through `eval` or a
/// shell's `-c`. Returns `(pipelines, undetermined)`; a compound whose closing
/// `fi` / `done` is not found is an Ask reason.
///
/// A compound's members are its statements up to the matching closer, each
/// with its leading keywords removed (the condition and every branch: all
/// inherit the pipe, an over-approximation on the restrictive side). A
/// forwarder's member is each statement of the command text it runs — every
/// `-c` candidate of [`dash_c_payloads`], and for `eval` the argument text as
/// written and with one layer of wrapping quotes removed.
pub(super) fn compound_and_forwarder_pipelines(
    statements: &[Vec<String>],
) -> (Vec<String>, Vec<String>) {
    let mut pipelines: Vec<String> = Vec::new();
    let mut undetermined: Vec<String> = Vec::new();
    for (s, statement) in statements.iter().enumerate() {
        for i in 1..statement.len() {
            let upstream = statement[i - 1].trim();
            let stage = statement[i].as_str();
            let first = stage.split_whitespace().next().unwrap_or("");
            if opens_read_compound(first) {
                let (body, opened) = strip_compound_prefix(stage);
                let mut members: Vec<String> = Vec::new();
                let mut head: Vec<String> = vec![body.to_string()];
                head.extend(statement[i + 1..].iter().cloned());
                members.push(joined(&head));
                let mut level = opened;
                let mut closed = false;
                for later in &statements[s + 1..] {
                    let lead = later.first().map(|st| st.trim()).unwrap_or("");
                    let lead_word = lead.split_whitespace().next().unwrap_or("");
                    if closes_compound(lead_word) {
                        level -= 1;
                        if level == 0 {
                            closed = true;
                            break;
                        }
                        continue;
                    }
                    if lead_word == "}" {
                        continue;
                    }
                    let (body, opened) = strip_compound_prefix(lead);
                    level += opened;
                    let mut stages = later.clone();
                    if let Some(st) = stages.first_mut() {
                        *st = body.to_string();
                    }
                    members.push(joined(&stages));
                }
                if !closed {
                    undetermined.push(format!(
                        "`{upstream}` is piped into the compound `{} …`, whose closing `fi` / \
`done` blastguard cannot find — it cannot tell which command inside reads the piped program",
                        stage.trim()
                    ));
                }
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
