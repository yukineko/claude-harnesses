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
    ///   * its execution is guarded — by a `&&`/`||` earlier in its own and-or
    ///     list, or by a compound command or subshell opened anywhere earlier
    ///     on the line — so whether it ran in THIS
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

/// An option word with the variable name attached (`-vP`, `-aP`, `-vP[1]`):
/// any suffix of the cluster that is the name may be what the builtin assigns
/// (superset: only costs a resolution).
fn option_attaches_name(word: &str, name: &str) -> bool {
    word.starts_with('-')
        && !word.starts_with("--")
        && word.char_indices().skip(1).any(|(i, _)| {
            let rest = &word[i..];
            rest == name || word_binds_name(rest, name) || rest.starts_with(&format!("{name}["))
        })
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
            // zsh builtins that assign a variable named by an operand.
            | "print"
            | "zformat"
            | "vared"
            | "zparseopts"
            | "zstat"
            | "sysread"
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
///   * `read BIN` and `printf -v BIN` — `PROG_B`;
///   * a word opening a subshell, `( BIN=…` / `( { BIN=…` — not measured as a
///     rebind of the PARENT (it is not one); refused because the use may sit
///     inside the same subshell, where it is (backlog 6cd305a9).
///
/// Answering true only makes the caller refuse to name a program, so the
/// coarseness here (a `printf` mentioning `BIN` anywhere, a `local` at top
/// level where bash would error) costs resolutions and never invents one.
pub(super) fn segment_may_rebind(seg: &str, name: &str) -> bool {
    segment_may_rebind_with(seg, name, false, true)
}

/// [`segment_may_rebind`]; `exempt_ok` lets a bare `name` operand of a
/// non-binding command pass ([`exemption_disabled`] decides, per command).
pub(super) fn segment_may_rebind_with(seg: &str, name: &str, exempt_ok: bool, posix: bool) -> bool {
    let words = quote_aware_words(seg);
    let Some(first) = words.first() else {
        return false;
    };
    let head = normalized_command(first);
    if matches!(head.as_str(), "eval" | "source" | ".") {
        return true;
    }
    // A prefix assignment on a SPECIAL builtin persists under POSIX mode
    // (`set -o posix; P=/b :` leaves P=/b), and a prefix on `eval` is not
    // known to be transient either (backlog f3301770, measured by the
    // verifier): the name is rebound.
    if let Some((hi, h)) = command_head(&words) {
        let prefix_binds = words[..hi].iter().any(|w| {
            word_binds_name(w, name) || tied_partner(name).is_some_and(|p| word_binds_name(w, p))
        });
        if prefix_binds && (h == "eval" || (posix && POSIX_SPECIAL_BUILTINS.contains(&h.as_str())))
        {
            return true;
        }
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
            plain == name || word_binds_name(&plain, name) || option_attaches_name(&plain, name)
        })
    {
        return true;
    }
    // Structural catch-all (backlog f3301770): ANY command word after the
    // transient assignment prefix that spells the name — or the name it is
    // tied to in zsh — other than as a plain `$name` / `${name…` reference
    // may bind it: `read P`, `printf -v P`, `print -vP P`, `zformat -f P`,
    // `zstat -A P`, `sysread P`, `vared P`, `zparseopts -A P`, `P[0]=`,
    // `{P}>` redirect words, `trap 'P=…'`, behind `command` / `builtin` /
    // `X=1 cmd`. A word list of known binders cannot be complete (zsh adds
    // more), so an unrecognised command naming the variable counts too.
    // Over-reporting only costs a resolution; it never invents one.
    {
        let lead = words.iter().take_while(|w| is_assignment(w)).count();
        let partner = tied_partner(name);
        // A bare `name` operand of a non-binding command is data.
        let exempt = exempt_ok
            && command_head(&words).is_some_and(|(_, h)| NON_BINDERS.contains(&h.as_str()));
        let skip = |w: &str| {
            exempt && (plain_name_arg(w, name) || partner.is_some_and(|p| plain_name_arg(w, p)))
        };
        if words[lead..].iter().any(|w| {
            !skip(w)
                && (mentions_name_outside_reference(w, name)
                    || partner.is_some_and(|p| mentions_name_outside_reference(w, p)))
        }) {
            return true;
        }
    }
    if indirect_name_operand(&words) {
        return true;
    }
    // zsh's `[[ x =~ re ]]` sets MATCH / MBEGIN / MEND / match / mbegin / mend.
    if matches!(
        name,
        "MATCH" | "MBEGIN" | "MEND" | "match" | "mbegin" | "mend"
    ) && words.iter().any(|w| w == "=~")
    {
        return true;
    }
    // A word that OPENS a subshell or arithmetic command (`(L=x`, `( { L=x`,
    // `((L=5))`): `quote_aware_words` keeps everything after an unclosed `(`
    // in that one word, so the word tests below never see the statements
    // inside it. Any mention of the name there other than a plain reference
    // counts (backlog 6cd305a9: the function-call reading spells `f` of
    // `x=$(f; rm -f $L)` as `( { L=/tmp/x` / `}; rm -f $L` / `)`, and the
    // `rm` read the outer `L`). Coarse in the restrictive direction: an
    // assignment in a subshell that does NOT enclose the use refuses too.
    if words
        .iter()
        .any(|w| w.starts_with('(') && mentions_name_outside_reference(w, name))
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

/// The name zsh ties to `name` (assigning one assigns the other): the
/// lowercase array and the uppercase colon-separated scalar of
/// `path`/`PATH`, `cdpath`/`CDPATH`, `fpath`/`FPATH`, `manpath`/`MANPATH`,
/// `mailpath`/`MAILPATH`, `module_path`/`MODULE_PATH`.
pub(super) fn tied_partner(name: &str) -> Option<&'static str> {
    const TIES: [(&str, &str); 6] = [
        ("path", "PATH"),
        ("cdpath", "CDPATH"),
        ("fpath", "FPATH"),
        ("manpath", "MANPATH"),
        ("mailpath", "MAILPATH"),
        ("module_path", "MODULE_PATH"),
    ];
    TIES.iter().find_map(|&(lo, up)| {
        if name == lo {
            Some(up)
        } else if name == up {
            Some(lo)
        } else {
            None
        }
    })
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

/// True when `text` spells the identifier `name` anywhere OTHER than as the
/// plain reference `$name` / `${name…`: `name=…`, `export name`, `read name`,
/// `for name in`, `unset name`, `${#name}`, an escaped `\$name`, or the bare
/// word inside some string. Any of those may bind the name (or is text this
/// helper does not try to tell apart from one), so a caller that needs "nothing
/// here can rebind `name`" must refuse on `true`. Over-reporting only costs a
/// resolution; it never invents one.
///
/// One literal spelling is NOT a mention (backlog 54231a0a): the name as a
/// path component between two slashes, not followed by `=` (`fd` in
/// `/dev/fd/$fd`, `/proc/self/fd/…`). No binding form puts a `/` on both
/// sides of the name — an assignment word, a builtin's name operand
/// (`read`/`unset`/`for`/`export`/…), an `{name}` redirect, or an arithmetic
/// lvalue, which needs the name next to `=` / `op=` / `++` / `--`; the one
/// arithmetic shape with `/` after it, `/=`, is excluded. Without this, a
/// statically bound `$fd` glued to a literal `/dev/fd/` stayed unresolved and
/// was judged as an fd spelled with an expansion instead of as the literal.
pub(super) fn mentions_name_outside_reference(text: &str, name: &str) -> bool {
    let chars: Vec<char> = text.chars().collect();
    let is_ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let mut i = 0;
    while i < chars.len() {
        if !is_ident(chars[i]) {
            i += 1;
            continue;
        }
        let start = i;
        while i < chars.len() && is_ident(chars[i]) {
            i += 1;
        }
        let run: String = chars[start..i].iter().collect();
        let before = |back: usize| start.checked_sub(back).map(|k| chars[k]);
        // An option cluster with the name attached (`printf -vP`, `read -aP`):
        // the builtin may assign it. Superset: only costs a resolution.
        if run != name && run.ends_with(name) && before(1) == Some('-') {
            return true;
        }
        if run != name {
            continue;
        }
        // `${#name}` (a length read) is a plain reference too.
        let plain = before(1) == Some('$') && before(2) != Some('\\')
            || (before(1) == Some('#')
                && before(2) == Some('{')
                && before(3) == Some('$')
                && before(4) != Some('\\'));
        let braced = before(1) == Some('{') && before(2) == Some('$') && before(3) != Some('\\');
        // Backlog 54231a0a: a path component strictly between two slashes
        // (the `fd` of `/dev/fd/$fd`) is literal text, not a binding — but
        // `/=` after it is the arithmetic compound assignment (`x/fd/=2`), so
        // that shape still counts.
        let path_component =
            before(1) == Some('/') && chars.get(i) == Some(&'/') && chars.get(i + 1) != Some(&'=');
        if !plain && !braced && !path_component {
            return true;
        }
        // `${NAME:=v}` / `${NAME::=v}` / `${NAME=v}` (optionally after a
        // subscript) ASSIGN the name: a braced reference is a plain read
        // only when no assignment operator follows (backlog f3301770).
        if braced && expansion_assigns_after(&chars, i) {
            return true;
        }
    }
    false
}

/// True when the text at `chars[i..]` (just after the name inside `${…`) is an
/// assignment operator of a parameter expansion: `=`, `:=`, `::=`, possibly
/// after a `[subscript]`.
fn expansion_assigns_after(chars: &[char], mut i: usize) -> bool {
    if chars.get(i) == Some(&'[') {
        match chars[i..].iter().position(|&c| c == ']') {
            Some(p) => i += p + 1,
            None => return true,
        }
    }
    let rest: String = chars[i..].iter().take(3).collect();
    rest.starts_with('=') || rest.starts_with(":=") || rest.starts_with("::=")
}

/// Commands that cannot bind a shell variable named by one of their plain
/// operands: external programs and non-binding builtins. A bare `P` operand
/// to one of these (`echo P`, `grep P file`) is data, not a binding.
const NON_BINDERS: &[&str] = &[
    "echo", "cat", "grep", "egrep", "fgrep", "ls", "wc", "head", "tail", "sort", "uniq", "test",
    "[", "true", "false", "pwd", "diff", "cut", "tr", "touch", "mkdir", "rm", "cp", "mv", "date",
];

/// Index and normalized name of the command word of `words`: leading
/// assignments and `command` / `builtin` wrappers (with their options) are
/// skipped.
fn command_head<S: AsRef<str>>(words: &[S]) -> Option<(usize, String)> {
    let mut i = 0;
    while words.get(i).is_some_and(|w| is_assignment(w.as_ref())) {
        i += 1;
    }
    loop {
        let name = normalized_command(words.get(i)?.as_ref());
        if matches!(name.as_str(), "command" | "builtin") {
            i += 1;
            while words.get(i).is_some_and(|w| w.as_ref().starts_with('-')) {
                i += 1;
            }
            continue;
        }
        return Some((i, name));
    }
}

/// The normalized command name of one raw word.
pub(super) fn normalized_head(word: &str) -> String {
    normalized_command(word)
}

/// `word` is exactly `name`, quoted or not.
fn plain_name_arg(word: &str, name: &str) -> bool {
    unquote_literal_value(word).as_deref() == Some(name)
}

/// `word` is a bare `name` operand of a command (`words`) that cannot bind a
/// variable: data, not a binding.
pub(super) fn exempt_plain_arg<S: AsRef<str>>(words: &[S], word: &str, name: &str) -> bool {
    command_head(words).is_some_and(|(hi, h)| {
        NON_BINDERS.contains(&h.as_str())
            && words.get(hi).is_some_and(|w| w.as_ref() != word)
            && plain_name_arg(word, name)
    })
}

/// True when some simple statement inside the compound command `src`
/// (`while read "$Q"; do …; done`, `{ printf -v "$Q" x; }`) is a binder whose
/// name operand is an expansion ([`indirect_name_operand`]).
pub(super) fn compound_source_has_indirect_binder(src: &str) -> bool {
    const KEYWORDS: &[&str] = &[
        "while", "until", "if", "elif", "then", "else", "do", "!", "{", "(", "time",
    ];
    src.split([';', '\n', '|', '&', '(', ')', '}'])
        .any(|piece| {
            let words = quote_aware_words(piece);
            let skip = words
                .iter()
                .take_while(|w| KEYWORDS.contains(&w.as_str()) || w.starts_with('{'))
                .count();
            indirect_name_operand(&words[skip..])
        })
}

/// [`mentions_name_outside_reference`] for one segment, except that a bare
/// `name` operand of a command in [`NON_BINDERS`] is not a mention (`echo P`,
/// `grep P file`). Every other spelling of the name still counts.
pub(super) fn segment_mentions_name(seg: &str, name: &str, exempt_ok: bool) -> bool {
    let words = quote_aware_words(seg);
    let exempt =
        exempt_ok && command_head(&words).is_some_and(|(_, h)| NON_BINDERS.contains(&h.as_str()));
    if !exempt || words.is_empty() {
        return mentions_name_outside_reference(seg, name);
    }
    let kept: Vec<&str> = words
        .iter()
        .filter(|w| !plain_name_arg(w, name))
        .map(String::as_str)
        .collect();
    mentions_name_outside_reference(&kept.join(" "), name)
}

/// True when `cmd` may change what a command word MEANS, so a name in
/// [`NON_BINDERS`] cannot be trusted not to bind: an `alias` / `shopt` /
/// `enable` / `unalias` / `functions` / `autoload` / `eval` / `source` / `.`
/// word anywhere (`alias echo=read` makes `echo P` a `read P`), or a function
/// definition. The exemption is off for the whole command then.
pub(super) fn exemption_disabled(cmd: &str) -> bool {
    parsed_command_may_redefine_words(cmd) || exemption_disabled_by_words(cmd)
}

/// POSIX special builtins: under POSIX mode a prefix assignment on one of
/// these persists in the shell.
const POSIX_SPECIAL_BUILTINS: &[&str] = &[
    ":", ".", "break", "continue", "eval", "exec", "exit", "export", "readonly", "return", "set",
    "shift", "times", "trap", "unset", "source", "typeset", "declare", "local", "integer", "float",
];

/// What the whole command says about how command words and prefix
/// assignments behave, decided from the PARSED command (every simple command,
/// compound bodies included; words after quote removal, backslash removal and
/// `$'…'` decoding), never from raw text.
#[derive(Clone, Copy)]
pub(super) struct ShellModes {
    /// Something on the line may run code that is not the text of the
    /// statement being read, so NO earlier binding is believed: an `alias` /
    /// `unalias` / alias-table write, `.` / `source` / `eval`, any `trap` or
    /// `TRAP*` function, a function named like a trusted non-binder command
    /// (`echo(){ … }`; other functions are handled by the function-body
    /// logic), `PS4`, `zle`, or a command
    /// word whose value is not a determined literal. An undetermined parse
    /// counts (fail closed).
    pub alias: bool,
    /// POSIX mode may be on (`set -o posix`, `setopt posix_builtins`,
    /// `unsetopt noposixbuiltins`, `options[posixbuiltins]=`, `emulate sh|ksh`,
    /// `--posix`, `POSIXLY_CORRECT=`), or an option command has an
    /// undetermined argument that could enable it. An undetermined parse
    /// counts.
    pub posix: bool,
}

pub(super) fn shell_modes(cmd: &str) -> ShellModes {
    use super::ast::{analyze, Analysis, Value};
    let mut modes = ShellModes {
        alias: false,
        posix: false,
    };
    // Each statement is parsed ON ITS OWN: after `alias` / `set -o posix` the
    // evaluator's state is opaque and the rest of a whole-line analysis turns
    // `Undetermined`, which would hide the statements that follow. A
    // statement that does not parse alone (`then …`, `do …`) is read from a
    // word split with the keywords and operators peeled off, dequoted by
    // `unquote_literal_value`; a word that does not dequote to a literal is
    // undetermined.
    let segs = split_segments_with_separators(cmd);
    let (is_body, _) = here_document_layout(&segs);
    for (idx, seg) in segs.iter().enumerate() {
        if is_body.get(idx).copied().unwrap_or(false) {
            continue;
        }
        // A function that takes the name of a command the NON_BINDERS
        // exemption trusts (`echo(){ read P; }`) makes that trust false.
        // Other function names are covered by the function-body handling.
        if defined_function_name(&seg.text).is_some_and(|n| NON_BINDERS.contains(&n.as_str())) {
            modes.alias = true;
        }
        match analyze(&seg.text) {
            Analysis::Known(script) => {
                for sc in &script.commands {
                    let assigns: Vec<String> =
                        sc.assignments.iter().map(|(n, _)| n.clone()).collect();
                    let words: Vec<(String, bool)> = sc
                        .argv
                        .iter()
                        .map(|w| match &w.value {
                            Value::Literal(s) => (s.clone(), true),
                            Value::Undetermined(_) => (w.raw.clone(), false),
                        })
                        .collect();
                    classify_command(&assigns, &words, &mut modes);
                }
            }
            Analysis::Undetermined(_) => classify_text_statement(&seg.text, &mut modes),
        }
    }
    modes
}

/// Name defined by a leading `name(){…}` / `function name …` statement.
fn defined_function_name(text: &str) -> Option<String> {
    let t = text.trim_start();
    let t = t.strip_prefix('{').map_or(t, str::trim_start);
    if let Some(rest) = t.strip_prefix("function") {
        if rest.starts_with(char::is_whitespace) {
            let name: String = rest
                .trim_start()
                .chars()
                .take_while(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | ':'))
                .collect();
            return (!name.is_empty()).then_some(name);
        }
    }
    let name: String = t
        .chars()
        .take_while(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | ':'))
        .collect();
    let after = t[name.len()..].trim_start();
    let inner = after.strip_prefix('(')?.trim_start();
    inner
        .starts_with(')')
        .then_some(name)
        .filter(|n| !n.is_empty())
}

