use super::ast::targets;
use super::function_calls::{function_call_reading, FUNCTION_CALL_UNDETERMINED};
use super::shell_lex::{
    bash32_lexed_reading, line_continuation_reading, shell_lexed_reading, Bash32Lexed, ShellLexed,
};
use super::substitution::{
    assuming_comment_hides_quotes, under_bash32_reading, under_bash32_reading_diverged,
};
use super::{
    detect_bash, Ctx, Decision, SafeRoots, VerdictAcc, ANALYSIS_BUDGET, MAX_ANALYSIS_NODES,
};

mod static_bindings;

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
/// A further pass (backlog bdce0261) judges [`function_call_reading`] — the
/// text with every call of a function defined on the line replaced by that
/// function's body at the call site, where bash expands it with the variables
/// in effect at the CALL (the plain reading judges a body, if at all, with the
/// variables of its DEFINITION). Undetermined is Ask.
///
/// When either bash 3.2 pass is undetermined, the answer is at least Ask, and
/// both passes run once more under a candidate parse of the unmodelled
/// comment-with-quote shape ([`bash32_comment_candidate`], issue #1614): a
/// Deny there is returned, anything else leaves the Ask standing.
///
/// D4: each pass starts from a full per-command node budget, so a later
/// reading cannot be starved by an earlier one; budget exhaustion in any pass
/// is that pass's own Ask.
///
/// Every pass runs with the AST index of the (statically resolved) text
/// active ([`targets::with_index`], task `migrate-path`): the path-resolution
/// rules judge a target their string reading cannot place on the values the
/// AST evaluator proved it names. A bash 3.2 pass whose parse differs from
/// the modern one, and the comment-hides-quotes candidate, run without it
/// ([`bash32_pass`]).
///
/// The verdict of all those (legacy) readings is then raised by the Table B.5
/// layer (stage S4, [`super::ast::b5::raise`]): `max(B.5, legacy)`, never
/// lower than the legacy verdict.
///
/// Stage S5 ([`super::ast::s5::prepare`]) runs first: every inline Python
/// program is parsed and its effects judged by this function on their shell
/// spelling (that verdict is folded in like the others), and the legacy
/// readings judge a text in which each program the parser read as Known is
/// `pass` — so they no longer read its string literals as commands. A program
/// the parser cannot read keeps its legacy readings.
///
/// Each call also narrows whether nothing on the derivation chain so far can
/// change what the names a determined producer pipeline uses run
/// ([`chain_names_fixed`], backlog 40dc52c0): the OUTERMOST call (the user's
/// line) sets it to `resolution_fixed(c)`; a re-entry on derived text (the
/// b5 / S5 `judge` re-entries) to the enclosing value AND `resolution_fixed`
/// of its own text, restored on return. Derived text only ever narrows it, so
/// a middle layer that rebinds a name (`bash -c 'PATH=…; bash -c "…"'`)
/// disables static resolution for every layer below it.
pub(super) fn detect_bash_every_reading(c: &str, scope: &SafeRoots) -> Decision {
    struct Restore(Option<bool>);
    impl Drop for Restore {
        fn drop(&mut self) {
            CHAIN_NAMES_FIXED.with(|t| t.set(self.0));
        }
    }
    let outer = CHAIN_NAMES_FIXED.with(std::cell::Cell::get);
    let _restore = Restore(outer);
    let fixed = outer.unwrap_or(true) && super::payload::determined_output::resolution_fixed(c);
    CHAIN_NAMES_FIXED.with(|t| t.set(Some(fixed)));
    // User ruling 2026-11-19 ①: the AST evaluator holds the hook's cwd (the
    // payload cwd) as a known value, so a relative `cd` is resolved.
    super::ast::with_hook_dir(scope.session_cwd(), || every_reading_scoped(c, scope))
}

thread_local! {
    /// `Some(v)` while a Bash detection runs on this thread: `v` is
    /// [`super::payload::determined_output::resolution_fixed`] of the user's
    /// top-level line AND of every derived text [`detect_bash_every_reading`]
    /// was re-entered on around the current call.
    static CHAIN_NAMES_FIXED: std::cell::Cell<Option<bool>> =
        const { std::cell::Cell::new(None) };
}

/// Whether the user's top-level line and every enclosing derived text leave
/// `echo` / `seq` / `cat` / `sort` unrebound (no function definition, no
/// assignment, only allowlisted commands), as narrowed by
/// [`detect_bash_every_reading`]. Outside a Bash detection (a `Ctx` built for
/// Write / Edit, a unit test) it is `false`: no line was read, so nothing is
/// shown to be fixed. Read by [`super::Ctx::new`] and by the Table B.5 layer.
pub(in crate::detect) fn chain_names_fixed() -> bool {
    CHAIN_NAMES_FIXED
        .with(std::cell::Cell::get)
        .unwrap_or(false)
}

