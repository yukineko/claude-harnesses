//! The entry point that judges one code-interpreter invocation
//! ([`analyze_code_interpreter`]): the inline-eval arm (`python3 -c`, `perl -e`,
//! …) and the stdin / substituted-source arm. Kept apart from `interpreter.rs`
//! for the file-size cap; the helpers it calls stay there.

use super::super::{
    here_document_delimiter_is_quoted, here_string_operands, inline_command_payloads,
    interpreter_payloads_after, interpreter_stdin_operand, is_short_flag, opens_here_document,
    process_substitution_payloads, Ctx, Decision,
};
use super::{
    interpreter_code_verdict, interpreter_edits_in_place, interpreter_eval_flag_letters,
    interpreter_inline_eval_pos, osascript_app_delete, protected_operand_ask, python_file,
    quoted_string_literals, strip_version_suffix,
};

/// The operand that makes an interpreter read its program from STDIN rather
/// than from a file. `python3 -` and `python3 /dev/stdin` execute whatever is
/// piped, here-doc'd or here-string'd in — the same capability as `-c`, which
/// is why leaving these at `Allow` was the mirror fail-open: the `-c` shape was
/// denied while every stdin spelling of it went unjudged.
/// Judge a code-interpreter invocation: `python3`, `perl`, `ruby`, `node`,
/// `php`, `lua`.
///
/// Two shapes reach the same capability — running arbitrary code — and before
/// this function only the first was judged at all:
///
/// 1. **Inline eval** (`python3 -c "…"`): was a flat `Deny` on shape, with the
///    program never read. Now the program is audited; see
///    [`interpreter_code_verdict`].
/// 2. **stdin / substituted source** (`python3 -`, `python3 /dev/stdin`,
///    `python3 - <<EOF`, `python3 <<<"…"`, `python3 <(…)`): every one of these
///    was `Allow`. That is the mirror fail-open — the same file already treats
///    `… | python3` as a code-exec sink in [`stage_is_interpreter_terminal`],
///    so it reached opposite conclusions about the same capability.
///
/// Running a script FILE (`python3 app.py`, `python3 -m http.server`) stays
/// `Allow`: the interpreter is not being handed a program on the command line,
/// and the file's contents are outside what a command-line gate can see. The
/// one exception is an in-place edit (`perl -i -p t.pl <file>`,
/// `ruby -i -n s.rb <file>`): its operands are files it rewrites, so they go
/// through the same [`protected_operand_ask`] check as the inline-eval arm, and
/// a protected gate/config operand asks.
pub(in crate::detect) fn analyze_code_interpreter(
    cmd: &str,
    rest: &[&str],
    depth: usize,
    ctx: &Ctx<'_>,
) -> Decision {
    if strip_version_suffix(cmd) == "osascript" {
        if let Some(deny) = osascript_app_delete(rest) {
            return deny;
        }
    }
    if let Some(pos) = interpreter_inline_eval_pos(cmd, rest) {
        let mut payloads = interpreter_payloads_after(rest, pos);
        // Glued forms, where the program is in the SAME token as the flag:
        // short `-c<payload>` and long `--eval=<payload>`.
        let flag = rest[pos];
        if let Some((_, value)) = flag.split_once('=') {
            payloads.extend(inline_command_payloads(rest, pos, value));
        } else if is_short_flag(flag) {
            let letters = interpreter_eval_flag_letters(cmd);
            if let Some(idx) = flag[1..].find(|c: char| letters.contains(c)) {
                payloads.extend(inline_command_payloads(rest, pos, &flag[1 + idx + 1..]));
            }
        }
        let verdict = interpreter_code_verdict(
            &format!("`{cmd}` is invoked with an inline-eval flag"),
            &payloads,
            depth,
            ctx,
            true,
            strip_version_suffix(cmd) == "python",
            false,
        );
        if verdict.is_deny() {
            return verdict;
        }
        // A Python program that does not tokenise (an unterminated string or
        // triple quote, an unbalanced bracket) has no known string/code
        // boundary, so nothing in it can be shown to be data: Ask, never Allow
        // (#1626). Judged on the program word only — the words after it are
        // `sys.argv`, not code.
        if strip_version_suffix(cmd) == "python"
            && payloads
                .first()
                .is_some_and(|p| !python_file::python_program_tokenises(p))
        {
            return Decision::ask(format!(
                "`{cmd}` is invoked with an inline-eval flag, and its program cannot be tokenised \
as Python (an unterminated string or an unbalanced bracket) — blastguard cannot tell its code \
from its strings, so it cannot vouch for it"
            ));
        }
        if !matches!(verdict, Decision::Allow) {
            return verdict;
        }
        // An interpreter is also an EDITOR. `perl -i -pe 's/x//' .githooks/pre-commit`
        // has a harmless program and a gate file for an operand, and until the
        // arm above started allowing benign programs, its blanket Ask was the
        // only thing stopping it.
        if let Some(ask) = protected_operand_ask(cmd, rest, interpreter_edits_in_place(cmd, rest)) {
            return ask;
        }
        return Decision::Allow;
    }

    // `analyze_command_at` is handed TOKENS, not the raw segment, so the
    // operand text is reassembled here. Whitespace-joining is lossless for the
    // purpose of auditing: the here-document body, the here-string and the
    // process-substitution payload all survive as text.
    let text = rest.join(" ");
    let stdin_operand = interpreter_stdin_operand(rest);
    let opens_heredoc = opens_here_document(&text);
    let mut payloads: Vec<String> = here_string_operands(&text)
        .into_iter()
        .map(|(payload, _)| payload)
        .collect();
    // A PROCESS SUBSTITUTION IS A COMMAND, NOT A PROGRAM. `python3 <(cat
    // evil.py)` hands the interpreter a `/dev/fd/…` file whose contents are the
    // OUTPUT of `cat evil.py` — so the substitution's own text says as little
    // about the program as the upstream stage of `cat evil.py | python3` does,
    // which is nothing. Pushing it verbatim made `payloads` non-empty and so
    // walked straight past the empty-set guard below; measured 2026-09-09,
    // `python3 <(cat evil.py)` and `node <(cat evil.js)` reached Allow while
    // their pipe twins asked, and the Deny on the destructive spellings was
    // incidental — it came from analysing the inner `echo`'s literal, not from
    // any reading of the program.
    //
    // Taking the inner command's string LITERALS is the same rule the pipe arm
    // in `analyze_interpreter_stdin_exec` already uses: `<(echo "console.log(1)")`
    // is readable and benign, `<(cat evil.py)` yields nothing and falls to the
    // empty-set Ask. The two mirrors now answer alike.
    let subs = process_substitution_payloads(&text);
    let has_process_substitution = !subs.is_empty();
    for sub in &subs {
        payloads.extend(quoted_string_literals(sub));
    }
    if opens_heredoc {
        // The here-document body is whatever follows the `<<TAG` token; hand
        // the whole operand text to the auditor rather than trying to find the
        // terminator, since a missed terminator must not silence the audit.
        payloads.push(text.clone());
    }

    // An in-place editor is an EDITOR whatever supplies its program. The
    // inline-eval arm above is not the only way to hand `perl -i` / `ruby -i`
    // a program: `perl -i.bak -p t.pl .githooks/pre-commit` reads it from a
    // script FILE and rewrites the protected operand exactly as
    // `perl -i -pe …` does. Before CA-blastguard-01 (2026W39) this shape fell
    // straight through to the script-file `Allow` below with no protected-path
    // check at all, so the gate file was rewritten without ever being asked.
    // Only in-place invocations are judged here: they are the ones whose
    // trailing tokens are file operands with certainty (see
    // [`interpreter_edits_in_place`]).
    let in_place_ask = if interpreter_edits_in_place(cmd, rest) {
        protected_operand_ask(cmd, rest, true)
    } else {
        None
    };

    if stdin_operand.is_none() && !opens_heredoc && !has_process_substitution && payloads.is_empty()
    {
        // A script file, a module, or a bare REPL. Nothing on this command line
        // is a program to audit — but an in-place edit of a protected operand
        // is still a gate-disabling write and asks.
        return in_place_ask.unwrap_or(Decision::Allow);
    }

    let shape = match stdin_operand {
        Some(op) => format!("`{cmd} {op}` reads its program from stdin"),
        None if opens_heredoc => format!("`{cmd}` is fed a here-document"),
        None => format!("`{cmd}` is fed a substituted source"),
    };
    let verdict = interpreter_code_verdict(
        &shape,
        &payloads,
        depth,
        ctx,
        !here_document_delimiter_is_quoted(&text),
        false,
        false,
    );
    if matches!(verdict, Decision::Allow) {
        if let Some(ask) = in_place_ask {
            return ask;
        }
    }
    verdict
}