/// A statement that did not parse alone, read from its words.
fn classify_text_statement(text: &str, modes: &mut ShellModes) {
    const KEYWORDS: &[&str] = &[
        "then", "do", "else", "elif", "if", "while", "until", "!", "{", "(", "time", "function",
    ];
    let mut assigns: Vec<String> = Vec::new();
    let mut words: Vec<(String, bool)> = Vec::new();
    for w in quote_aware_words(text) {
        let w = w.trim_start_matches(['(', '{']).to_string();
        if w.is_empty() {
            continue;
        }
        if words.is_empty() && KEYWORDS.contains(&w.as_str()) {
            continue;
        }
        if words.is_empty() && is_assignment(&w) {
            assigns.push(w.split('=').next().unwrap_or(&w).to_string());
            continue;
        }
        match unquote_literal_value(&w) {
            Some(s) => words.push((s, true)),
            None => words.push((w, false)),
        }
    }
    classify_command(&assigns, &words, modes);
}

/// One simple command: its prefix-assignment names and `(word, determined)`
/// argv. Sets [`ShellModes`] flags (fail closed on an undetermined command
/// word or an undetermined option argument).
fn classify_command(assigns: &[String], words: &[(String, bool)], modes: &mut ShellModes) {
    // brush-parser leaves `name[sub]=v` as a command WORD: it is an
    // assignment (`options[posixbuiltins]=on`, `BASH_ALIASES[echo]=…`).
    let mut assigns: Vec<String> = assigns.to_vec();
    let mut start = 0;
    while let Some((w, true)) = words.get(start) {
        let Some(eq) = w.find('=') else { break };
        let name = w[..eq].trim_end_matches('+');
        let base = name.split('[').next().unwrap_or(name);
        if !(is_shell_identifier(base) && (base == name || name.ends_with(']'))) {
            break;
        }
        assigns.push(name.to_string());
        start += 1;
    }
    let words = &words[start..];
    for n in &assigns {
        let base = n.split('[').next().unwrap_or(n).trim_end_matches('+');
        if is_command_table_name(n) || base == "PS4" || base.starts_with("TRAP") {
            modes.alias = true;
        }
        if base == "POSIXLY_CORRECT" || base == "options" {
            modes.posix = true;
        }
    }
    // Skip the `command` / `builtin` wrappers.
    let mut i = 0;
    while words
        .get(i)
        .is_some_and(|(s, det)| *det && matches!(s.as_str(), "command" | "builtin"))
    {
        i += 1;
    }
    let Some((h, det)) = words.get(i) else {
        return;
    };
    if !det {
        // The command word is not determined: it may be `alias`, `source`,
        // anything.
        modes.alias = true;
        return;
    }
    let h = h.as_str();
    let args = &words[i + 1..];
    if matches!(
        h,
        "alias" | "unalias" | "." | "source" | "eval" | "trap" | "zle" | "autoload" | "functions"
    ) || is_command_table_name(h)
        || h.starts_with("TRAP")
    {
        modes.alias = true;
    }
    if matches!(h, "set" | "setopt" | "unsetopt" | "shopt" | "emulate") {
        for (s, det) in args {
            let lower = s.to_ascii_lowercase().replace('_', "");
            if !det
                || lower.contains("posix")
                || (h == "emulate" && matches!(s.as_str(), "sh" | "ksh"))
            {
                modes.posix = true;
            }
        }
    }
    if words.iter().any(|(s, det)| *det && s == "--posix") {
        modes.posix = true;
    }
}

