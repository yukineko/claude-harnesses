use super::{
    assignment_execution_is_unconditional, assignments_reach_parent_shell, command_candidates,
    here_document_layout, is_assignment, normalized_command, split_segments,
    split_segments_with_separators,
};

// ---------------------------------------------------------------------------
// Reading an expansion-valued COMMAND WORD back off the same command line.
//
// WIRED INTO EXACTLY ONE DECISION ARM: the CA-blastguard-017 head check in
// [`unknown_wrapper_ask`]. [`detect_bash`] enumerates its `split_segments(cmd)`
// loop and threads `(cmd, seg_idx)` through [`analyze_segment`] to that arm.
// No other rule in this file consults anything below.
//
// That arm uses the answer SUBSTITUTIVELY, never as a skip: a
// [`CommandWordOrigin::Literal`] replaces the expansion token and the rewritten
// segment is re-judged by the full rule engine, so a head that resolves to a
// destructive program comes out DENY. Merely suppressing the ask on finding a
// literal would re-open CA-blastguard-017 in a new place — the tail alone
// (`-rf /path`) is a flag and a bare path, which does not parse as destructive.
// ---------------------------------------------------------------------------

/// What the TEXT of a command line says about the program that a command word
/// which is itself an expansion (`$BIN`, `${BIN}`, `"$BIN"`) will run.
///
/// Three answers, not two — the same reason `Decision::Ask` exists (see
/// `model.rs`): an `Option<String>` would collapse "nothing on this line
/// assigns that name" and "something assigns it, but to text I cannot read"
/// into one `None`, and a caller holding that `None` cannot tell "I looked and
/// there is nothing" from "I looked and could not read it". Only
/// [`CommandWordOrigin::Literal`] carries a program name; the other two say, in
/// two different ways, that the text does not name a program.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum CommandWordOrigin {
    /// (1) An assignment that reaches this use gives the name this literal
    /// text; the text contains no expansion of its own; and the line's own
    /// structure says that assignment RAN.
    ///
    /// That last clause is why an assignment guarded by `&&`/`||` does not
    /// arrive here. An earlier revision reported one as a literal and recorded,
    /// as a KNOWN LIMIT, that the variant therefore meant "IF the assignment
    /// ran, the program is this text". That conditional is the whole problem:
    /// it makes the only permissive answer this enum has depend on an exit
    /// status that no amount of reading the line recovers. The condition is now
    /// checked instead of documented ([`assignment_execution_is_unconditional`]),
    /// so this variant means what it appears to mean — the program is this text.
    Literal(String),
    /// (2) No assignment that reaches this use gives the name a value — either
    /// nothing on the line assigns it before the use in a position whose
    /// assignment survives, or the word is not a plain single-variable
    /// reference at all (`$(which foo)`, `${BIN:-x}`, `pre$BIN`, `$1`), so
    /// there is no name to look up.
    NoReachingAssignment,
    /// (3) An assignment DOES reach this use, but the text still does not name
    /// a program. Every way that happens:
    ///   * its execution is guarded — by a `&&`/`||`, or by a compound command
    ///     or subshell opened earlier on the line — so whether it ran in THIS
    ///     shell is a run-time fact the text does not contain (see
    ///     [`assignment_execution_is_unconditional`]). This case differs from
    ///     the others below in that the VALUE may be perfectly readable; the
    ///     line simply names two programs — the assigned one and whatever the
    ///     environment already held — and choosing between them would be a
    ///     guess. It is reported here rather than as
    ///     [`CommandWordOrigin::NoReachingAssignment`] because both are
    ///     restrictive but only this one is true: the assignment is not absent
    ///     from the line, and a caller told "there is nothing" would state
    ///     something the text contradicts.
    ///   * it is a here-document BODY rather than code
    ///     ([`opens_here_document`]). The same shape as the guarded case, and
    ///     worse: a `NAME=value` typed into a body never ran at all.
    ///   * something AFTER it rebinds the name in a spelling the scan cannot
    ///     read ([`segment_may_rebind`]) — `export BIN=…`, `{ BIN=…; }`,
    ///     `BIN=… # comment`, `for BIN in …`, `BIN+=…`, `unset`, `eval`,
    ///     `source`. The readable assignment is then not the current value but
    ///     a stale one, which is the same defect as the guarded case wearing a
    ///     different hat: the text names two programs.
    ///   * the right-hand side contains an expansion of its own
    ///     (`BIN=$(which foo)`, `` BIN=`which foo` ``);
    ///   * it is an ARRAY (`BIN=(/bin/rm)`). `$BIN` expands to the first
    ///     element, and picking that element out would depend on `IFS`, on
    ///     `[0]`-vs-`[@]` and on whether the use is quoted — none of which this
    ///     enum's producer models. The parenthesised text itself is not a
    ///     program name.
    ///   * its quoting does not balance, or it contains a backslash, neither of
    ///     which [`unquote_literal_value`] resolves to one literal;
    ///   * it is empty (`X=`), for which the word expands to nothing and some
    ///     LATER token becomes the program — a token this function was not
    ///     asked about.
    ///
    /// NOT covered, and stated because a reader would otherwise assume it is:
    /// a call to a shell FUNCTION defined off this line (`BIN=/bin/echo;
    /// myfunc; $BIN …`). A function call is textually identical to running an
    /// external command, which cannot touch the parent shell's variables, so
    /// separating them needs knowledge this file does not have. A function
    /// DEFINED on the same line is covered, via the assignment its body carries.
    AssignedButUnknowable,
}