fn every_reading_scoped(c: &str, scope: &SafeRoots) -> Decision {
    // Whether a glob on this line may match dot-names (`shopt -s dotglob`,
    // `setopt globdots`, …) is read once, from the outermost line, for every
    // layer and nested reading below (`ast::judge::dot_match`).
    super::ast::with_line_dot_match(c, || every_reading_on_line(c, scope))
}

fn every_reading_on_line(c: &str, scope: &SafeRoots) -> Decision {
    // Backlog 3a950c6c / 5ef2d51f: every reading below — and every rule inside
    // each — judges the text with the line's own literal bindings already
    // resolved (`W=/x; sed -i … $W/f` is judged as `sed -i … /x/f`). One
    // resolver, run once, before anything else; see `static_bindings`.
    let resolved = static_bindings::resolve_static_bindings(c);
    let c: &str = &resolved;
    let judge = |text: &str| detect_bash_every_reading(text, scope);
    // Stage S5 (user rulings 2026-11-17): every inline Python program is read
    // with a real parser and its effects are judged by this same detector on
    // their shell spelling. A program the parser reads as Known is retired
    // from the legacy string readings (they judge `s5.neutral`, where it is
    // `pass`); an Undetermined one keeps them, raise-only.
    let s5 = super::ast::s5::prepare(c, &judge);
    let legacy_text: &str = s5.neutral.as_deref().unwrap_or(c);
    let legacy = targets::with_index(legacy_text, || every_reading(legacy_text, scope));
    // Stage S3b (user ruling 2026-11-18): the AST judge for the commands with
    // a SUBSET.md Table B B.1 row — an unlisted option or an operand the
    // evaluator cannot resolve is Ask (main) / Deny (subagent). Raise-only:
    // a legacy Deny is kept, a legacy Ask keeps its reason, and the judge
    // never lowers the legacy verdict.
    let mut acc = VerdictAcc::default();
    if let Some(deny) = acc.record(legacy) {
        return deny;
    }
    if let Some(deny) = acc.record(s5.verdict) {
        return deny;
    }
    if let Some(deny) = acc.record(super::ast::table_b_verdict(c, scope)) {
        return deny;
    }
    // Backlog 7275b9d9 (user rulings 2026-11-14): the Q3/Q4 deny-target
    // table over the resolved write-target values. Raise-only. An S5 Python
    // effect reaches it through `judge` on its shell spelling.
    if let Some(deny) = acc.record(super::ast::deny_target_verdict(c, scope)) {
        return deny;
    }
    let before_b5 = acc.finish();
    // S4: the standalone Table B.5 layer; it only raises (`max(B.5, …)`).
    super::ast::b5::raise(
        before_b5,
        c,
        &super::ast::b5::Probes {
            judge: &judge,
            scope,
        },
    )
}

/// [`under_bash32_reading`] of `pass`. When that reading closed a construct
/// where the modern scan does not, the text is read differently from the
/// parse the AST index describes, so the pass is judged once more with the
/// index suspended and that verdict is the answer.
fn bash32_pass(pass: impl Fn() -> Decision) -> (Decision, bool) {
    let (verdict, undetermined, diverged) = under_bash32_reading_diverged(&pass);
    if !diverged {
        return (verdict, undetermined);
    }
    ANALYSIS_BUDGET.with(|b| b.set(MAX_ANALYSIS_NODES));
    targets::suspended(|| under_bash32_reading(&pass))
}