/// Names whose assignment redefines what a command word means: zsh's
/// `functions` / `aliases` / `galiases` / `saliases` / `dis_*` tables and
/// bash's `BASH_ALIASES` (`name`, `name[k]`, `name+` all count).
pub(super) fn is_command_table_name(name: &str) -> bool {
    let base = name.split('[').next().unwrap_or(name).trim_end_matches('+');
    matches!(
        base,
        "functions"
            | "aliases"
            | "galiases"
            | "saliases"
            | "dis_aliases"
            | "dis_galiases"
            | "dis_saliases"
            | "dis_functions"
            | "BASH_ALIASES"
    )
}

/// The decision of [`exemption_disabled`] from the PARSED command (the same
/// brush-parser AST + evaluator the rest of blastguard judges with), not from
/// a whitespace split: command words are read after quote removal, backslash
/// removal and `$'…'` decoding, so `\shopt`, `"alias"`, `sh''opt` are
/// `shopt` / `alias`. True (exemption off) when
/// * the parse or evaluation is undetermined (fail closed);
/// * some simple command, anywhere (compound bodies included), has a
///   command word that is not a literal, or any literal word in
///   {alias, unalias, shopt, enable, functions, autoload, eval, source, `.`,
///   trap, zle, function} (so `command` / `builtin` / `\` wrappers are
///   covered), or `typeset` / `declare` with an `f` option cluster;
/// * any word or assignment names one of [`is_command_table_name`].
fn parsed_command_may_redefine_words(cmd: &str) -> bool {
    use super::ast::{analyze, Analysis, Value};
    let Analysis::Known(script) = analyze(cmd) else {
        return true;
    };
    script.commands.iter().any(|sc| {
        if sc.assignments.iter().any(|(n, _)| is_command_table_name(n)) {
            return true;
        }
        let mut literals: Vec<&str> = Vec::new();
        for (i, w) in sc.argv.iter().enumerate() {
            match &w.value {
                Value::Literal(s) => literals.push(s.as_str()),
                Value::Undetermined(_) if i == 0 => return true,
                Value::Undetermined(_) => {}
            }
        }
        literals.iter().any(|s| {
            matches!(
                *s,
                "alias"
                    | "unalias"
                    | "shopt"
                    | "enable"
                    | "functions"
                    | "autoload"
                    | "eval"
                    | "source"
                    | "."
                    | "trap"
                    | "zle"
                    | "function"
            ) || is_command_table_name(s.split('=').next().unwrap_or(s))
        }) || (literals
            .iter()
            .any(|h| matches!(*h, "typeset" | "declare" | "local"))
            && literals
                .iter()
                .any(|s| s.starts_with('-') && !s.starts_with("--") && s.contains('f')))
    })
}

