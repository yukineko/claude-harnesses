use super::shell_lex::{
    bash32_lexed_reading, line_continuation_reading, shell_lexed_reading, Bash32Lexed, ShellLexed,
};
use super::substitution::{assuming_comment_hides_quotes, under_bash32_reading};
use super::{
    detect_bash, Ctx, Decision, SafeRoots, VerdictAcc, ANALYSIS_BUDGET, MAX_ANALYSIS_NODES,
};

/// The top-level Bash analysis: [`detect_bash`] under the modern reading and,
/// when the command could be parsed differently by macOS `/bin/bash` 3.2 /
/// `/bin/sh`, again under [`under_bash32_reading`], the MOST restrictive
/// verdict winning (`Deny > Ask > Allow`, via [`VerdictAcc`]).
///
/// The two readings differ for a here-document inside a command substitution
/// (bash 3.2 reads the body as command text, so a `)` in it can close the
/// substitution and run what follows), so the second pass runs only when the
/// text holds `<<` together with `$(` or a backtick. Where bash 3.2 provably
/// closes a substitution elsewhere, the second pass analyses that parse. Where
/// bash 3.2 provably runs out of input (a syntax error: that command and
/// everything after it never run), it analyses the modern end again, which is
/// no less than the first pass. Where the bash 3.2 parse is not modelled, the
/// answer is at least Ask: "cannot determine" is never folded into Allow.
///
/// A third pass (backlog e2b9ffb1) judges the SHELL-LEXED spelling of the
/// text — `#` comments removed, backslash-escaped quotes spelt `_` — whenever
/// it differs from the text, because the splitters track quotes without
/// knowing comments (and the other line-level quote trackers know neither
/// comments nor escapes), and a quote in a comment or an escaped quote hid
/// the next line from them. The same pass spells the literal `'`/`"` of
/// an UNQUOTED here-document body `_` and lexes its `$(...)`/backtick spans as
/// the live code they are, so a stray quote in such a body no longer hides a
/// later substitution (backlog 5916d385); an unquoted body whose expansion the
/// lexer does not model is Ask. See [`shell_lexed_reading`].
///
/// A further pass (backlog 040b959b) judges [`line_continuation_reading`] —
/// the text with only its line continuations (backslash-newline) removed where
/// the shell removes them — whenever it differs from the text: every splitter
/// splits at the newline of such a pair, which hid a command word or operand
/// cut in two by it. The shell-lexed pass above removes them too, but it also
/// re-spells escaped quotes, which a backslash-escaped program operand needs
/// intact. Undetermined is Ask.
///
/// A fourth pass (backlog 089bedd4) does the same for the bash 3.2 reading:
/// where bash 3.2 closes a substitution early at a here-document body `)`,
/// the text after that close is top-level code, and the naive trackers again
/// read a quote in its comment (or an escaped quote) as opening a span that
/// hid the next line. [`bash32_lexed_reading`] spells the command the way
/// bash 3.2 tokenises it, and that text is judged under
/// [`under_bash32_reading`] too; a shape that lexer does not model is Ask.
///
/// When either bash 3.2 pass is undetermined, the answer is at least Ask, and
/// both passes run once more under a candidate parse of the unmodelled
/// comment-with-quote shape ([`bash32_comment_candidate`], issue #1614): a
/// Deny there is returned, anything else leaves the Ask standing.
///
/// D4: each pass starts from a full per-command node budget, so a later
/// reading cannot be starved by an earlier one; budget exhaustion in any pass
/// is that pass's own Ask.
pub(super) fn detect_bash_every_reading(c: &str, scope: &SafeRoots) -> Decision {
    ANALYSIS_BUDGET.with(|b| b.set(MAX_ANALYSIS_NODES));
    let mut acc = VerdictAcc::default();
    if let Some(deny) = acc.record(detect_bash(c, 0, &Ctx::new(scope, c))) {
        return deny;
    }
    match shell_lexed_reading(c) {
        ShellLexed::Same => {}
        ShellLexed::Undetermined => {
            // An Ask never short-circuits, so there is nothing to return here.
            let _ = acc.record(Decision::ask(
                "the body of an unquoted here-document (`<<EOF`) is expanded by the shell — \
                 its `$(...)` and backtick spans run as commands — and blastguard cannot \
                 determine how this one expands, so it cannot vouch for it either way",
            ));
        }
        ShellLexed::Text(lexed) => {
            ANALYSIS_BUDGET.with(|b| b.set(MAX_ANALYSIS_NODES));
            if let Some(deny) = acc.record(detect_bash(&lexed, 0, &Ctx::new(scope, &lexed))) {
                return deny;
            }
        }
    }
    match line_continuation_reading(c) {
        ShellLexed::Same => {}
        ShellLexed::Undetermined => {
            // An Ask never short-circuits, so there is nothing to return here.
            let _ = acc.record(Decision::ask(LINE_CONTINUATION_UNDETERMINED));
        }
        ShellLexed::Text(joined) => {
            ANALYSIS_BUDGET.with(|b| b.set(MAX_ANALYSIS_NODES));
            if let Some(deny) = acc.record(detect_bash(&joined, 0, &Ctx::new(scope, &joined))) {
                return deny;
            }
        }
    }
    if !(c.contains("<<") && (c.contains("$(") || c.contains('`'))) {
        return acc.finish();
    }
    ANALYSIS_BUDGET.with(|b| b.set(MAX_ANALYSIS_NODES));
    let (bash32, mut undetermined) =
        under_bash32_reading(|| detect_bash(c, 0, &Ctx::new(scope, c)));
    if let Some(deny) = acc.record(bash32) {
        return deny;
    }
    match bash32_lexed_reading(c) {
        Bash32Lexed::Same => {}
        Bash32Lexed::Undetermined => undetermined = true,
        Bash32Lexed::Text(lexed) => {
            ANALYSIS_BUDGET.with(|b| b.set(MAX_ANALYSIS_NODES));
            let (verdict, lexed_undetermined) =
                under_bash32_reading(|| detect_bash(&lexed, 0, &Ctx::new(scope, &lexed)));
            if let Some(deny) = acc.record(verdict) {
                return deny;
            }
            undetermined |= lexed_undetermined;
        }
    }
    if undetermined {
        // Issue #1614: an undetermined bash 3.2 parse is Ask, but a candidate
        // parse of it may expose a harmful command; that is Deny, the more
        // restrictive answer. The candidate's own verdict only adds — the Ask
        // below is recorded whatever it says.
        if let Some(deny) = bash32_comment_candidate(c, scope, &mut acc) {
            return deny;
        }
        // An Ask never short-circuits, so there is nothing to return here.
        let _ = acc.record(Decision::ask(
            "macOS /bin/bash 3.2 and /bin/sh read a here-document inside `$(...)` as command \
             text (a `)` in its body can end the substitution early and run what follows), \
             and blastguard cannot determine how bash 3.2 parses this one — it cannot vouch \
             for it either way",
        ));
    }
    acc.finish()
}