fn every_reading(c: &str, scope: &SafeRoots) -> Decision {
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
        // The joined text is the program the shell runs, so it gets EVERY
        // reading the original gets (its function-call reading, its bash 3.2
        // readings, ...), not only the plain one: a reading of the original
        // that splits at a continuation (`git add \` alone, judged as adding
        // nothing; `f() { git add \` / `-A; }; f` inlined unjoined) is only
        // sound because the joined text is judged in full. The recursion
        // ends: what `line_continuation_reading` leaves joined (single
        // quotes, comments, here-document bodies) reads `Same` the second
        // time.
        ShellLexed::Text(joined) => {
            if let Some(deny) = acc.record(every_reading(&joined, scope)) {
                return deny;
            }
        }
    }
    // Backlog bdce0261: a function body judged where it is CALLED, with the
    // variables in effect there. See `function_call_reading`.
    match function_call_reading(c) {
        ShellLexed::Same => {}
        ShellLexed::Undetermined => {
            // An Ask never short-circuits, so there is nothing to return here.
            let _ = acc.record(Decision::ask(FUNCTION_CALL_UNDETERMINED));
        }
        ShellLexed::Text(inlined) => {
            ANALYSIS_BUDGET.with(|b| b.set(MAX_ANALYSIS_NODES));
            if let Some(deny) = acc.record(detect_bash(&inlined, 0, &Ctx::new(scope, &inlined))) {
                return deny;
            }
        }
    }
    if !(c.contains("<<") && (c.contains("$(") || c.contains('`'))) {
        return acc.finish();
    }
    ANALYSIS_BUDGET.with(|b| b.set(MAX_ANALYSIS_NODES));
    let (bash32, mut undetermined) = bash32_pass(|| detect_bash(c, 0, &Ctx::new(scope, c)));
    if let Some(deny) = acc.record(bash32) {
        return deny;
    }
    match bash32_lexed_reading(c) {
        Bash32Lexed::Same => {}
        Bash32Lexed::Undetermined => undetermined = true,
        Bash32Lexed::Text(lexed) => {
            ANALYSIS_BUDGET.with(|b| b.set(MAX_ANALYSIS_NODES));
            let (verdict, lexed_undetermined) =
                bash32_pass(|| detect_bash(&lexed, 0, &Ctx::new(scope, &lexed)));
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
/// `bash -c 'chm\<NL>od …'` runs `chmod` (backlog 040b959b). The payload's
/// [`function_call_reading`] is judged too (backlog bdce0261): the nested
/// shell runs a function body at its call; an undetermined one is Ask.
pub(super) fn shell_payload_verdict(payload: &str, depth: usize, ctx: &Ctx<'_>) -> Decision {
    let mut acc = VerdictAcc::default();
    if let Some(deny) = acc.record(detect_bash(payload, depth + 1, ctx)) {
        return deny;
    }
    // The joined payload gets every reading of this function (its own
    // function-call reading included), as at the top level; the recursion
    // ends because the joined text reads `Same` the second time.
    let joined = match line_continuation_reading(payload) {
        ShellLexed::Same => Decision::Allow,
        ShellLexed::Undetermined => Decision::ask(LINE_CONTINUATION_UNDETERMINED),
        ShellLexed::Text(joined) => shell_payload_verdict(&joined, depth, ctx),
    };
    if let Some(deny) = acc.record(joined) {
        return deny;
    }
    // Backlog bdce0261: the nested shell runs a function body at its call.
    let inlined = match function_call_reading(payload) {
        ShellLexed::Same => Decision::Allow,
        ShellLexed::Undetermined => Decision::ask(FUNCTION_CALL_UNDETERMINED),
        ShellLexed::Text(inlined) => detect_bash(&inlined, depth + 1, ctx),
    };
    if let Some(deny) = acc.record(inlined) {
        return deny;
    }
    acc.finish()
}

/// A SHELL program read from stdin (a here-string or here-document fed to
/// `bash`/`sh`, or a literal piped into one): `texts` followed by the
/// [`line_continuation_reading`] of each text that has one, plus an Ask when
/// one is undetermined. The reading shell removes the program's line
/// continuations before it splits words, whatever quoting delivered the text
/// (`bash <<< 'chm\<NL>od …'` runs `chmod`, backlog 040b959b), so the caller
/// judges every returned text and the most restrictive verdict wins. A text
/// without a line continuation adds nothing.
pub(super) fn shell_program_readings(texts: &[String]) -> (Vec<String>, Option<Decision>) {
    let mut all = texts.to_vec();
    let mut undetermined = None;
    for text in texts {
        match line_continuation_reading(text) {
            ShellLexed::Same => {}
            ShellLexed::Text(joined) => all.push(joined),
            ShellLexed::Undetermined => {
                undetermined = Some(Decision::ask(LINE_CONTINUATION_UNDETERMINED));
            }
        }
    }
    (all, undetermined)
}

/// The text an UNQUOTED here-document body reaches its reader with, as far as
/// backslashes go (POSIX: inside such a body a backslash quotes only `$`,
/// `` ` ``, `\` and newline): `\<LF>` is removed, `\\` `\$` `` \` `` become the
/// second character, any other backslash stays. `None` when that changes
/// nothing. Expansions (`$x`, `$(…)`) are left as spelled — the caller judges
/// them as an expansion it cannot read (backlog 47b20647: a Python program in
/// `python3 - <<EOF` with `'/etc/ho\\<LF>sts'` reaches Python as
/// `'/etc/ho\<LF>sts'`, i.e. `/etc/hosts`).
pub(super) fn unquoted_here_document_reading(body: &str) -> Option<String> {
    if !body.contains('\\') {
        return None;
    }
    let mut out = String::with_capacity(body.len());
    let mut chars = body.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.peek() {
            Some('\n') => {
                chars.next();
            }
            Some(&e @ ('\\' | '$' | '`')) => {
                chars.next();
                out.push(e);
            }
            _ => out.push('\\'),
        }
    }
    (out != body).then_some(out)
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
/// the command is undetermined and records that Ask itself. A candidate parse
/// is not the one the AST index describes, so the index is suspended here.
fn bash32_comment_candidate(c: &str, scope: &SafeRoots, acc: &mut VerdictAcc) -> Option<Decision> {
    targets::suspended(|| {
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
    })
}