fn exemption_disabled_by_words(cmd: &str) -> bool {
    defines_shell_function(cmd)
        || quote_aware_words(cmd).iter().any(|w| {
            let w = unquote_literal_value(w).unwrap_or_else(|| w.to_string());
            let w = w.trim_start_matches(['(', '{']);
            matches!(
                w,
                "alias"
                    | "shopt"
                    | "enable"
                    | "unalias"
                    | "functions"
                    | "autoload"
                    | "eval"
                    | "source"
                    | "."
                    | "function"
            ) || w.starts_with("functions[")
        })
}

/// [`mentions_name_outside_reference`] over the simple statements of a compound
/// command's source `src`, with the bare-operand exemption of
/// [`segment_mentions_name`] applied per statement.
pub(super) fn compound_mentions_name(src: &str, name: &str, exempt_ok: bool) -> bool {
    const KEYWORDS: &[&str] = &[
        "while", "until", "if", "elif", "then", "else", "do", "!", "{", "(", "time",
    ];
    src.split([';', '\n', '|', '&', '(', ')', '}'])
        .any(|piece| {
            let words = quote_aware_words(piece);
            let skip = words
                .iter()
                .take_while(|w| KEYWORDS.contains(&w.as_str()) || w.starts_with('{'))
                .count();
            segment_mentions_name(&words[skip..].join(" "), name, exempt_ok)
                || mentions_name_outside_reference(&words[..skip].join(" "), name)
        })
}

