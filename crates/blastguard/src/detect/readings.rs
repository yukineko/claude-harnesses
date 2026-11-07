use super::shell_lex::shell_lexed_reading;
use super::substitution::under_bash32_reading;
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
/// knowing comments or escapes, and a quote in a comment or an escaped quote
/// hid the next line from them. See [`shell_lexed_reading`].
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
    if let Some(lexed) = shell_lexed_reading(c) {
        ANALYSIS_BUDGET.with(|b| b.set(MAX_ANALYSIS_NODES));
        if let Some(deny) = acc.record(detect_bash(&lexed, 0, &Ctx::new(scope, &lexed))) {
            return deny;
        }
    }
    if !(c.contains("<<") && (c.contains("$(") || c.contains('`'))) {
        return acc.finish();
    }
    ANALYSIS_BUDGET.with(|b| b.set(MAX_ANALYSIS_NODES));
    let (bash32, undetermined) = under_bash32_reading(|| detect_bash(c, 0, &Ctx::new(scope, c)));
    if let Some(deny) = acc.record(bash32) {
        return deny;
    }
    if undetermined {
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
