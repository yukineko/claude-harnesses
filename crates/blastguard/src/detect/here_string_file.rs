//! A here-string (`<<<`) that `cat` / `tee` writes to a file is the file's
//! content, and that content is judged as a shell script when a shell may run
//! it (tests/heredoc_system_harm_holes_20261111.rs case E:
//! `cat > x.rs <<< 'rm -rf ~' && sh x.rs` was Allow). The here-DOCUMENT
//! twin keeps its body in the command text, where every shell rule reads it
//! line by line, unless the target language rules otherwise (user ruling
//! 2026-11-08, [`super::heredoc::python_file_here_document`] /
//! [`super::heredoc::non_shell_code_file_here_document`]); a here-string's
//! operand sat on the writer's line as one quoted word, which no rule read
//! as a script. This module gives it the same reading, by the same target
//! classes:
//!   * every target a data file or `/dev/null`
//!     ([`super::heredoc::is_data_file_target`]): not judged, as for the
//!     here-document twin;
//!   * every target a `.py` file or a non-shell code file (`.rs`, `.go`, …):
//!     judged only when another segment of the command is not an inert
//!     read-only command ([`super::heredoc::INERT_LATER_COMMANDS`], with no
//!     `$`, backtick, `\`, bracket, brace, `<`, `>`, `#` or unbalanced quote),
//!     because that segment may run the file (`sh x.rs`);
//!   * any other target (`.sh`, extension-less, an unknown extension, a
//!     non-literal word): always judged, as the here-document body is.
//!
//! Judged means the operand goes through every shell rule
//! (`analyze_shell_payload`), Deny over Ask over Allow. An operand whose text
//! is not what the file will hold is an Ask: a double-quoted or bare operand
//! with `$` or a backtick (the writing shell expands it), or a bare operand
//! with a quote or a backslash (the shell unquotes it in ways this scan does
//! not model).

use super::heredoc::{
    here_document_write_targets, is_data_file_target, is_non_shell_code_file_target,
    is_python_file_target, quotes_balance, INERT_LATER_COMMANDS,
};
use super::{
    analyze_shell_payload, command_candidates, here_string_spans, normalized_command,
    quote_aware_words, split_segments, Ctx, Decision, VerdictAcc,
};

/// The verdict on the here-strings `stage` (one pipe stage of `cmd`) writes to
/// a file through `cat` or `tee`; Allow when the stage writes none. See the
/// module documentation for which targets are judged and how.
pub(super) fn written_here_string_verdict(
    cmd: &str,
    stage: &str,
    depth: usize,
    ctx: &Ctx<'_>,
) -> Decision {
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
    let Some(writer) = command_candidates(&tokens)
        .into_iter()
        .map(|idx| normalized_command(tokens[idx]))
        .find(|head| head == "cat" || head == "tee")
    else {
        return Decision::Allow;
    };
    let targets = here_document_write_targets(&remainder);
    if targets.is_empty() || targets.iter().all(|t| is_data_file_target(t)) {
        return Decision::Allow;
    }
    let code_only = targets.iter().all(|t| {
        is_data_file_target(t) || is_python_file_target(t) || is_non_shell_code_file_target(t)
    });
    if code_only && others_are_inert(cmd, stage) {
        return Decision::Allow;
    }
    let shape = format!(
        "`{writer}` writes a here-string to `{}`, and that content is judged as a shell script a \
later command may run",
        targets.join("`, `")
    );
    let mut acc = VerdictAcc::default();
    for span in &spans {
        let unreadable = if span.bare {
            span.operand.contains(['\'', '"', '\\', '$', '`'])
        } else {
            !span.single_quoted && span.operand.contains(['$', '`'])
        };
        let verdict = if unreadable {
            Decision::ask(format!(
                "{shape}; its operand (`{}`) is expanded or unquoted by the writing shell, so \
blastguard cannot see the script the file will hold",
                span.operand
            ))
        } else {
            match analyze_shell_payload(&span.operand, depth, ctx) {
                Decision::Ask(r) => Decision::ask(format!("{shape}: {r}")),
                d => d,
            }
        };
        if let Some(deny) = acc.record(verdict) {
            return deny;
        }
    }
    acc.finish()
}

/// True when every segment of `cmd` other than `stage` is blank or a simple
/// command headed by a word on [`INERT_LATER_COMMANDS`], with none of the
/// characters that could run, substitute or redirect something on it (the
/// later-line rule of [`super::heredoc::non_shell_code_file_here_document`]).
fn others_are_inert(cmd: &str, stage: &str) -> bool {
    let stage = stage.trim();
    split_segments(cmd).iter().all(|seg| {
        let t = seg.trim();
        if t.is_empty() || t == stage {
            return true;
        }
        !t.contains(['$', '`', '\\', '(', ')', '{', '}', '<', '>', '#'])
            && quotes_balance(t)
            && quote_aware_words(t)
                .first()
                .is_some_and(|w| INERT_LATER_COMMANDS.contains(&w.as_str()))
    })
}