/// True when the segment runs a command that binds a variable NAMED BY AN
/// EXPANSION (`printf -v "$Q"`, `read "$Q"`, `declare "$Q=x"`, `set -A $N`):
/// the name is not in the text, so no tracked variable can be trusted
/// afterwards (backlog f3301770). `words` are raw spellings.
pub(super) fn indirect_name_operand<S: AsRef<str>>(words: &[S]) -> bool {
    let Some((hi, head)) = command_head(words) else {
        return false;
    };
    // Operands, with redirections (`<<< "$x"`, `2>$f`) removed.
    let mut rest: Vec<&str> = Vec::new();
    let mut skip = false;
    for w in words[hi + 1..].iter().map(AsRef::as_ref) {
        if skip {
            skip = false;
            continue;
        }
        let op = w.trim_start_matches(|c: char| c.is_ascii_digit());
        if op.starts_with('<') || op.starts_with('>') {
            skip = op.trim_start_matches(['<', '>', '&']).is_empty() && !op.ends_with('&');
            continue;
        }
        rest.push(w);
    }
    let cluster = |p: char| {
        rest.iter()
            .any(|w| w.starts_with('-') && !w.starts_with("--") && w.contains(p))
    };
    let expands_name = |w: &&str| {
        !w.starts_with('-') && w.split('=').next().is_some_and(|n| n.contains(['$', '`']))
    };
    match head.as_str() {
        "read" | "readarray" | "mapfile" | "unset" | "declare" | "typeset" | "local" | "export"
        | "readonly" | "integer" | "float" | "zformat" | "vared" | "zparseopts" | "zstat"
        | "sysread" => rest.iter().any(expands_name),
        "getopts" => rest
            .iter()
            .filter(|w| !w.starts_with('-'))
            .nth(1)
            .is_some_and(|w| w.contains(['$', '`'])),
        // Only the NAME operand (the word after the flag, or the attached
        // `-vNAME`) matters; value arguments are data.
        "printf" | "print" => rest.iter().enumerate().any(|(i, w)| {
            w.starts_with('-')
                && !w.starts_with("--")
                && w.find('v').is_some_and(|p| {
                    let attached = &w[p + 1..];
                    if attached.is_empty() {
                        rest.get(i + 1).is_some_and(|n| n.contains(['$', '`']))
                    } else {
                        attached.contains(['$', '`'])
                    }
                })
        }),
        "zstyle" | "strftime" => cluster('s') && rest.iter().any(expands_name),
        "set" => rest
            .iter()
            .position(|w| *w == "-A" || *w == "+A")
            .is_some_and(|i| rest.get(i + 1).is_some_and(|n| n.contains(['$', '`']))),
        _ => false,
    }
}