/// True when this segment opens a compound command whose body may run zero
/// times, or may run in a child shell — either way, a later assignment is not
/// something the line's text says happened in THIS shell.
///
/// Only the OPENING keywords are listed. The closing ones (`fi`, `done`,
/// `esac`) need no entry: a segment cannot be inside a construct whose opener
/// this scan did not already pass over.
///
/// This is a first-word test, so the keywords are recognised only where a shell
/// would treat them as keywords. `echo if` and `grep -q done` are ordinary
/// commands and are not flagged.
pub(super) fn opens_unmodelled_construct(seg: &str) -> bool {
    let Some(first) = seg.split_whitespace().next() else {
        return false;
    };
    // A subshell's `(` may be glued to the first command word inside it
    // (`(cd x`), so this is a prefix test rather than an equality one. A brace
    // group (`{`) is deliberately absent: it runs in the current shell,
    // unconditionally, so an assignment inside one really does reach later
    // segments.
    if first.starts_with('(') {
        return true;
    }
    matches!(
        first,
        "if" | "then" | "elif" | "else" | "while" | "until" | "for" | "do" | "case" | "select"
    )
}

/// Split a segment into words on whitespace that is OUTSIDE quotes, backticks
/// and `(…)`, keeping those characters in the word.
///
/// `split_whitespace` cannot be used for this, and neither can a quote-only
/// scan. Both break a word that contains a space inside a construct:
///   * `BIN="/a b/tool"` → `BIN="/a` + `b/tool"` under `split_whitespace`;
///   * `BIN=$(which foo)` → `BIN=$(which` + `foo)` under a quote-only scan,
///     since parentheses are not quotes.
///
/// In each case the segment stops looking like a pure assignment list, and the
/// caller then ignores an assignment that really is there. That is restrictive,
/// not permissive — but it answers `NoReachingAssignment` for a name the line
/// visibly DOES assign, which is the wrong one of the two restrictive answers.
pub(super) fn quote_aware_words(seg: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let (mut in_s, mut in_d, mut in_backtick) = (false, false, false);
    let mut paren_depth: u32 = 0;
    let mut chars = seg.chars();
    while let Some(c) = chars.next() {
        if c == '\\' && !in_s {
            // Keep BOTH characters: the escape is not interpreted here, it only
            // stops the escaped character from ending the word or toggling a
            // quote. `unquote_literal_value` refuses any value containing one.
            cur.push(c);
            if let Some(next) = chars.next() {
                cur.push(next);
            }
            continue;
        }
        if c == '\'' && !in_d && !in_backtick {
            in_s = !in_s;
            cur.push(c);
            continue;
        }
        if c == '"' && !in_s && !in_backtick {
            in_d = !in_d;
            cur.push(c);
            continue;
        }
        if c == '`' && !in_s {
            in_backtick = !in_backtick;
            cur.push(c);
            continue;
        }
        if !in_s && !in_d && !in_backtick {
            if c == '(' {
                paren_depth += 1;
                cur.push(c);
                continue;
            }
            if c == ')' {
                paren_depth = paren_depth.saturating_sub(1);
                cur.push(c);
                continue;
            }
            if c.is_whitespace() && paren_depth == 0 {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
                continue;
            }
        }
        cur.push(c);
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// The literal text a quoted right-hand side stands for, or `None` when the
/// text does not determine one.
///
/// Refuses (returns `None` for):
///   * any value containing a backslash. Backslash means three different things
///     depending on context — `X=a\b` is `ab`, `X="a\b"` is `a\b`, `X='a\b'` is
///     `a\b` — and modelling three rules to gain a rare case is a bad trade for
///     a function whose `Some` is the only permissive answer. This costs a
///     resolution it could have made; it never invents one.
///   * unbalanced quoting, which the shell would not run as written anyway.
pub(super) fn unquote_literal_value(raw: &str) -> Option<String> {
    if raw.contains('\\') {
        return None;
    }
    let mut out = String::new();
    let (mut in_s, mut in_d) = (false, false);
    for c in raw.chars() {
        if c == '\'' && !in_d {
            in_s = !in_s;
            continue;
        }
        if c == '"' && !in_s {
            in_d = !in_d;
            continue;
        }
        out.push(c);
    }
    if in_s || in_d {
        return None;
    }
    Some(out)
}

/// True when `word` gives `name` a new value: the plain `NAME=value` spelling
/// and bash's append form `NAME+=value`.
///
/// The append form matters because [`is_assignment`] rejects it — the character
/// before the `=` is not alphanumeric — so a segment consisting only of
/// `BIN+=/rm` is not a "pure assignment list" and was skipped entirely. Skipped
/// is not harmless here: it leaves an EARLIER assignment standing as the
/// answer. Measured: `BIN=<dir>; BIN+=/progB; $BIN marker` prints `PROG_B`, so
/// the program really is the concatenation and not the half the resolver read.
pub(super) fn word_binds_name(word: &str, name: &str) -> bool {
    match word.strip_prefix(name) {
        Some(rest) => rest.starts_with('=') || rest.starts_with("+="),
        None => false,
    }
}

/// Builtins that bind a variable whose NAME is given as a bare operand rather
/// than through an `=`, so [`word_binds_name`] alone cannot see them.
///
/// The `=`-taking declaration builtins (`export`, `declare`, …) are here too:
/// `export BIN=x` is caught by `word_binds_name` on its second word, but
/// `export BIN` with a separate assignment is not, and listing them costs
/// nothing.
pub(super) fn binds_name_as_operand(head: &str) -> bool {
    matches!(
        head,
        "unset"
            | "read"
            | "readarray"
            | "mapfile"
            | "printf"
            | "getopts"
            | "let"
            | "for"
            | "select"
            | "declare"
            | "typeset"
            | "local"
            | "export"
            | "readonly"
    )
}

/// True when this segment may give `name` a value that
/// [`resolve_expanded_command_word`]'s pure-assignment-list scan does not
/// record — so a literal read from an EARLIER segment is stale, not current.
///
/// Only called for a segment that is NOT a pure assignment list. The scan used
/// to `continue` past those silently, on the reading that ignoring a segment can
/// only lose a resolution. That reading is wrong whenever another assignment to
/// the same name exists: ignoring the second one does not leave the resolver
/// with nothing, it leaves it holding the first one and calling it the program.
/// Every shape below was run through `bash -c` with two marker programs, and in
/// each the program that ran was the one the scan discarded:
///
///   * `export`/`declare`/`readonly`/`typeset`/`local BIN=…` — `PROG_B`;
///   * a brace group, `{ BIN=…; }` — `PROG_B` (a brace group runs in the
///     current shell, which is why [`opens_unmodelled_construct`] rightly
///     leaves `{` off its list; the consequence for THIS scan is the opposite
///     of harmless);
///   * a trailing comment, `BIN=… # switch` — `PROG_B`. `#` starts a comment,
///     so the segment is a bare assignment list and the value PERSISTS. Note
///     this is the exact opposite of the transient command-prefix shape
///     `BIN=… cmd`, which a whitespace splitter cannot tell it from;
///   * `for BIN in …` — `PROG_B`, the control variable outlives the loop;
///   * `eval`/`source`/`.` — `PROG_B`. These need no mention of the name at
///     all, which is why they are matched on the HEAD;
///   * `unset BIN` — `marker: command not found`, i.e. the word expanded to
///     nothing and some later token became the program;
///   * `read BIN` and `printf -v BIN` — `PROG_B`.
///
/// Answering true only makes the caller refuse to name a program, so the
/// coarseness here (a `printf` mentioning `BIN` anywhere, a `local` at top
/// level where bash would error) costs resolutions and never invents one.
pub(super) fn segment_may_rebind(seg: &str, name: &str) -> bool {
    let words = quote_aware_words(seg);
    let Some(first) = words.first() else {
        return false;
    };
    let head = normalized_command(first);
    if matches!(head.as_str(), "eval" | "source" | ".") {
        return true;
    }
    if binds_name_as_operand(&head)
        && words[1..].iter().any(|w| {
            // Quote removal applies here and NOT to the general test below,
            // because the two spellings behave differently. `export "BIN=x"`
            // and `export 'BIN'=x` both really do assign — measured, both print
            // `PROG_B` — since the quotes are removed before `export` sees its
            // argument. A quoted word standing ALONE does not: `"BIN=x"` is a
            // command word, and bash reports `BIN=x: No such file or directory`
            // while `BIN` keeps its old value.
            let plain = unquote_literal_value(w).unwrap_or_else(|| w.to_string());
            plain == name || word_binds_name(&plain, name)
        })
    {
        return true;
    }
    // A LEADING run of assignments is a command prefix — transient — but only
    // when a command really follows it. Anything binding the name outside that
    // run (`export BIN=x`, `{ BIN=x`, `then BIN=x`) is not a prefix.
    let lead = words.iter().take_while(|w| is_assignment(w)).count();
    if words[lead..].iter().any(|w| word_binds_name(w, name)) {
        return true;
    }
    // …and if what follows the run is a `#` comment (or nothing), there is no
    // command for the prefix to be transient FOR: the shell sees a bare
    // assignment list and the value persists.
    let followed_by_command = words
        .get(lead)
        .map(|w| !w.starts_with('#'))
        .unwrap_or(false);
    if !followed_by_command && words[..lead].iter().any(|w| word_binds_name(w, name)) {
        return true;
    }
    false
}

/// True for a name a shell would accept on the left of `=`.
pub(super) fn is_shell_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// The variable name a command word REFERS to, when the word is a plain single
/// reference and nothing else.
///
/// Accepts `$NAME`, `${NAME}` and the double-quoted forms `"$NAME"` /
/// `"${NAME}"`, which expand identically.
///
/// Returns `None` — never a name — for everything else, including:
///   * a backslash-escaped dollar (`\$FOO`, `"\$FOO"`). This agrees with
///     [`has_unresolvable_expansion`]'s own escape handling, where `\` makes the
///     next character literal, so `\$FOO` is a literal dollar and not a
///     reference at all. There is no separate backslash check below because
///     none is needed: a `\` before the `$` fails the prefix match, and a `\`
///     anywhere in the name fails [`is_shell_identifier`]. (Measured — adding
///     an early `word.contains('\\')` return changes no test in this section.)
///   * a single-quoted word (`'$FOO'`), which the shell does not expand.
///   * command substitution (`$(…)`, backticks), expansion modifiers
///     (`${BIN:-x}`), positional/special parameters (`$1`, `$@`), and any word
///     with text glued around the reference (`pre$BIN`, `$A$B`) — in each case
///     the value is either not a variable or not just a variable.
pub(super) fn referenced_variable_name(word: &str) -> Option<String> {
    let inner = if word.len() >= 2 && word.starts_with('"') && word.ends_with('"') {
        let mid = &word[1..word.len() - 1];
        if mid.contains('"') {
            return None;
        }
        mid
    } else {
        word
    };
    let name = match inner.strip_prefix("${") {
        Some(braced) => braced.strip_suffix('}')?,
        None => inner.strip_prefix('$')?,
    };
    if is_shell_identifier(name) {
        Some(name.to_string())
    } else {
        None
    }
}

/// Answer, for a command word that [`has_unresolvable_expansion`] flagged,
/// which of [`CommandWordOrigin`]'s three things the command line's own text
/// says.
///
/// `seg_idx` is the index of `word`'s segment in `split_segments(cmd)`, and
/// `word` is the RAW token (quotes included), for the same reason
/// `unresolvable_command_word` compares the raw token: one level of quoting is
/// stripped elsewhere and `"$CMD"` must still read as a reference.
///
/// An assignment counts only when ALL of the following hold, each of which was
/// measured against `bash -c` rather than assumed:
///
///   * it is in a segment STRICTLY BEFORE `seg_idx`. A same-segment assignment
///     is a command PREFIX, and the shell expands the word before applying the
///     prefix: `bash -c 'RM=/bin/echo $RM prefixtest'` reports
///     `bash: prefixtest: command not found`, i.e. `$RM` expanded to the empty
///     string and never to `/bin/echo`; with an earlier `RM=/bin/cat` in scope,
///     `RM=/bin/echo $RM prefix2` runs `cat`, the OLD value.
///   * its segment is executed by the parent shell
///     ([`assignments_reach_parent_shell`]).
///   * its segment is a pure assignment list — every word in it is a
///     `NAME=value`. A `NAME=value` in front of a COMMAND is transient:
///     `bash -c 'RM=/bin/echo true; echo "[$RM]"'` prints `[]`, and `/bin/sh`
///     agrees. Reading such a prefix as a lasting assignment would be exactly
///     the permissive error this function exists to avoid.
///
/// The LAST such assignment wins, since each later one overwrites the earlier.
///
/// One further condition applies to that winner alone, and it is about
/// EXECUTION rather than scope: no `&&`/`||` may stand before it
/// ([`assignment_execution_is_unconditional`]). A guarded winner answers
/// `AssignedButUnknowable` however readable its value is — including when an
/// earlier UNGUARDED assignment to the same name exists, because then the two
/// branches genuinely run two different programs and the text does not say
/// which. Note the position this tests: a conditional between the winning
/// assignment and the USE is irrelevant and is not tested, which is what keeps
/// the corpus's dominant `BIN=…` / `cd … && "$BIN" …` shape resolvable.
///
/// Values whose quoting does not balance or that contain a backslash are
/// deliberately not resolved (see [`unquote_literal_value`], which refuses
/// both); that costs a resolution and never invents one.
///
/// A segment that is NOT a pure assignment list is not simply skipped. An
/// earlier revision of this comment said `export NAME=value` / `declare` /
/// `local` were "ignored, so the answer stays `NoReachingAssignment`", which
/// was only true of a line that assigns the name NOWHERE else. With any other
/// assignment present, ignoring the second one does not leave this function
/// with nothing to say — it leaves it holding the FIRST value and reporting it
/// as the program, and measurement says that is the wrong one:
/// `BIN=A; export BIN=B; $BIN marker` runs `B`. Such a segment is now tested by
/// [`segment_may_rebind`], and one standing after the winning assignment makes
/// the answer `AssignedButUnknowable`.
///
/// How the caller gets `seg_idx`: [`detect_bash`] enumerates its
/// `for seg in split_segments(cmd)` loop, which produces exactly the index this
/// parameter means, and threads `(cmd, seg_idx)` through [`analyze_segment`] to
/// [`unknown_wrapper_ask`]. That index survives the trip because
/// [`advance_cwd_and_rewrite`] maps one segment to one segment — it rewrites a
/// segment's TEXT but never splits or merges one, so the loop position and the
/// `split_segments` position stay the same number.
///
/// [`analyze_shell_payload`] is deliberately NOT wired to this:
/// `unresolvable_command_word` walks `split_segments(payload)` itself and keeps
/// its pre-existing Ask.
pub(super) fn resolve_expanded_command_word(
    cmd: &str,
    seg_idx: usize,
    word: &str,
) -> CommandWordOrigin {
    let Some(name) = referenced_variable_name(word) else {
        return CommandWordOrigin::NoReachingAssignment;
    };
    let segs = split_segments_with_separators(cmd);
    if seg_idx >= segs.len() {
        return CommandWordOrigin::NoReachingAssignment;
    }
    // The winning assignment's SEGMENT INDEX is carried alongside its value:
    // whether the value may be believed depends on where the assignment sits,
    // so keeping only the text would discard the deciding fact.
    let mut latest: Option<(usize, String)> = None;
    // The last segment that may give `name` a value in a shape this scan does
    // NOT record (see `segment_may_rebind`). Skipping such a segment is only
    // harmless when there is nothing else to report; when a readable assignment
    // exists too, skipping means answering with a value the shell has since
    // replaced.
    let mut latest_rebind: Option<usize> = None;
    // A here-document BODY is data the shell never executes, so a `NAME=value`
    // typed inside one neither assigns nor rebinds. Skipping it here is what
    // lets the closed-here-document case above be believed at all: the reason
    // to distrust the tail was the body, and the body is now identified.
    let (is_here_doc_body, _) = here_document_layout(&segs);
    for idx in 0..seg_idx {
        if is_here_doc_body.get(idx).copied().unwrap_or(false) {
            continue;
        }
        if !assignments_reach_parent_shell(&segs, idx) {
            continue;
        }
        let words = quote_aware_words(&segs[idx].text);
        if words.is_empty() || !words.iter().all(|w| is_assignment(w)) {
            if segment_may_rebind(&segs[idx].text, &name) {
                latest_rebind = Some(idx);
            }
            continue;
        }
        for w in &words {
            let Some(eq) = w.find('=') else {
                continue;
            };
            if w[..eq] == name {
                latest = Some((idx, w[eq + 1..].to_string()));
            }
        }
    }
    let Some((assign_idx, raw_value)) = latest else {
        return CommandWordOrigin::NoReachingAssignment;
    };
    if latest_rebind.map(|r| r > assign_idx).unwrap_or(false) {
        // Something after the winning assignment rebinds the name in a way the
        // scan above cannot read. The assignment is not absent and its text is
        // perfectly readable — it simply is not the current value any more, so
        // this is `AssignedButUnknowable` for the same reason a guarded
        // assignment is. A rebinding BEFORE the winner is irrelevant: the
        // winner overwrote it, which is what "the last one wins" means.
        return CommandWordOrigin::AssignedButUnknowable;
    }
    if raw_value.starts_with('(') {
        // An ARRAY assignment. `quote_aware_words` tracks parentheses, so
        // `BIN=(/bin/rm)` arrives here in one piece and `unquote_literal_value`
        // would hand back the text `(/bin/rm)` — which is not a program name.
        // The rewritten head `(/bin/rm)` has basename `rm)`, matches no rule
        // arm, and reaches the catch-all Allow with the program unexamined.
        // Measured: `BIN=(B); $BIN marker` prints `PROG_B`, so `$BIN` really
        // does expand to the first element and the program really does run.
        // Reading the first element out is not attempted — the value would then
        // depend on `IFS`, on the `[0]`-vs-`[@]` distinction and on whether the
        // use is quoted, none of which this function models.
        return CommandWordOrigin::AssignedButUnknowable;
    }
    if !assignment_execution_is_unconditional(&segs, assign_idx) {
        // An assignment IS here and its text may even be perfectly readable —
        // what is missing is whether it ran, so the line names two programs and
        // this function may not pick one. `AssignedButUnknowable`, not
        // `NoReachingAssignment`: the assignment is not absent, it is
        // unbelievable.
        return CommandWordOrigin::AssignedButUnknowable;
    }
    if has_unresolvable_expansion(&raw_value) {
        return CommandWordOrigin::AssignedButUnknowable;
    }
    let Some(value) = unquote_literal_value(&raw_value) else {
        return CommandWordOrigin::AssignedButUnknowable;
    };
    if value.is_empty() {
        // `X=; $X foo` runs `foo`: the word expands to nothing and DISAPPEARS,
        // so the program is a token this function was never asked about.
        // Verified: `bash -c 'X=; $X echo vanished'` prints `vanished` and exits
        // 0. An empty program name is not a program, and answering `Literal("")`
        // would hand a caller a program name for a command whose program it has
        // not seen — so the empty value resolves to the restrictive side.
        return CommandWordOrigin::AssignedButUnknowable;
    }
    CommandWordOrigin::Literal(value)
}

/// Shell metacharacters whose value is only known when the command actually
/// runs: parameter expansion (`$VAR`, `${VAR}`), command substitution
/// (`$(…)`) and the backtick form of the same.
///
/// A token containing one of these is not text blastguard can read — it is a
/// promise about text that will exist later.
pub(super) fn has_unresolvable_expansion(tok: &str) -> bool {
    let mut escaped = false;
    for c in tok.chars() {
        if escaped {
            escaped = false;
            continue;
        }
        match c {
            // A backslash-escaped `$`/backtick is a LITERAL dollar/backtick, not
            // an expansion, so skipping the next char is correct here.
            '\\' => escaped = true,
            '$' | '`' => return true,
            _ => {}
        }
    }
    false
}

/// The first COMMAND-WORD position in `cmd` whose text is an unresolvable
/// expansion, if any.
///
/// This is deliberately restricted to command-word positions — the token that
/// names the *program* — rather than flagging any `$` anywhere on the line.
/// `sh -c "grep $pattern src/"` is fully analysable as far as blastguard's
/// rules go: the program is `grep`, and `$pattern` is an operand whose value
/// cannot turn `grep` into `rm`. Whereas in `sh -c "$CMD"` the expansion IS the
/// program, and NOTHING about what will run is knowable from the text. Only the
/// latter is unanalysable, and only the latter asks.
///
/// Each `;`/`&&`/`||`/`|`/`&`-separated segment gets its own command word, so
/// `sh -c "cd $dir && $BUILD"` is caught on its second segment.
pub(super) fn unresolvable_command_word(cmd: &str) -> Option<String> {
    for seg in split_segments(cmd) {
        let tokens: Vec<&str> = seg.split_whitespace().collect();
        for idx in command_candidates(&tokens) {
            // Compare against the RAW token: `normalized_command` strips one
            // level of quoting, and `"$CMD"` must still read as an expansion
            // after that. Checking the raw token needs no such assumption.
            if has_unresolvable_expansion(tokens[idx]) {
                return Some(tokens[idx].to_string());
            }
        }
    }
    None
}