/// [`detect_bash`] on a shell payload (the code a nested shell parses —
/// `bash -c '…'`, `eval`, a program's `os.system(…)` string) and on its
/// [`line_continuation_reading`], the more restrictive verdict winning
/// (`Deny > Ask > Allow`; an undetermined reading is Ask). The nested shell
/// removes the payload's line continuations before it splits words, and the
/// outer single quotes that kept them literal are gone by then:
/// `bash -c 'chm\<NL>od …'` runs `chmod` (backlog 040b959b).
pub(super) fn shell_payload_verdict(payload: &str, depth: usize, ctx: &Ctx<'_>) -> Decision {
    let mut acc = VerdictAcc::default();
    if let Some(deny) = acc.record(detect_bash(payload, depth + 1, ctx)) {
        return deny;
    }
    let joined = match line_continuation_reading(payload) {
        ShellLexed::Same => Decision::Allow,
        ShellLexed::Undetermined => Decision::ask(LINE_CONTINUATION_UNDETERMINED),
        ShellLexed::Text(joined) => detect_bash(&joined, depth + 1, ctx),
    };
    if let Some(deny) = acc.record(joined) {
        return deny;
    }
    acc.finish()
}

/// Reason of the Ask for a [`line_continuation_reading`] that is undetermined.
const LINE_CONTINUATION_UNDETERMINED: &str =
    "the shell removes a backslash-newline (line continuation) before it reads the words, \
     and blastguard cannot determine how this command reads with them removed (a \
     here-document whose lines it joins, or nesting too deep) — it cannot vouch for it \
     either way";

/// The bash 3.2 passes of [`detect_bash_every_reading`] (plain and lexed)
/// re-run under [`assuming_comment_hides_quotes`]: a comment whose line holds
/// a quote is read as hiding it (observed to run what follows the early close
/// in D1-D3 of `tests/heredoc_paren_bash32_verifier_20261107.rs`). Each
/// verdict is recorded into `acc`; a Deny is returned. The passes' own
/// undetermined flags are ignored on purpose: the caller has already decided
/// the command is undetermined and records that Ask itself.
fn bash32_comment_candidate(c: &str, scope: &SafeRoots, acc: &mut VerdictAcc) -> Option<Decision> {
    assuming_comment_hides_quotes(|| {
        ANALYSIS_BUDGET.with(|b| b.set(MAX_ANALYSIS_NODES));
        let (plain, _) = under_bash32_reading(|| detect_bash(c, 0, &Ctx::new(scope, c)));
        if let Some(deny) = acc.record(plain) {
            return Some(deny);
        }
        if let Bash32Lexed::Text(lexed) = bash32_lexed_reading(c) {
            ANALYSIS_BUDGET.with(|b| b.set(MAX_ANALYSIS_NODES));
            let (verdict, _) =
                under_bash32_reading(|| detect_bash(&lexed, 0, &Ctx::new(scope, &lexed)));
            if let Some(deny) = acc.record(verdict) {
                return Some(deny);
            }
        }
        None
    })
}