/// The two names `typeset -T A B` / `declare -T A B` ties (zsh): assigning
/// either assigns the other.
pub(super) fn tie_pair<S: AsRef<str>>(words: &[S]) -> Option<(String, String)> {
    let (hi, head) = command_head(words)?;
    if !matches!(
        head.as_str(),
        "typeset" | "declare" | "local" | "export" | "readonly"
    ) {
        return None;
    }
    let rest = &words[hi + 1..];
    if !rest.iter().any(|w| {
        w.as_ref().starts_with('-') && !w.as_ref().starts_with("--") && w.as_ref().contains('T')
    }) {
        return None;
    }
    let mut names = rest
        .iter()
        .map(AsRef::as_ref)
        .filter(|w| !w.starts_with('-'))
        .map(|w| w.split('=').next().unwrap_or(w).to_string());
    Some((names.next()?, names.next()?))
}

/// True when `line` may define a shell function: a `function` word, or a `(`
/// followed (after optional blanks) by `)` that is not the opening of a
/// `$(…)` / `<(…)` / `>(…)` substitution (`f() { … }`, `f ( ) …`). A function
/// called later can assign any variable, so a caller that must know nothing
/// rebinds a name refuses on `true`. Over-reporting only costs a resolution.
pub(super) fn defines_shell_function(line: &str) -> bool {
    if line
        .split(|c: char| c.is_whitespace() || matches!(c, ';' | '&' | '|' | '(' | ')'))
        .any(|w| w == "function")
    {
        return true;
    }
    let chars: Vec<char> = line.chars().collect();
    for (k, c) in chars.iter().enumerate() {
        if *c != '(' {
            continue;
        }
        if k > 0 && matches!(chars[k - 1], '$' | '<' | '>') {
            continue;
        }
        let mut j = k + 1;
        while j < chars.len() && chars[j].is_whitespace() {
            j += 1;
        }
        if chars.get(j) == Some(&')') {
            return true;
        }
    }
    false
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
/// EXECUTION rather than scope: no `&&`/`||` may stand before it within its
/// OWN and-or list, and no compound command, subshell or unclosed
/// here-document may be opened anywhere before it on the line
/// ([`assignment_execution_is_unconditional`]). A `&&`/`||` in an earlier
/// and-or list already closed by `;`, `&` or a newline does not guard it:
/// `foo && BIN=/bin/a; BIN=/bin/b; $BIN x` resolves to `/bin/b`. A guarded
/// winner answers
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
    let mut ties: Vec<(String, String)> = Vec::new();
    let exempt_ok = !exemption_disabled(cmd);
    let modes = shell_modes(cmd); // A here-document BODY is data the shell never executes, so a `NAME=value`
                                  // typed inside one neither assigns nor rebinds. Skipping it here is what
                                  // lets the closed-here-document case above be believed at all: the reason
                                  // to distrust the tail was the body, and the body is now identified.
    let (is_here_doc_body, _) = here_document_layout(&segs);
    for idx in 0..seg_idx {
        if is_here_doc_body.get(idx).copied().unwrap_or(false) {
            continue;
        }
        // `typeset -T A B` (zsh) ties two names for the rest of the line.
        if let Some(t) = tie_pair(&quote_aware_words(&segs[idx].text)) {
            ties.push(t);
        }
        if !assignments_reach_parent_shell(&segs, idx) {
            // Not a parent-shell assignment, but zsh runs the LAST element of
            // a pipeline in the parent shell (`echo x | read P`) and bash
            // may too (`shopt -s lastpipe`), so a stage that may bind the
            // name makes the earlier value stale (backlog f3301770).
            if segment_may_rebind_with(&segs[idx].text, &name, exempt_ok, modes.posix) {
                latest_rebind = Some(idx);
            }
            continue;
        }
        let words = quote_aware_words(&segs[idx].text);
        if words.is_empty() || !words.iter().all(|w| is_assignment(w)) {
            if segment_may_rebind_with(&segs[idx].text, &name, exempt_ok, modes.posix) {
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
            } else if tied_partner(&name).is_some_and(|p| w[..eq] == *p)
                || ties
                    .iter()
                    .any(|(a, b)| (*a == name && w[..eq] == *b) || (*b == name && w[..eq] == *a))
            {
                // zsh ties `path`/`PATH` (and the other pairs): assigning
                // the partner assigns this name (backlog f3301770).
                latest_rebind = Some(idx);
            }
        }
    }
    let Some((assign_idx, raw_value)) = latest else {
        return CommandWordOrigin::NoReachingAssignment;
    };
    if modes.alias {
        // An alias defined anywhere on the line (before the assignment
        // too) may give any command word a body that binds this name, so
        // no assignment is believed (backlog f3301770).
        return CommandWordOrigin::AssignedButUnknowable;
    }
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
        // Redirection words (`< "$f"`, `2>/dev/null`) are not command-word
        // candidates: under an exec wrapper every following token is one, so
        // `env -i /usr/bin/wc < "$f"` read the stdin operand as the program
        // (3775027f). Dropping them leaves every real word, so `env -i "$X"`
        // and `< f "$X"` still reach the check.
        let mut skip_next = false;
        let mut tokens: Vec<&str> = Vec::new();
        for w in seg.split_whitespace() {
            if skip_next {
                skip_next = false;
                continue;
            }
            let rest = w.trim_start_matches(|c: char| c.is_ascii_digit());
            if rest.starts_with('>') || rest.starts_with('<') {
                let target = rest.trim_start_matches(['>', '<', '&']);
                skip_next = target.is_empty() && !rest.ends_with('&');
                continue;
            }
            tokens.push(w);
        }
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
