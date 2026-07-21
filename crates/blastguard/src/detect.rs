//! Pure destructive-operation detection. No I/O, no globals beyond the static
//! allowlist in [`crate::exclude`] — just `(tool_name, tool_input) -> Decision`.
//!
//! The bias is deliberately asymmetric: we only Deny on *clearly* destructive,
//! hard-to-undo patterns (recursive/wildcard deletion, full-file truncation,
//! disk-level writes, working-tree discards).
//!
//! There are three answers, not two. A command blastguard positively knows to
//! be destructive is DENIED; one it positively knows to be ordinary is ALLOWED;
//! and a construct it genuinely CANNOT ANALYSE is ASKED
//! ([`crate::model::Decision::Ask`]) rather than waved through. "Unknown" here
//! means specifically *unanalysable* — an unresolvable expansion sitting in a
//! position that gets executed, an unrecognised wrapper in front of a
//! destructive command line, or a command too complex to finish analysing.
//! It does NOT mean "not on an allowlist": there is no allowlist, and an
//! ordinary command this module has no rule for still falls through to Allow.
//!
//! An `Ask` is only emitted by the hook when a human is actually there to
//! answer it (see [`crate::interactive`]); otherwise it is hardened to a Deny.

use serde_json::Value;

use crate::exclude;
use crate::model::Decision;

/// Top-level entry: dispatch on the tool name.
pub fn detect(tool_name: &str, tool_input: Option<&Value>) -> Decision {
    match tool_name {
        "Bash" => {
            let cmd = tool_input
                .and_then(|v| v.get("command"))
                .and_then(|c| c.as_str());
            match cmd {
                Some(c) => {
                    // D4: reset the per-command node budget at the single
                    // top-level entry point, so each tool call gets a full
                    // budget and no state leaks between calls.
                    ANALYSIS_BUDGET.with(|b| b.set(MAX_ANALYSIS_NODES));
                    detect_bash(c, 0)
                }
                None => Decision::Allow,
            }
        }
        "Write" => detect_write(tool_input),
        // Edit / MultiEdit / NotebookEdit are partial edits, not full-file
        // destruction — always allowed.
        _ => Decision::Allow,
    }
}

// ---------------------------------------------------------------------------
// File-write handling
// ---------------------------------------------------------------------------

fn extract_path(ti: Option<&Value>) -> Option<String> {
    let v = ti?;
    for key in ["file_path", "notebook_path", "path"] {
        if let Some(p) = v.get(key).and_then(|p| p.as_str()) {
            if !p.is_empty() {
                return Some(p.to_string());
            }
        }
    }
    None
}

/// Write is new/overwrite both. We stay conservative and only Deny the clearly
/// destructive shapes: replacing a (non-config) file with empty content, or
/// overwriting git internals. Everything else is allowed.
fn detect_write(ti: Option<&Value>) -> Decision {
    let path = match extract_path(ti) {
        Some(p) => p,
        None => return Decision::Allow,
    };
    if exclude::is_config_file(&path) {
        return Decision::Allow;
    }
    if exclude::is_git_internal(&path) {
        return Decision::deny(format!(
            "Write would overwrite git internals ({path}) — refusing"
        ));
    }
    let content = ti
        .and_then(|v| v.get("content"))
        .and_then(|c| c.as_str())
        .unwrap_or("");
    if content.trim().is_empty() {
        return Decision::deny(format!(
            "Write would replace {path} with empty content, wiping the file"
        ));
    }
    Decision::Allow
}

// ---------------------------------------------------------------------------
// Bash handling
// ---------------------------------------------------------------------------

/// How deep we recurse into shell-evaluation wrappers (`eval`, `sh -c`, …)
/// before giving up. Bounds work and guards against pathological nesting; no
/// realistic command line wraps a destructive payload this many layers deep.
const MAX_SHELL_DEPTH: usize = 8;

/// Total number of `detect_bash` invocations allowed for ONE top-level command.
///
/// D4 (verified availability defect, fixed): a depth cap alone does NOT bound
/// the work, because the re-analysis arms FAN OUT. `analyze_find` loops over
/// every `-exec` position and re-analyses the whole remaining tail, which still
/// contains the other `-exec` tokens, so the branching factor is the number of
/// `-exec` tokens and the tree is O(N^MAX_SHELL_DEPTH). Measured on a single
/// invocation of `find . ` + `-exec find . `×N + `-exec echo {} ` + `+ `×(N+1):
/// N=20 → 2.2s, N=24 → 8.2s, N=28 → 30s, N=32 (a 503-byte command) → >60s,
/// killed. blastguard is a PreToolUse hook, so that hangs the user's turn.
///
/// This caps total NODE VISITS across the whole recursion tree, not just its
/// depth. Ordinary commands use fewer than a dozen visits; the cap is three
/// orders of magnitude above that, so it cannot be reached by real work.
const MAX_ANALYSIS_NODES: u32 = 2_000;

thread_local! {
    /// Remaining node budget for the top-level command currently being analysed.
    static ANALYSIS_BUDGET: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// Consume one unit of the analysis budget. Returns false when it is exhausted.
fn take_budget() -> bool {
    ANALYSIS_BUDGET.with(|b| {
        let left = b.get();
        if left == 0 {
            false
        } else {
            b.set(left - 1);
            true
        }
    })
}

/// Combines sub-analysis verdicts under the `Deny > Ask > Allow` ranking.
///
/// Every re-analysis loop in this file (segments, `-c` payload candidates,
/// command-word candidates, `-exec` tails) evaluates several sub-commands and
/// must report the STRONGEST verdict any of them produced. Two mistakes are
/// possible here and both are fail-opens:
///
///   * testing `is_deny()` and returning early on it alone — an `Ask` from a
///     sub-analysis is then dropped and the loop falls through to Allow;
///   * returning the FIRST blocking verdict — an early `Ask` would then mask a
///     later `Deny` on the same line, downgrading a known-destructive command
///     to a question.
///
/// So: a `Deny` short-circuits (nothing can outrank it), an `Ask` is remembered
/// but the scan continues in case a `Deny` follows, and `finish` reports the
/// remembered `Ask` only if no `Deny` was ever seen.
#[derive(Default)]
struct VerdictAcc {
    ask: Option<Decision>,
}

impl VerdictAcc {
    /// Record a sub-verdict. Returns `Some(deny)` when the caller must stop and
    /// return it immediately; `None` when the scan should continue.
    fn record(&mut self, d: Decision) -> Option<Decision> {
        match d {
            Decision::Deny(_) => Some(d),
            Decision::Ask(_) => {
                // Keep the FIRST ask, so the reported reason is the outermost /
                // earliest unanalysable construct rather than the last one.
                if self.ask.is_none() {
                    self.ask = Some(d);
                }
                None
            }
            Decision::Allow => None,
        }
    }

    /// The strongest verdict seen, given no `Deny` short-circuited the caller.
    fn finish(self) -> Decision {
        self.ask.unwrap_or(Decision::Allow)
    }
}

/// Shell metacharacters whose value is only known when the command actually
/// runs: parameter expansion (`$VAR`, `${VAR}`), command substitution
/// (`$(…)`) and the backtick form of the same.
///
/// A token containing one of these is not text blastguard can read — it is a
/// promise about text that will exist later.
fn has_unresolvable_expansion(tok: &str) -> bool {
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
fn unresolvable_command_word(cmd: &str) -> Option<String> {
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

/// Re-analyse a payload string that a shell-evaluation position will EXECUTE
/// (`sh -c <payload>`, `eval <payload>`, `flock -c <payload>`).
///
/// Two outcomes beyond the ordinary rules:
///   * the payload analyses as destructive → propagate that `Deny` unchanged;
///   * the payload's command word is an unresolvable expansion → `Ask`. The
///     text that will actually be executed is not in the command line at all,
///     so "no rule matched" here means "nothing was examined", not "safe".
///
/// Deny is checked FIRST so a payload that is both destructive and partly
/// unresolvable (`sh -c "rm -rf $DIR"` — command word `rm`, resolvable) keeps
/// its Deny.
fn analyze_shell_payload(payload: &str, depth: usize) -> Decision {
    let d = detect_bash(payload, depth + 1);
    if d.is_blocking() {
        return d;
    }
    if let Some(word) = unresolvable_command_word(payload) {
        return Decision::ask(format!(
            "the command executed here comes from `{word}`, whose value only exists at run time — blastguard cannot see what would actually run"
        ));
    }
    Decision::Allow
}

/// The recursion cap was reached, so analysis DID NOT FINISH. That is the same
/// condition as budget exhaustion a few lines up (`detect_bash`'s D4 guard) —
/// "we did not finish looking, so we have no verdict" — and it gets the same
/// answer: an Ask, which `Decision::hardened` turns into the pre-existing Deny
/// whenever no human is available to answer.
///
/// Every one of the five depth-capped sites used to resolve to `Allow`, either
/// by returning it outright or by skipping the recursion and falling through to
/// `acc.finish()`. A destructive payload nested past the cap was therefore
/// waved through *unanalysed* — the exact fail-open this module exists to
/// prevent, sitting one line away from the sibling condition that handles it
/// correctly.
///
/// The reason deliberately differs from the budget ask so the two unfinished-
/// analysis causes stay distinguishable in the logs.
fn depth_exhausted() -> Decision {
    Decision::ask(
        "command nests shell/wrapper invocations deeper than blastguard analyses \
         (recursion depth limit) — analysis did not finish, so blastguard cannot \
         vouch for it either way",
    )
}

fn detect_bash(cmd: &str, depth: usize) -> Decision {
    // D4: when the budget is exhausted we never Allow. A command too complex to
    // analyse within a bounded amount of work must not be waved through
    // unanalysed — an unanalysed destructive command is exactly the fail-open
    // this module exists to prevent.
    //
    // Budget exhaustion is the definition of "unknown": we did not finish
    // looking, so we have no verdict about the command. That is an ASK, which
    // hardens to the pre-existing DENY whenever no human is available to answer
    // (see `crate::interactive` and `Decision::hardened`) — so the fallback is
    // exactly as safe as the deny this replaced, never weaker.
    if !take_budget() {
        return Decision::ask(
            "command is too complex to analyse within the safety budget — blastguard cannot vouch for it either way",
        );
    }

    // 1. Fork bomb (whitespace-insensitive signature).
    let compact: String = cmd.chars().filter(|c| !c.is_whitespace()).collect();
    if compact.contains(":(){") && compact.contains(":|:") {
        return Decision::deny("fork bomb pattern detected");
    }

    // 2. Truncating `>` redirects (quote-aware, ignores >>, &>> and fd dups
    //    like `2>&1`/`>&2`; but catches EVERY `N>file` truncating form for any
    //    fd N, plus the combined stdout+stderr truncating form `&>`). Scan
    // *every* redirect on the line, not just the first: a safe early redirect
    // (`> /dev/null`) must not blind the gate to a later truncating redirect in
    // a subsequent `;`/`&&`/`|` segment.
    for target in redirect_targets(cmd) {
        if !redirect_target_is_safe(&target) {
            return Decision::deny(format!(
                "'> {target}' truncates and overwrites an existing file"
            ));
        }
    }

    // 3. Per-command-segment analysis. Ranked: a Deny in ANY segment outranks
    //    an Ask in any other (see `VerdictAcc`).
    let mut acc = VerdictAcc::default();
    for seg in split_segments(cmd) {
        if let Some(deny) = acc.record(analyze_segment(&seg, depth)) {
            return deny;
        }
    }

    acc.finish()
}

fn redirect_target_is_safe(target: &str) -> bool {
    let t = exclude::normalize(target);
    matches!(t.as_str(), "/dev/null" | "/dev/stdout" | "/dev/stderr") || exclude::is_config_file(&t)
}

// D1 (verified universal bypass, REMOVED — do not re-add): a `is_temp_scratch`
// predicate used to allow any redirect target under `/tmp`, `/var/tmp`,
// `/var/folders` and their `/private` twins, so that `cargo test 2> /tmp/log`
// would not be denied. It was a raw `starts_with` prefix test, and
// `exclude::normalize` does NOT resolve `..`, so `/tmp/../etc/hosts`,
// `/var/tmp/../../etc/hosts`, `/private/tmp/../../Users/yuki/.zshrc`,
// `/var/folders/../../etc/hosts` and `/tmp//../etc/hosts` all passed the
// prefix test and thereby disabled the ENTIRE truncating-redirect rule for
// ANY target on the line.
//
// It was NOT replaced with a `..`-resolving version. A carve-out whose only
// purpose is convenience is exactly what produced this bypass, and the repo's
// governing rule is that a gate which fails open is worse than no gate. The
// pre-existing (sound) behaviour is restored: `> /tmp/log` is DENIED. That is
// a false positive, which is the acceptable side of the trade — the user can
// rephrase (`>> /tmp/log`, `2>&1`, `/dev/null`) or approve manually.

/// Quote-aware split of a command line into individual simple-command segments
/// on `;`, newline, `&&`, `||`, `|`, `&`.
fn split_segments(cmd: &str) -> Vec<String> {
    // Iterate over `char`s, not raw bytes: casting a UTF-8 continuation byte
    // `as char` yields a bogus Latin-1 scalar (e.g. 0xA0 → U+00A0), which both
    // corrupts non-ASCII segments and can misfire operator/quote checks. All the
    // operators we split on are ASCII, so char iteration preserves ASCII behavior
    // exactly while keeping multi-byte text intact.
    let chars: Vec<char> = cmd.chars().collect();
    let mut segs = Vec::new();
    let mut cur = String::new();
    let (mut in_s, mut in_d) = (false, false);
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\'' && !in_d {
            in_s = !in_s;
            cur.push(c);
            i += 1;
            continue;
        }
        if c == '"' && !in_s {
            in_d = !in_d;
            cur.push(c);
            i += 1;
            continue;
        }
        if !in_s && !in_d {
            // Two-char operators.
            if (c == '&' && chars.get(i + 1) == Some(&'&'))
                || (c == '|' && chars.get(i + 1) == Some(&'|'))
            {
                segs.push(std::mem::take(&mut cur));
                i += 2;
                continue;
            }
            if c == ';' || c == '\n' || c == '|' || c == '&' {
                segs.push(std::mem::take(&mut cur));
                i += 1;
                continue;
            }
        }
        cur.push(c);
        i += 1;
    }
    segs.push(cur);
    segs
}

/// Find the first single `>` redirect outside quotes and return its target
/// token. Returns None for `>>`, `&>>`, `>&<digit>` (fd dup) and quoted `>`
/// (but `&>`, `>&<filename>` — the combined stdout+stderr truncating forms —
/// and every explicit-fd `N>file` yield their target).
#[cfg(test)]
fn single_redirect_target(seg: &str) -> Option<String> {
    redirect_targets(seg).into_iter().next()
}

/// Every single `>` truncating-redirect target on the line, in order, outside
/// quotes. Skips `>>`, `&>>`, `>&<digit>` (fd dup), quoted `>`, Rust
/// arrows (`->`) and angle-bracket placeholders (`<value>`); catches every
/// explicit-fd truncating form (`2>f`, `0>f`, `3>f`, `1>f`) and the
/// truncating `>&<filename>` mirror of `&>`. Scanning the *whole* line (rather
/// than pre-split segments) keeps the fd-dup context (`2>&1`, `>&2`) intact
/// while still catching a truncating redirect in any later segment.
fn redirect_targets(seg: &str) -> Vec<String> {
    let bytes = seg.as_bytes();
    let (mut in_s, mut in_d) = (false, false);
    let mut targets = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'\'' && !in_d {
            in_s = !in_s;
            i += 1;
            continue;
        }
        if c == b'"' && !in_s {
            in_d = !in_d;
            i += 1;
            continue;
        }
        if c == b'>' && !in_s && !in_d {
            let prev = if i > 0 { bytes[i - 1] } else { 0 };
            let next = *bytes.get(i + 1).unwrap_or(&0);
            // BG-2 (fail-open regression, fixed): explicit-fd redirects whose fd
            // is NOT stdout (`2>file`, `0>file`, `3>file`, `11>file`) used to be
            // skipped outright. That is WRONG — `N>file` truncates `file` for
            // EVERY fd N, exactly like bare `>file`; only the fd the program
            // writes through differs, not the effect on the target. Skipping them
            // let `shred --help 2> /some/path` and `echo x 2> /some/path` through.
            // There is deliberately no fd-based skip left here: `2>&1` / `>&2`
            // (fd DUP, no file touched) and `2>/dev/null` are still allowed, but
            // via the `fd_dup_amp` clause and `redirect_target_is_safe`
            // respectively — i.e. by what the redirect DOES, not by which fd
            // number precedes the `>`.
            //
            // `>&<target>` is an fd DUPLICATION / fd-CLOSE (safe to skip) ONLY
            // when the ENTIRE target token is all ASCII digits (`>&2`, `>&1`,
            // `>&02`, `1>&2`) or the fd-close `-` (`>&-`, bash touches no file).
            // A target that merely STARTS with a digit but contains ANY
            // non-digit (`>&2x`, `>&2.txt`, `>&10.log`) is a FILENAME — the
            // bash MIRROR of `&>` — and truncates BOTH stdout and stderr into
            // that file, so it must NOT be skipped. Scan the FULL token after
            // `&` (the WHOLE token, not just its first byte) and require it
            // to terminate at whitespace / a
            // command terminator / end-of-token.
            let fd_dup_amp = next == b'&' && {
                let tstart = i + 2;
                let mut k = tstart;
                while k < bytes.len() {
                    let ck = bytes[k];
                    if ck.is_ascii_whitespace()
                        || ck == b';'
                        || ck == b'|'
                        || ck == b'&'
                        || ck == b'>'
                    {
                        break;
                    }
                    k += 1;
                }
                let tok = &bytes[tstart..k];
                // All-digit fd number (>=1 digit) => dup; bare `-` => fd close.
                (!tok.is_empty() && tok.iter().all(u8::is_ascii_digit)) || tok == b"-"
            };
            // Skip append `>>`, fd dup forms, and stderr/other-fd forms.
            // NOTE: `&>` (prev == `&`, single `>`) is NOT an fd-dup form — it is
            // the combined stdout+stderr TRUNCATING redirect (`echo x &> f` /
            // `&>f`), so it must fall through and be read as a real truncating
            // target. Only the APPEND form `&>>` (caught here by `next == b'>'`)
            // is non-truncating and stays skipped. (`2>&1`, `>&2` are still
            // skipped via the `fd_dup_amp` fd-dup clause.)
            if next == b'>' || prev == b'>' || fd_dup_amp {
                i += 1;
                continue;
            }
            // Not a redirect: the second char of a Rust return-type arrow `->`.
            if prev == b'-' {
                i += 1;
                continue;
            }
            // Not a redirect: the close of an angle-bracket identifier
            // placeholder like `<value>` / `<id>` / `<RID>` — a `>` reached by
            // scanning back over identifier chars to an opening `<`.
            if is_angle_placeholder_close(bytes, i) {
                i += 1;
                continue;
            }
            // Single truncating redirect — read the target token.
            // Byte-scan the target token. Only ASCII bytes are treated as
            // whitespace/delimiters: ASCII bytes are always UTF-8 char
            // boundaries, so `seg[start..j]` never slices inside a multi-byte
            // char. (Casting a continuation byte `as char` could read as
            // U+00A0 no-break-space and break mid-character → panic.)
            let mut j = i + 1;
            // `>&<filename>` (the `&>` mirror): the `&` immediately follows `>`.
            // Step over it so we read the FILENAME target, not an empty token.
            // (`>&<digit>` fd DUPs were already skipped above via `fd_dup_amp`.)
            if bytes.get(j) == Some(&b'&') {
                j += 1;
            }
            while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            let start = j;
            while j < bytes.len() {
                let cj = bytes[j];
                if cj.is_ascii_whitespace() || cj == b';' || cj == b'|' || cj == b'&' || cj == b'>'
                {
                    break;
                }
                j += 1;
            }
            let tok = &seg[start..j];
            // A bare `-` target is an fd CLOSE (`>&-` / `>& -`), not a file —
            // bash touches no file, so it is non-destructive.
            if tok != "-" {
                targets.push(tok.to_string());
            }
            i = j;
            continue;
        }
        i += 1;
    }
    targets
}

/// True when the `>` at `bytes[gt]` closes an angle-bracket identifier
/// placeholder like `<value>` / `<id>` / `<RID>` / `<run-id>`: scan back over
/// identifier chars (`[A-Za-z0-9_-]`, at least one, plus any non-ASCII UTF-8
/// byte so non-English placeholders like `<同一key>` / `<セッションID>` are
/// recognized too) and find an opening `<`. Such prose is a placeholder token,
/// not a truncating redirect target. Hyphens are included because kebab-case
/// placeholders (`<run-id>`, `<session-id>`, `<pdo-unit-id>`) are pervasive in
/// this repo's prose; allowing them (and non-ASCII bytes) only matches when a
/// real opening `<` is found, so genuine redirects like `foo-bar>file` (no
/// preceding `<`) are still detected. Every byte `>= 0x80` is either a
/// multi-byte UTF-8 lead byte or a continuation byte (never a bare ASCII
/// delimiter like `<`/whitespace/`;`), so treating them all as
/// identifier-continuing never runs past a real `<` or swallows a delimiter —
/// `bytes` comes from a `&str` so this scan never lands mid-character.
fn is_angle_placeholder_close(bytes: &[u8], gt: usize) -> bool {
    let mut k = gt;
    while k > 0
        && (bytes[k - 1].is_ascii_alphanumeric()
            || bytes[k - 1] == b'_'
            || bytes[k - 1] == b'-'
            || bytes[k - 1] >= 0x80)
    {
        k -= 1;
    }
    // Require ≥1 identifier char between the `<` and the `>`.
    k < gt && k > 0 && bytes[k - 1] == b'<'
}

fn basename(tok: &str) -> &str {
    tok.rsplit('/').next().unwrap_or(tok)
}

/// Parse the FIRST shell word of `s`, honouring single quotes, double quotes and
/// backslash escapes, and return it with ONE level of quoting removed.
///
/// Whitespace-only tokenisation elsewhere in this file never unquotes, so a
/// command word written in any of the standard quoting forms (`\rm`, `"rm"`,
/// `'rm'`, `r""m`) survives as a literal that matches no rule arm and falls
/// through to Allow. This is the shared primitive that closes that hole; it is
/// also what makes a `-c` operand recoverable as a single word regardless of
/// what follows it on the line.
///
/// Deliberately NOT a full shell lexer: no expansion of `$VAR`, `${…}`,
/// `$(…)`/backticks, `~`, globs or history. See `normalized_command` for the
/// list of forms that remain out of reach and why.
fn first_shell_word(s: &str) -> Option<String> {
    let mut chars = s.chars().peekable();
    while chars.peek().is_some_and(|c| c.is_whitespace()) {
        chars.next();
    }
    let mut out = String::new();
    let mut saw_word = false;
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                // Single quotes: everything up to the next `'` is literal.
                saw_word = true;
                for d in chars.by_ref() {
                    if d == '\'' {
                        break;
                    }
                    out.push(d);
                }
            }
            '"' => {
                // Double quotes: only `\"`, `\\`, `\$` and '\`' are escapes.
                saw_word = true;
                while let Some(d) = chars.next() {
                    if d == '"' {
                        break;
                    }
                    if d == '\\' {
                        match chars.next() {
                            Some(e @ ('"' | '\\' | '$' | '`')) => out.push(e),
                            Some(e) => {
                                out.push('\\');
                                out.push(e);
                            }
                            None => out.push('\\'),
                        }
                    } else {
                        out.push(d);
                    }
                }
            }
            '\\' => {
                // Backslash escapes the next char (`\rm` -> `rm`, the standard
                // alias-bypass idiom); a trailing lone `\` is kept literal.
                saw_word = true;
                match chars.next() {
                    Some(d) => out.push(d),
                    None => out.push('\\'),
                }
            }
            c if c.is_whitespace() => break,
            c => {
                saw_word = true;
                out.push(c);
            }
        }
    }
    if saw_word {
        Some(out)
    } else {
        None
    }
}

/// Normalise a COMMAND WORD to the bare program name used for rule matching.
///
/// Applies, in order: one level of shell quoting/escaping removal
/// (`first_shell_word`) and then `basename`. Together these defend the forms
/// that are reachable in ordinary use and that all name the SAME program:
///
///   * `\rm`      — the standard alias-bypass idiom (verified bypass)
///   * `"rm"` / `'rm'` / `r""m` — quoted or split command words
///   * `/bin/rm`, `$HOME/bin/rm` (literal path part) — via `basename`
///   * `command rm`, `sudo rm`, `env rm`, … — via `command_index`'s wrapper skip
///
/// OUT OF REACH (deliberately not attempted — each needs runtime state we do
/// not have, and guessing would deny benign commands):
///   * `$(which rm)`, `` `which rm` ``, `$RM`, `${RM}` — command/parameter
///     substitution: the value is only known at execution time.
///   * user aliases and shell functions (`alias x='rm -rf'`) — defined in the
///     user's rc files, not visible from the tool call.
///   * `r$'m'` / `$'\x72m'` ANSI-C quoting and `%` -style expansions.
///   * a program reached through a symlink or a PATH shadow whose NAME is not
///     `rm` (e.g. `~/bin/cleanup` that execs `rm -rf`) — nothing in the command
///     line says so.
///
/// Operand tokens are also intentionally left un-normalised: unquoting them
/// would change glob/path semantics that `analyze_rm` reasons about.
fn normalized_command(tok: &str) -> String {
    let unquoted = first_shell_word(tok).unwrap_or_else(|| tok.to_string());
    basename(&unquoted).to_string()
}

fn is_assignment(tok: &str) -> bool {
    if let Some(eq) = tok.find('=') {
        let name = &tok[..eq];
        !name.is_empty()
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            && name
                .chars()
                .next()
                .map(|c| !c.is_ascii_digit())
                .unwrap_or(false)
    } else {
        false
    }
}

/// True if `cmd` is an exec-wrapper: a program whose own arguments end in a
/// COMMAND that it then runs, so the effective command word is BEHIND it.
fn is_exec_wrapper(cmd: &str) -> bool {
    matches!(
        cmd,
        "sudo"
            | "doas"
            | "nohup"
            | "env"
            | "command"
            | "time"
            | "nice"
            | "ionice"
            | "timeout"
            | "stdbuf"
            | "setsid"
            | "flock"
            | "chroot"
    )
}

/// Candidate indices of the effective command word, skipping leading `VAR=val`
/// assignments. Callers analyse EVERY candidate and deny if ANY of them is
/// destructive.
///
/// D3 (verified fail-open, fixed): this used to be `command_index` — a function
/// that computed the SINGLE index it believed was the command word by modelling
/// which of each wrapper's options take a separate value token
/// (`wrapper_flag_takes_value`) and how many positional operands the wrapper
/// consumes (`wrapper_leading_operands`). That model was wrong for GNU LONG
/// options, which accept `--opt VALUE` as two tokens: `wrapper_flag_takes_value`
/// returned false for every flag whose length was not exactly 2, commented "long
/// flags already carry their value inline". So the VALUE token broke the flag
/// loop, was eaten as the wrapper's leading operand, and the real command word
/// was misresolved — `timeout --kill-after 10 5 rm -rf /path`,
/// `chroot --userspec root /newroot rm -rf /path`, `stdbuf --output 0 rm …`,
/// `flock --wait 5 /tmp/l rm …`, `sudo --user root rm …`, `env --unset FOO rm …`
/// and `nice --adjustment 5 rm …` were all ALLOW. The short forms denied
/// correctly, which is why the tests missed it.
///
/// The fix deliberately does NOT extend the option model — that is the "reason
/// about token positions" pattern that has already rotted twice in this file,
/// and every long option added later would reopen the same gap. Both option
/// tables are DELETED. Instead, once a wrapper is seen, EVERY subsequent
/// non-empty token position becomes a candidate command word. The candidate set
/// is additive and evaluated deny-if-ANY, so whichever position the wrapper
/// really execs is guaranteed to be among them, however its options parse.
///
/// This mirrors the shape BG-1 already used for `-c` payloads
/// (see `dash_c_payloads`).
///
/// Widening justification: a candidate at a non-command position denies only if
/// that token is ITSELF a destructive command word with destructive operands
/// following it (e.g. an option value that literally reads `rm -rf <path>`).
/// That is a false positive, and a recoverable one — the acceptable side of this
/// module's fail-closed bias.
///
/// When the first non-assignment token is NOT a wrapper, exactly one candidate
/// is returned, so ordinary commands are unaffected.
fn command_candidates(tokens: &[&str]) -> Vec<usize> {
    let mut i = 0;
    while i < tokens.len() {
        let t = tokens[i];
        if t.is_empty() || is_assignment(t) {
            i += 1;
            continue;
        }
        if is_exec_wrapper(&normalized_command(t)) {
            // Every position behind the wrapper is a candidate.
            return (i + 1..tokens.len())
                .filter(|&j| !tokens[j].is_empty() && !opens_unclosed_quote(tokens[j]))
                .collect();
        }
        return vec![i];
    }
    Vec::new()
}

/// True when `tok` OPENS a quote it does not close within the same
/// whitespace-delimited token — i.e. the token is the first fragment of a
/// multi-word quoted STRING (`'rm` in `grep -rn 'rm -rf' src/`), not a command
/// word.
///
/// Tokenisation elsewhere splits on whitespace only, so such a fragment
/// unquotes to a bare `rm` and, as a D3 candidate position, would deny ordinary
/// work like `stdbuf --output 0 grep -rn 'rm -rf' src/` — the same false
/// positive CA-blastguard-016 fixed for `find -exec`.
///
/// Not a fail-open: excluding these fragments cannot hide a real payload,
/// because a quoted multi-word string is never a program that exists on disk,
/// and the ways such a string actually gets EXECUTED — `sh -c '…'`, `eval '…'`,
/// `flock -c '…'` — are all re-analysed by their own arms
/// (`dash_c_payloads`, the `eval` arm), which reassemble the full quoted word
/// across token boundaries rather than reading a single fragment. A properly
/// closed quoted command word (`sudo 'rm' -rf /path`) closes within its token
/// and therefore remains a candidate.
fn opens_unclosed_quote(tok: &str) -> bool {
    unclosed_quote_char(tok).is_some()
}

/// The quote character `tok` leaves open, if any.
///
/// Callers that must skip the WHOLE quoted string (not merely its first token)
/// need to know which quote to look for when finding the end — see
/// `unknown_wrapper_ask`.
fn unclosed_quote_char(tok: &str) -> Option<char> {
    let mut chars = tok.chars();
    match chars.next() {
        Some(q @ ('\'' | '"')) if !chars.any(|c| c == q) => Some(q),
        _ => None,
    }
}

/// Wrappers whose `-c` operand is a SHELL COMMAND STRING (run via `sh -c`),
/// not an opaque option value.
///
/// D2 (verified fail-open, fixed): `flock`'s `-c` / `--command` takes a shell
/// command string and executes it through `sh -c`, exactly like `bash -c`. It
/// used to be listed in `wrapper_flag_takes_value` as a value-taking option, so
/// the payload was SKIPPED as an opaque flag value and never analysed:
/// `flock /tmp/l -c 'rm -rf /Users/yuki/src'` and
/// `flock -c 'rm -rf /Users/yuki/src' /tmp/l` were both ALLOW. The payload is
/// now routed into the same `dash_c_payloads` re-analysis path used for shells.
fn takes_shell_command_flag(cmd: &str) -> bool {
    matches!(cmd, "flock")
}

fn is_short_flag(tok: &str) -> bool {
    tok.starts_with('-') && !tok.starts_with("--")
}

/// True if any short flag bundle in `rest` contains `ch`, or the long flag is set.
fn has_short(rest: &[&str], ch: char) -> bool {
    rest.iter().any(|t| is_short_flag(t) && t.contains(ch))
}

/// True if `tok` is a shell redirection token rather than an operand — either an
/// operator with its target attached (`2>&1`, `>&2`, `2>/dev/null`, `>file`) or a
/// bare operator (`>`, `>>`, `2>`, `&>`). Redirection is punctuation the shell
/// consumes itself; it is never an argument the command can act on.
fn is_redirect_token(tok: &str) -> bool {
    tok.contains('>') || tok.contains('<')
}

/// True if `tok` is a redirect operator with NO attached target (`>`, `>>`,
/// `2>`, `&>`), meaning the FOLLOWING token is its target and is likewise not a
/// command operand. Recognised by "made only of redirect punctuation/fd digits
/// AND ending in `>`" — `2>&1` and `>&2` end in a digit and carry their own
/// target, so they consume only themselves.
fn is_bare_redirect_op(tok: &str) -> bool {
    tok.ends_with('>')
        && tok
            .chars()
            .all(|c| matches!(c, '>' | '<' | '&' | '0'..='9'))
}

/// True if `rest` contains at least one OPERAND — a token that is neither one of
/// the command's own flags nor shell redirection punctuation. Operands are
/// identified exactly as `analyze_rm` identifies them (`!t.starts_with('-')`) so
/// the two stay in agreement about what counts as a destroyable target.
fn has_operand(rest: &[&str]) -> bool {
    let mut i = 0;
    while i < rest.len() {
        let t = rest[i];
        if t.is_empty() {
            i += 1;
        } else if is_bare_redirect_op(t) {
            // Operator plus the target token it owns.
            i += 2;
        } else if is_redirect_token(t) || t.starts_with('-') {
            i += 1;
        } else {
            return true;
        }
    }
    false
}

/// Shells that take a command line as a string argument (e.g. `sh -c "…"`).
fn is_shell(cmd: &str) -> bool {
    matches!(cmd, "sh" | "bash" | "zsh" | "ksh" | "dash")
}

/// Non-shell interpreters that run an inline program supplied as a string
/// argument (e.g. `python3 -c "…"`, `perl -e "…"`). Like a shell's `-c`, this
/// lets `find -exec <interp> -c "<payload>"` run an arbitrary destructive
/// command per match, slipping past the literal `rm` token scan.
///
/// CA-blastguard-008: recognizes versioned basenames too (e.g. `python3.12`,
/// `perl5.36`) by stripping a trailing `\d+(\.\d+)*` version suffix from the
/// token before matching against the known unversioned stems. Without this, a
/// versioned interpreter invocation (`find -exec python3.12 -c "…" \;`) slips
/// past the check entirely.
fn is_code_interpreter(cmd: &str) -> bool {
    matches!(
        strip_version_suffix(cmd),
        "python" | "python2" | "python3" | "perl" | "ruby" | "node" | "nodejs" | "php" | "lua"
    )
}

/// Strip a trailing version suffix (`\d+(\.\d+)*`, e.g. `3.12`, `5`, `2.7`)
/// from a command basename, returning the bare stem (`python3.12` -> `python`,
/// `perl5.36` -> `perl`). If there is no trailing digit run, returns `cmd`
/// unchanged.
fn strip_version_suffix(cmd: &str) -> &str {
    let bytes = cmd.as_bytes();
    let mut end = bytes.len();
    loop {
        // Find the start of a trailing digit run.
        let mut start = end;
        while start > 0 && bytes[start - 1].is_ascii_digit() {
            start -= 1;
        }
        if start == end {
            // No trailing digits at `end` — nothing more to strip.
            break;
        }
        end = start;
        // A `.` immediately before a digit run continues the version suffix
        // (e.g. the `.12` in `python3.12`) — strip it too and keep looking.
        if end > 0 && bytes[end - 1] == b'.' {
            end -= 1;
        } else {
            break;
        }
    }
    &cmd[..end]
}

/// Inline-code eval flags for the interpreters in [`is_code_interpreter`]
/// (`python -c`, `perl -e`/`-E`, `ruby -e`, `node -e`/`--eval`/`-p`, `php -r`).
/// A script-file argument (no such flag) is deliberately NOT matched, so
/// `find -exec python3 script.py` is left alone.
///
/// CA-blastguard-007: also recognizes a combined/stacked short-flag token
/// (e.g. `-ic`, python's `-i` interactive flag stacked with `-c`) that
/// *contains* one of the single-character eval flags (`c`, `e`, `r`, `p`) as
/// one of its bundled option letters. This is deliberately restricted to
/// short-flag tokens (`-xyz`, not `--xyz`) so long flags like `--color` are
/// never mistaken for an eval flag (avoiding false positives on unrelated
/// long options that merely contain a letter `c`/`e`/`r`/`p`).
fn is_inline_eval_flag(tok: &str) -> bool {
    if matches!(tok, "-c" | "-e" | "-E" | "-r" | "-p" | "--eval" | "--print") {
        return true;
    }
    if is_short_flag(tok) {
        // Bundled short flags, e.g. `-ic` = `-i` + `-c`. Match on the
        // lowercase eval-flag letters only (`E` is intentionally excluded
        // here as a bundled char: it is Ruby/Perl's `-E`, distinct enough
        // from common bundles that we keep the stacked check to the
        // clearly-common `-c`/`-e`/`-r`/`-p` letters to stay precise).
        return tok[1..].chars().any(|c| matches!(c, 'c' | 'e' | 'r' | 'p'));
    }
    false
}

/// Strip one layer of matching surrounding quotes from a reconstructed
/// argument string. Tokenisation has already split on whitespace, so a quoted
/// payload like `"rm -rf /"` arrives as `"rm -rf /"` — peel the wrapper so the
/// inner command line can be re-analysed.
fn strip_wrapping_quotes(s: &str) -> &str {
    let s = s.trim();
    let bytes = s.as_bytes();
    if bytes.len() >= 2 {
        let (first, last) = (bytes[0], bytes[bytes.len() - 1]);
        if (first == b'"' || first == b'\'') && first == last {
            return &s[1..s.len() - 1];
        }
    }
    s
}

/// For `<shell> … -c <payload>`, return every candidate payload command line to
/// re-analyse. Returns an empty vec when there is no `-c` flag (e.g.
/// `bash script.sh`, whose file we cannot inspect).
///
/// CA-blastguard-012 (verified bypass): this used to JOIN all words after `-c`
/// and then peel quotes only when the JOINED string began and ended with the
/// same quote char. Appending ANY trailing token defeated that —
/// `sh -c "rm -rf /some/path" --help` (or `; true`, or a bare `arg0`) left the
/// last char belonging to the trailing token, nothing was peeled, the payload's
/// command word parsed as the literal `"rm`, and analysis gave up: ALLOW.
/// Trailing tokens are ordinary shell usage (`sh -c '…' scriptname arg1`), where
/// they become `$0`, `$1`, … and are NOT executed — so the operand must be read
/// as a single properly-quoted shell WORD, independent of what follows it.
///
/// Two candidates are returned, both analysed, deny-if-either:
///   1. the first shell word after `-c` — the real, faithful shell semantics;
///   2. the legacy join-then-peel string, when it differs — so an UNQUOTED
///      multi-word payload (`sh -c rm -rf /path`) keeps being denied exactly as
///      before. (Real `sh` would only run `rm` there, but the historical
///      denial is the conservative side of the trade and costs no realistic
///      false positive.)
///
/// CA-blastguard-014 (verified bypass, fail-open): the word after `-c` is not
/// necessarily the payload. A shell keeps parsing OPTIONS until the first
/// non-option operand, so `bash -c -- 'rm -rf /some/path'`, `sh -c -- "rm …"`
/// and `bash -c -x 'rm …'` all really execute the payload while the old code
/// read `--` / `-x` as the "first shell word" and gave up: ALLOW (verified:
/// `bash -c -- 'echo RAN'`, `sh -c -- 'echo RAN'` and `bash -c -x 'echo RAN'`
/// all print RAN).
///
/// BG-1 (incomplete fix): the CA-blastguard-014 scan skipped `-`-prefixed
/// tokens and stopped at the first token that did not start with `-`. For a
/// VALUE-TAKING option that token is the option's ARGUMENT, not the payload —
/// `bash -c -o pipefail 'rm -rf /some/path'` stopped on `pipefail`, and `+o` /
/// `+O` were not recognised as options at all (`bash -c +o history 'rm …'`).
/// All of these really execute the payload; all were ALLOW. The old docstring's
/// claim to "mirror the shell's own option parsing" was therefore false.
///
/// The fix deliberately does NOT model option parsing more precisely — that is
/// the "reason about token positions" approach that has regressed this file
/// three rounds running, and every value-taking option added later (`-o`, `-O`,
/// `+o`, `--rcfile`, `--init-file`, …) would reopen the same gap. Instead it
/// pushes a candidate for EVERY position after `-c`. The candidate set is
/// additive and evaluated deny-if-ANY, so extra candidates can only ever WIDEN
/// detection: whichever position the real shell picks as the command string is
/// guaranteed to be among them, however the intervening options parse.
///
/// Widening justification (no false positive of consequence): a candidate at a
/// non-payload position is re-analysed by the full pipeline, so it denies only
/// if that text is ITSELF a destructive command line. The residue is a token
/// that is both an option's value and a destructive command line (e.g.
/// `bash -c 'echo hi' 'rm -rf /path'`, where the second string is `$0`, not
/// code) — a deny there is a false positive, but a recoverable one, and it is
/// the deliberate side of this module's fail-closed bias for shell-eval
/// wrappers.
///
/// Fail-open condition: the real payload's first shell word is not derivable
/// from any suffix of `rest` — e.g. the payload arrives through a variable
/// (`bash -c "$CMD"`) or is assembled at runtime. That is unchanged by this fix
/// and out of reach of any static analysis of the command line.
fn dash_c_payloads(rest: &[&str]) -> Vec<String> {
    let Some(pos) = rest
        .iter()
        .position(|t| is_short_flag(t) && t.contains('c'))
    else {
        return Vec::new();
    };
    let mut out = payloads_after(rest, pos);
    // Glued short form `-c<payload>` (value in the SAME token, after `c`):
    // `payloads_after` only sees LATER tokens, so `flock /l -c'rm -rf /'` left
    // the payload unanalysed — the twin of the `--command=` attached-form
    // bypass (138607bc). By getopt convention an option that takes an argument
    // is last in a short bundle, so everything after the first `c` is its value.
    if let Some(cidx) = rest[pos].find('c') {
        let inline = &rest[pos][cidx + 1..];
        out.extend(inline_command_payloads(rest, pos, inline));
    }
    out
}

/// The `dash_c_payloads` candidate set for a command-string flag already located
/// at index `pos`. Split out so callers with a different spelling of the flag
/// (e.g. flock's long form `--command`) reuse the identical candidate logic
/// rather than duplicating it.
fn payloads_after(rest: &[&str], pos: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut push_first_word = |from: usize| {
        if from >= rest.len() {
            return;
        }
        let joined = rest[from..].join(" ");
        if let Some(word) = first_shell_word(&joined) {
            // The `!word.is_empty()` guard matters: `sh -c ''` yields
            // `Some("")`, which must not be re-analysed as an empty command.
            if !word.is_empty() && !out.contains(&word) {
                out.push(word);
            }
        }
    };
    // Candidate 1: EVERY position after `-c`. This subsumes the historical
    // `pos + 1` candidate (so nothing previously denied can regress to Allow)
    // and any option-skipping heuristic, without needing to know which of the
    // shell's own options take a separate value token.
    for from in pos + 1..rest.len() {
        push_first_word(from);
    }

    // Candidate 2: legacy join-then-peel, unchanged.
    let joined = rest[pos + 1..].join(" ");
    let legacy = strip_wrapping_quotes(&joined);
    if !legacy.is_empty() && !out.iter().any(|p| p == legacy) {
        out.push(legacy.to_string());
    }
    out
}

/// Payloads for a command-string flag whose value is GLUED INSIDE the flag
/// token — the long attached form `--command=<payload>` (value after `=`) and
/// the short glued form `-c<payload>` (value after the `c`). `inline` is the
/// in-token remainder already peeled off by the caller; the payload may also
/// spill into following tokens once the line is split on whitespace
/// (`--command='rm -rf /'` → `--command='rm`, `-rf`, `/'`), so we splice the
/// inline remainder back in front of `rest[pos + 1..]` and reuse the identical
/// candidate logic. `payloads_after` alone only inspects tokens strictly AFTER
/// the flag, so without this the inline value was never analysed — the
/// depth-0 Allow bypass this closes (138607bc).
fn inline_command_payloads(rest: &[&str], pos: usize, inline: &str) -> Vec<String> {
    // An empty inline means the value is NOT glued (`--command`, `-c`, or a
    // bundle like `-nc` with the value in the next token) — `payloads_after`
    // already covers that, so there is nothing extra to reconstruct here.
    if inline.is_empty() {
        return Vec::new();
    }
    // A dummy flag at index 0 lets us reuse `payloads_after`'s "everything after
    // the flag" candidate logic with the inline value spliced in as the first
    // operand token.
    let mut synthetic = vec!["--x", inline];
    synthetic.extend_from_slice(&rest[pos + 1..]);
    payloads_after(&synthetic, 0)
}

fn analyze_segment(seg: &str, depth: usize) -> Decision {
    let tokens: Vec<&str> = seg.split_whitespace().collect();
    let mut acc = VerdictAcc::default();

    // D2: a wrapper that takes a shell command string via `-c` (flock) runs that
    // string through `sh -c`. Re-analyse it exactly like a shell's own `-c`
    // payload. Scanned position-independently so both `flock FILE -c '…'` and
    // `flock -c '…' FILE` are covered.
    if depth < MAX_SHELL_DEPTH {
        for (i, t) in tokens.iter().enumerate() {
            if takes_shell_command_flag(&normalized_command(t)) {
                let rest = &tokens[i + 1..];
                // Both spellings of flock's command flag: the short `-c` (via
                // `dash_c_payloads`) and the long `--command`/`--command=…`.
                let mut payloads = dash_c_payloads(rest);
                if let Some(pos) = rest
                    .iter()
                    .position(|t| *t == "--command" || t.starts_with("--command="))
                {
                    // Detached long form `--command <payload>`: payload is the
                    // token(s) AFTER the flag.
                    payloads.extend(payloads_after(rest, pos));
                    // Attached long form `--command=<payload>`: the value is
                    // glued to the flag token by `=`. Reconstruct it so the
                    // same candidate logic applies — otherwise the inline value
                    // is never extracted and `flock /l --command='rm -rf /'`
                    // stayed Allow at depth 0 (138607bc).
                    if let Some(eq) = rest[pos].find('=') {
                        let inline = &rest[pos][eq + 1..];
                        payloads.extend(inline_command_payloads(rest, pos, inline));
                    }
                }
                for payload in payloads {
                    // Shell-eval position: an unresolvable command word here is
                    // an Ask, not a silent Allow.
                    if let Some(deny) = acc.record(analyze_shell_payload(&payload, depth)) {
                        return deny;
                    }
                }
            }
        }
    } else {
        // Cap reached: the recursion above did NOT run, so this segment was not
        // fully analysed. Record the unfinished-analysis Ask into `acc` rather
        // than returning early, so a Deny found by a later non-recursive rule in
        // this same function still outranks it (see `VerdictAcc::record`).
        let _ = acc.record(depth_exhausted());
    }

    // D3: analyse EVERY candidate command-word position; deny if ANY is
    // destructive. See `command_candidates`.
    for idx in command_candidates(&tokens) {
        if let Some(deny) = acc.record(analyze_command_at(&tokens, idx, depth)) {
            return deny;
        }
    }

    // ASK-2: an UNRECOGNISED wrapper standing in front of a destructive command
    // line. See `unknown_wrapper_ask`.
    if let Some(deny) = acc.record(unknown_wrapper_ask(&tokens, depth)) {
        return deny;
    }

    acc.finish()
}

/// True when `cmd` is a command word this module has an actual opinion about —
/// either a rule arm in `analyze_command_at`, a re-analysis arm, or a wrapper
/// whose payload is already followed.
///
/// Kept in lockstep with `analyze_command_at`'s `match` and the arms above it;
/// `unknown_wrapper_ask_covers_every_unrecognized_head` pins that a head this
/// returns false for really does fall through to the catch-all Allow arm.
fn is_recognized_command(cmd: &str) -> bool {
    // Rule arms in `analyze_command_at`'s match (the `mkfs` arm is a prefix).
    matches!(
        cmd,
        "rm" | "git"
            | "find"
            | "xargs"
            | "truncate"
            | "shred"
            | "dd"
            | "chmod"
            | "chown"
            | "tee"
            | "eval"
            | "exec"
            | "source"
            | "."
            | "-exec"
            | "-execdir"
            | "-ok"
            | "-okdir"
    ) || cmd.starts_with("mkfs")
        // Re-analysis / wrapper arms.
        || is_shell(cmd)
        || is_code_interpreter(cmd)
        || is_exec_wrapper(cmd)
        || takes_shell_command_flag(cmd)
}

/// ASK-2: an unrecognised command word standing in front of something that
/// parses as a DESTRUCTIVE command line.
///
/// `command_candidates` only widens the candidate set behind a KNOWN wrapper
/// name (`sudo`, `env`, `timeout`, …). Behind anything else it returns the head
/// position alone, so `my-cleanup-wrapper rm -rf /some/path` resolved to the
/// single word `my-cleanup-wrapper`, matched no rule arm, and was ALLOWED with
/// its payload never examined. The wrapper list cannot be completed — a local
/// script, a `just` recipe or a `Makefile` shim is a wrapper blastguard has
/// never heard of.
///
/// It is NOT a Deny: unlike `sudo`, we do not know that this program execs its
/// arguments at all. It genuinely is the unknown middle, so it asks.
///
/// Narrowness, which is the whole difficulty here:
///   * the head must be UNRECOGNISED — every rule arm and every known wrapper
///     has already had its say above, and a Deny from there outranks this;
///   * only ONE tail is examined: the first token after the head that is not a
///     flag, not empty, and not the opening fragment of a quoted STRING
///     (`opens_unclosed_quote`, so `mytool -x 'rm -rf' file` reads `'rm` as the
///     DATA it is and never asks);
///   * that tail must itself analyse to a full DENY. `mytool report.txt`,
///     `cargo test`, `npm run build`, `gh pr list` all analyse to Allow and are
///     silent. Only a genuinely destructive command line behind an unknown word
///     asks.
///
/// Examining exactly one tail (rather than every position, as
/// `command_candidates` does behind a known wrapper) also keeps the fan-out at
/// one child per segment. That matters: fanning out per position is what made
/// the recursion tree exponential in D4.
///
/// Missing an ask because the real payload sat at a position this scan skipped
/// is not a regression — that case was, and remains, the pre-existing Allow.
fn unknown_wrapper_ask(tokens: &[&str], depth: usize) -> Decision {
    if depth >= MAX_SHELL_DEPTH {
        return depth_exhausted();
    }
    // Resolve the head exactly as `analyze_segment` did.
    let candidates = command_candidates(tokens);
    // Behind a known wrapper `command_candidates` returns many positions; this
    // rule is only about the single-candidate (non-wrapper head) shape.
    let [idx] = candidates[..] else {
        return Decision::Allow;
    };
    if is_recognized_command(&normalized_command(tokens[idx])) {
        return Decision::Allow;
    }
    // First plausible command-line start behind the unknown head. Redirect
    // punctuation is skipped here and truncates the tail below; see the note on
    // `end` for why.
    //
    // A token that OPENS an unclosed quote begins a quoted STRING, and the
    // whole string — not just that first token — is data. Skipping only the
    // opening token left the scan free to resume INSIDE the string, so
    // `echo 'eval rm -rf /'` skipped `'eval` and then started the tail at `rm`,
    // asking about `echo` over text that is an argument, never a command. So
    // `in_quote` runs to the token that closes the quote.
    let mut in_quote: Option<char> = None;
    let mut start = None;
    for (j, &t) in tokens.iter().enumerate().skip(idx + 1) {
        if let Some(q) = in_quote {
            // The closing token ends the string; it is still data itself. An
            // unterminated quote never closes, so the whole rest of the line is
            // data and no ask is raised — the safe direction here, since an
            // unbalanced quote means the shell would not run it as written.
            if t.contains(q) {
                in_quote = None;
            }
            continue;
        }
        if let Some(q) = unclosed_quote_char(t) {
            in_quote = Some(q);
            continue;
        }
        if t.is_empty() || t.starts_with('-') || is_redirect_token(t) {
            continue;
        }
        start = Some(j);
        break;
    }
    let Some(start) = start else {
        return Decision::Allow;
    };
    // The tail STOPS at the first redirect token (and at a bare operator's
    // target, which is punctuation too). Redirects belong to the outer command
    // line and were already judged in full by the caller before this rule ran;
    // re-analysing them here attributes the OUTER line's redirect to the head,
    // which turned `cargo test 2>&1`, `make >&2` and `echo x >&2` into asks
    // about `cargo` / `make` / `echo`. Re-joining tokens with a single space
    // also re-serialises redirect punctuation into a shape the redirect
    // classifier reads differently from the original text, so the re-parse was
    // not even asking about the same redirect.
    //
    // Dropping them costs nothing: a destructive REDIRECT is a deny from the
    // outer analysis regardless of what the head is, so nothing is missed.
    let end = tokens
        .iter()
        .enumerate()
        .skip(start)
        .find(|(_, t)| is_redirect_token(t))
        .map_or(tokens.len(), |(j, _)| j);
    let tail = tokens[start..end].join(" ");
    if tail.trim().is_empty() {
        return Decision::Allow;
    }
    // Only a positively DESTRUCTIVE tail asks. An Ask from the tail is not
    // propagated: it would already have been reported by whichever construct
    // produced it if that construct were reachable, and re-reporting it here
    // would ask about a wrapper we have no evidence executes anything.
    if detect_bash(&tail, depth + 1).is_deny() {
        let head = tokens[idx];
        return Decision::ask(format!(
            "`{head}` is not a command blastguard recognises, and what follows it parses as a destructive command line — blastguard cannot tell whether `{head}` runs it"
        ));
    }
    Decision::Allow
}

fn analyze_command_at(tokens: &[&str], idx: usize, depth: usize) -> Decision {
    // CA-blastguard-013 (verified bypass): the command word must be UNQUOTED
    // before basename matching. `\rm -rf /some/path` (the standard alias-bypass
    // idiom) and `"rm" -rf /some/path` previously matched no rule arm and fell
    // through to Allow. See `normalized_command` for what this covers and what
    // stays out of reach.
    let cmd = normalized_command(tokens[idx]);
    let cmd = cmd.as_str();
    let rest = &tokens[idx + 1..];
    // Collects verdicts from the shell-evaluation re-analysis arms below, so an
    // Ask raised there survives the `match` that follows while still losing to
    // any Deny (see `VerdictAcc`).
    let mut acc = VerdictAcc::default();

    // A *genuine* help invocation never destroys anything — but only when the
    // help flag is the command's ONLY argument.
    //
    // CA-blastguard-011 (verified bypass): the previous rule short-circuited on
    // `rest.iter().any(is_help_flag)`, so appending `--help` anywhere disabled
    // every Bash rule. That assumed the flag makes the command print help and
    // exit, which is false for the BSD userland on macOS: BSD `rm` has no
    // `--help` option, so the token is taken as a plain filename operand and
    // `rm -rf <dir> --help` exits 0 after DELETING `<dir>`. The same holds for
    // any other operand ordering, because a command that does not know the flag
    // treats it as data, and a command that does know it may still have already
    // consumed the destructive operands (GNU getopt permutes, BSD getopt stops
    // at the first non-option — neither is safe to assume).
    //
    // The property that actually makes a help invocation safe is that there is
    // no OPERAND to destroy — not that the help flag is the only token.
    //
    // CA-blastguard-015 (false positive introduced by the `rest.len() == 1`
    // form): ANY second token re-enabled the unconditional-deny arms, including
    // a pure REDIRECT that carries no operand at all. `shred --help 2>&1 | head`,
    // `truncate --help 2>/dev/null` and `tee --help 2>&1` are ordinary usage,
    // and blastguard is a hard block whose stated bias is that ambiguity falls
    // through to Allow — denying them is exactly the kind of interference the
    // module header forbids.
    //
    // Narrowing justification (no fail-open): the operand test is computed the
    // same way `analyze_rm` computes operands (`!t.starts_with('-')`), so any
    // destructive target still counts and is still denied — `rm -rf /path --help`
    // (operand `/path`) and `rm --help *` (operand `*`) both keep their DENY,
    // which is the whole point of CA-blastguard-011 above. Only redirect
    // operators and their targets are excluded, and those cannot themselves be
    // destroyed by the command.
    //
    // BG-2: the previous version of this comment claimed a truncating redirect
    // "is already denied earlier in `detect_bash` (rule 2)". That was FALSE for
    // space-separated `N>` with N != 1 (`shred --help 2> /some/path`), because
    // rule 2 skipped every non-stdout fd — so this skip really did open a hole.
    // `redirect_targets` no longer has that fd-based skip: EVERY `N> target`
    // (any fd, attached or space-separated) is now read as truncating and
    // denied in rule 2 before segment analysis runs. What `has_operand` skips
    // is therefore only what rule 2 has already vetted (a truncating target it
    // judged safe) or what cannot truncate at all — append `>>`, fd dups
    // (`2>&1`), and input redirects (`<`). With no operand left, there is
    // nothing to
    // destroy whichever way the command parses the flag (GNU getopt permutes,
    // BSD getopt stops at the first non-option — neither assumption is needed).
    if rest.iter().any(|t| *t == "--help" || *t == "-h") && !has_operand(rest) {
        return Decision::Allow;
    }

    // Shell-evaluation wrappers that would otherwise smuggle a destructive
    // command past the per-command analysis. We re-analyse the *inline*
    // command line they evaluate; an opaque file argument (e.g.
    // `source .venv/bin/activate`) re-analyses to a harmless path and stays
    // allowed, preserving the no-false-positive bias.
    if depth < MAX_SHELL_DEPTH {
        // `eval`/`exec`/`source`/`.` run their remaining words as a command.
        // ASK-1: this is a shell-EVALUATION position, so `eval "$CMD"` asks
        // rather than falling through unanalysed.
        if matches!(cmd, "eval" | "exec" | "source" | ".") && !rest.is_empty() {
            let joined = rest.join(" ");
            let inline = strip_wrapping_quotes(&joined);
            if let Some(deny) = acc.record(analyze_shell_payload(inline, depth)) {
                return deny;
            }
        }
        // `sh -c "<payload>"` and friends evaluate the `-c` argument.
        // ASK-1: likewise a shell-evaluation position — `bash -c "$CMD"` asks.
        if is_shell(cmd) {
            for payload in dash_c_payloads(rest) {
                if let Some(deny) = acc.record(analyze_shell_payload(&payload, depth)) {
                    return deny;
                }
            }
        }
    } else {
        // Cap reached: the recursion above did NOT run, so this segment was not
        // fully analysed. Record the unfinished-analysis Ask into `acc` rather
        // than returning early, so a Deny found by a later non-recursive rule in
        // this same function still outranks it (see `VerdictAcc::record`).
        let _ = acc.record(depth_exhausted());
    }

    let verdict = match cmd {
        // BG-3 (second instance): `split_segments` splits on `;`, so the `\;`
        // terminator of a `find -exec` CUTS THE LINE before `analyze_find` ever
        // sees it. `find . -name x -exec grep rm {} \; -exec rm -rf {} \;`
        // reaches this function as three segments, and the second one has
        // `-exec` as its command word — matching no rule, so the destructive
        // `-exec rm` was ALLOW while the `+`-terminated twin was correctly
        // denied. Recognising a segment whose command word is an exec predicate
        // and routing it back through `analyze_find` restores the multi-exec
        // scan for the `\;` form without depending on where the split fell.
        "-exec" | "-execdir" | "-ok" | "-okdir" => analyze_find(&tokens[idx..], depth),
        "rm" => analyze_rm(rest),
        "git" => analyze_git(rest),
        "find" => analyze_find(rest, depth),
        "xargs" => analyze_xargs(rest, depth),
        "truncate" => Decision::deny("truncate can shrink a file to zero bytes"),
        "shred" => Decision::deny("shred destroys file contents irreversibly"),
        "dd" => {
            if rest.iter().any(|t| t.starts_with("of=")) {
                Decision::deny("dd with of= writes raw bytes over a device/file")
            } else {
                Decision::Allow
            }
        }
        "chmod" => {
            if has_short(rest, 'R') || rest.contains(&"--recursive") {
                Decision::deny("recursive chmod re-permissions a whole tree")
            } else {
                Decision::Allow
            }
        }
        "chown" => {
            if has_short(rest, 'R') || rest.contains(&"--recursive") {
                Decision::deny("recursive chown re-owns a whole tree")
            } else {
                Decision::Allow
            }
        }
        // CA-blastguard-009: `tee FILE` (no -a/--append) truncates/overwrites
        // FILE identically to a `>` redirect. Append-mode tee is safe and
        // stays Allow.
        "tee" => {
            if rest.iter().any(|t| *t == "-a" || *t == "--append") {
                Decision::Allow
            } else {
                Decision::deny(
                    "tee without -a/--append truncates and overwrites its target file(s)",
                )
            }
        }
        // CA-blastguard-006: a bare top-level command-interpreter invocation
        // with an inline-eval flag (`python3 -c "…"`, no `find` wrapper) is
        // just as dangerous as the same payload wrapped in `find -exec`/`-ok`
        // (handled in analyze_find) — it can run an arbitrary destructive
        // command. Deny it here too, not only inside the find-exec path.
        cmd if is_code_interpreter(cmd) && rest.iter().any(|t| is_inline_eval_flag(t)) => {
            Decision::deny(
                "a code interpreter invoked with an inline-eval flag can run an arbitrary destructive command",
            )
        }
        other => {
            if other.starts_with("mkfs") {
                Decision::deny("mkfs formats a filesystem, destroying all data")
            } else {
                Decision::Allow
            }
        }
    };
    // A Deny from the rule arms wins outright; otherwise an Ask banked by the
    // shell-evaluation arms above is reported instead of being dropped.
    match acc.record(verdict) {
        Some(deny) => deny,
        None => acc.finish(),
    }
}

/// True when `tok` contains an unescaped shell glob metacharacter (`*`, `?`,
/// `[`) — i.e. it is a wildcard pattern that can expand to many paths, not a
/// single literal filename.
fn has_glob_meta(tok: &str) -> bool {
    let mut escaped = false;
    for c in tok.chars() {
        if escaped {
            escaped = false;
            continue;
        }
        match c {
            '\\' => escaped = true,
            '*' | '?' | '[' => return true,
            _ => {}
        }
    }
    false
}

fn analyze_rm(rest: &[&str]) -> Decision {
    let recursive = rest
        .iter()
        .any(|t| (is_short_flag(t) && (t.contains('r') || t.contains('R'))) || *t == "--recursive");
    let operands: Vec<&str> = rest
        .iter()
        .filter(|t| !t.starts_with('-'))
        .copied()
        .collect();
    let wildcard = operands.iter().any(|o| has_glob_meta(o));

    if !recursive && !wildcard {
        // Single, non-recursive rm of named files — below the destructive bar.
        return Decision::Allow;
    }

    // Destructive shape. Exempt only when every operand is a known, *literal*
    // config file. A wildcard operand (`*.toml`, `*.lock`) must never qualify:
    // it self-matches the config globs and would otherwise re-open the gate for
    // an unbounded wildcard delete.
    if !operands.is_empty()
        && operands
            .iter()
            .all(|o| !has_glob_meta(o) && exclude::is_config_file(o))
    {
        return Decision::Allow;
    }

    if recursive {
        Decision::deny("recursive rm (-r) can delete an entire directory tree")
    } else {
        Decision::deny("rm with a wildcard can delete many files at once")
    }
}

/// Index in `rest` of the git subcommand token, skipping git's global options
/// AND the separate-token value of a value-taking global option. Without this,
/// a naive "first non-dash token" scan lands on the VALUE of a value-taking
/// global option (`git -C DIR reset --hard`, `git -c user.name=x clean -fd`,
/// `git --git-dir PATH checkout --force`), misreads it as the subcommand, and
/// every git deny falls through to Allow — a fail-open bypass (CA-blastguard-07).
/// Mirrors `xargs_command_start`: `--` ends option parsing, a value-taking
/// global option in *separate* form consumes the next token too. Returns None
/// when no subcommand token follows (bare `git`/only global flags — nothing to
/// analyse).
fn git_subcommand_index(rest: &[&str]) -> Option<usize> {
    // Global options whose value is the NEXT token when given in *separate*
    // form (`-C DIR`, `-c k=v`, `--git-dir PATH`). Stuck/`=`-joined forms
    // (`-C/path`, `--git-dir=path`) embed the value and consume only themselves,
    // so they are handled by the generic "other `-`-prefixed token" arm below.
    const VALUE_SEPARATE: &[&str] = &[
        "-C",
        "-c",
        "--git-dir",
        "--work-tree",
        "--namespace",
        "--super-prefix",
        "--config-env",
    ];
    let mut i = 0;
    while i < rest.len() {
        let t = rest[i];
        if t == "--" {
            // `--` ends option parsing: the next token is the subcommand.
            return if i + 1 < rest.len() {
                Some(i + 1)
            } else {
                None
            };
        }
        if !t.starts_with('-') {
            return Some(i); // first non-flag token = the subcommand
        }
        // A value-taking global option in separate form consumes the next token
        // too; any other `-`-prefixed token (stuck/`=`-joined/boolean) is self-
        // contained.
        i += if VALUE_SEPARATE.contains(&t) { 2 } else { 1 };
    }
    None
}

fn analyze_git(rest: &[&str]) -> Decision {
    let idx = match git_subcommand_index(rest) {
        Some(i) => i,
        None => return Decision::Allow,
    };
    let sub = normalized_command(rest[idx]);
    match sub.as_str() {
        "clean" => {
            let has_f = has_short(rest, 'f') || rest.contains(&"--force");
            let has_d = has_short(rest, 'd');
            let has_x = has_short(rest, 'x');
            if has_f && (has_d || has_x) {
                Decision::deny("git clean -f with -d/-x deletes untracked files & dirs")
            } else {
                Decision::Allow
            }
        }
        "reset" => {
            if rest.contains(&"--hard") {
                Decision::deny("git reset --hard discards working-tree changes")
            } else {
                Decision::Allow
            }
        }
        "checkout" => {
            if rest.contains(&"--force") || has_short(rest, 'f') {
                return Decision::deny("git checkout --force discards working-tree changes");
            }
            if let Some(pos) = rest.iter().position(|t| *t == "--") {
                if rest[pos + 1..].iter().any(|t| *t == "." || *t == "./") {
                    return Decision::deny("git checkout -- . discards all working-tree changes");
                }
            }
            Decision::Allow
        }
        "restore" => {
            // `git restore <path>` / `git restore .` overwrites the working tree
            // from the index (or a source), discarding uncommitted changes — the
            // same hazard as the already-denied `git checkout -- .` form.
            // `git restore --staged <path>` (without `--worktree`) only unstages
            // and leaves the working tree intact, so it stays allowed.
            let staged = rest.contains(&"--staged") || has_short(rest, 'S');
            let worktree = rest.contains(&"--worktree") || has_short(rest, 'W');
            if staged && !worktree {
                Decision::Allow
            } else {
                Decision::deny("git restore discards working-tree changes")
            }
        }
        "stash" => {
            // `git stash clear` drops every stash entry and `git stash drop`
            // drops one — both irreversibly delete stashed work, the same
            // hazard class as the working-tree-discard forms above. Every
            // other subcommand (list/show/push/save/pop/apply/branch and the
            // bare `git stash` push) is non-destructive, so only the two
            // discard forms are denied. The stash subcommand is the first
            // non-flag token *after* the resolved `stash` token (`idx`) — not a
            // fresh naive scan, which under a global prefix would land on the
            // prefix value (`git -C DIR stash clear`).
            let subcmd = rest[idx + 1..]
                .iter()
                .find(|t| !t.starts_with('-'))
                .map(|t| normalized_command(t))
                .unwrap_or_default();
            if subcmd == "clear" || subcmd == "drop" {
                Decision::deny("git stash clear/drop irreversibly deletes stashed changes")
            } else {
                Decision::Allow
            }
        }
        _ => Decision::Allow,
    }
}

fn analyze_find(rest: &[&str], depth: usize) -> Decision {
    if rest.contains(&"-delete") {
        return Decision::deny("find -delete removes every matching file");
    }
    let mut acc = VerdictAcc::default();
    // Every -exec/-execdir/-ok/-okdir on the line, not just the first: a benign
    // early `-exec grep …` must not hide a later `-exec rm …`.
    for pos in rest
        .iter()
        .enumerate()
        .filter(|(_, t)| matches!(**t, "-exec" | "-execdir" | "-ok" | "-okdir"))
        .map(|(i, _)| i)
    {
        let tail = &rest[pos + 1..];
        // The command run for each match starts right after -exec/-ok, modulo
        // the same leading `VAR=val` assignments and benign wrappers
        // (`sudo`, `env`, `command`, …) that `analyze_segment` skips — resolving
        // it through `command_index` is what keeps `-exec sudo rm …` and
        // `-exec env sh -c …` in reach. A shell there
        // (`find … -exec sh -c "rm …"`) can run any destructive command.
        // -ok/-okdir are the interactive-confirmation twins of -exec/-execdir:
        // they run the same arbitrary per-match command, just gated behind a
        // y/n prompt first — that prompt does not make the payload any less
        // destructive. Normalised like every other command-word site, so
        // `-exec \rm` / `-exec "sh" -c …` cannot slip past on quoting alone.
        // D3: every candidate command-word position behind a wrapper is checked,
        // not just one computed index. See `command_candidates`.
        for ci in command_candidates(tail) {
            let c = normalized_command(tail[ci]);
            let c = c.as_str();
            if is_shell(c) {
                return Decision::deny(
                    "find -exec on a shell can run an arbitrary destructive command per match",
                );
            }
            // A non-shell interpreter with an inline-eval flag (`python3 -c`,
            // `perl -e`, `node -e`, …) is equally dangerous: the payload runs
            // per match. (The nested shell's own `-c` payload is already covered
            // by the `is_shell` arm above, which denies the whole invocation
            // without needing to look at the payload text at all.)
            if is_code_interpreter(c) && tail[ci..].iter().any(|t| is_inline_eval_flag(t)) {
                return Decision::deny(
                "find -exec on a code interpreter with an inline-eval flag can run an arbitrary destructive command per match",
            );
            }
            // CA-blastguard-016 (false positive): this used to normalise EVERY
            // token in `rest` and deny if any of them came out as `rm`, so quoted
            // DATA matched — `find . -name '*.md' -exec grep -l 'rm' {} \;`, i.e.
            // searching for the literal string `rm`, is completely normal work and
            // was hard-blocked. Only a plausible COMMAND position is normalised
            // now.
            //
            // Narrowing justification (no fail-open): the token this checks is the
            // one `find` actually execs, resolved through the same wrapper-skipping
            // logic used everywhere else, so every genuinely destructive shape
            // (`-exec rm`, `-exec /bin/rm`, `-exec \rm`, `-exec sudo rm`) is still
            // denied. What is dropped is exactly the set of NON-command positions —
            // `-name` patterns, `-path` arguments and per-match argument data —
            // where a token named `rm` is a string being searched for, never a
            // program being run.
            if c == "rm" {
                return Decision::deny("find -exec rm removes every matching file");
            }
        }

        // BG-3 (fail-open regression, fixed): CA-blastguard-016 replaced the
        // blanket "normalise every token" scan with head-token resolution only.
        // That closed the `-exec grep -l 'rm'` false positive but re-opened
        // every WRAPPER bypass the blanket scan had caught — `-exec timeout 5
        // rm -rf {} +`, `-exec xargs rm -rf {} +`, `-exec stdbuf -o0 rm …`,
        // `-exec flock /tmp/l rm …` all became ALLOW, because the resolved head
        // was `timeout` / `xargs` / … and nothing looked further.
        //
        // The structural fix: re-analyse the whole `-exec` tail through
        // `detect_bash`, exactly as the `-c` and `eval` arms already re-analyse
        // their payloads. `find` really does exec this tail, so analysing it as
        // a command line is faithful — and it inherits every present and FUTURE
        // rule (rm, git, xargs, nested shells, interpreters, wrappers) instead
        // of a hand-maintained name list that rots the moment a new wrapper
        // ships. Depth-bounded like the other re-analysis arms.
        //
        // This does NOT reopen CA-blastguard-016: `detect_bash` analyses the
        // tail from its COMMAND WORD outward, so `-exec grep -l 'rm' {} \;`
        // still resolves to `grep` and the quoted DATA token `'rm'` is never in
        // a command position.
        //
        // Fail-open condition: the exec'd program is a wrapper whose command
        // word `command_index` cannot reach — an unknown wrapper name, or a
        // known one whose positional-operand count differs from
        // `wrapper_leading_operands`. That residue is strictly smaller than
        // before this fix, and it is the same residue the top-level path has.
        if depth < MAX_SHELL_DEPTH {
            let inline = tail.join(" ");
            if !inline.trim().is_empty() {
                if let Some(deny) = acc.record(detect_bash(&inline, depth + 1)) {
                    return deny;
                }
            }
        } else {
            // Cap reached: the recursion above did NOT run, so this segment was not
            // fully analysed. Record the unfinished-analysis Ask into `acc` rather
            // than returning early, so a Deny found by a later non-recursive rule in
            // this same function still outranks it (see `VerdictAcc::record`).
            let _ = acc.record(depth_exhausted());
        }
    }
    acc.finish()
}

/// `xargs` runs a command assembled from its trailing args (after xargs's own
/// option flags), appending the piped items. Without this branch a destructive
/// payload (`find … | xargs rm -rf`, `xargs -I{} sh -c "rm -rf {}"`) fell through
/// to the catch-all Allow arm. Re-analyse the inner command through `detect_bash`
/// so it reuses the rm / shell-`-c` / find logic (and the recursion bound).
fn analyze_xargs(rest: &[&str], depth: usize) -> Decision {
    if depth >= MAX_SHELL_DEPTH {
        return depth_exhausted();
    }
    match xargs_command_start(rest) {
        Some(start) => {
            let inner = rest[start..].join(" ");
            if inner.trim().is_empty() {
                Decision::Allow
            } else {
                detect_bash(&inner, depth + 1)
            }
        }
        None => Decision::Allow,
    }
}

/// Index in `rest` of the command word xargs will execute, skipping xargs's own
/// option flags (and the separate-token value of a value-taking short flag).
/// `--` ends option parsing. Returns None when no command word follows (xargs
/// with only flags just echoes its input — nothing destructive to re-analyse).
fn xargs_command_start(rest: &[&str]) -> Option<usize> {
    // Value-taking xargs short flags whose value is the NEXT token when the flag
    // is given standalone (`-I {}`, `-n 1`, BSD `-J %`). Bundled forms (`-I{}`,
    // `-n1`, `-J%`) embed the value and consume only themselves. Covers both GNU
    // (I i n L P s d E a) and BSD/macOS (J R S) — the default xargs on this host —
    // because a MISSED value flag would misread its value as the command word and
    // leak a destructive payload (`xargs -J % rm -rf %`) past re-analysis.
    const VALUE_SHORT: &str = "IinLPsdEaJRS";
    let mut i = 0;
    while i < rest.len() {
        let t = rest[i];
        if t == "--" {
            return if i + 1 < rest.len() {
                Some(i + 1)
            } else {
                None
            };
        }
        if let Some(stripped) = t.strip_prefix("--") {
            // Long flag: value-taking ones consume the next token unless `=`-joined.
            let takes = matches!(
                stripped.split('=').next().unwrap_or(""),
                "max-args"
                    | "max-lines"
                    | "max-procs"
                    | "max-chars"
                    | "delimiter"
                    | "eof"
                    | "arg-file"
                    | "replace"
                    | "process-slot-var"
            );
            i += if takes && !t.contains('=') { 2 } else { 1 };
            continue;
        }
        if t.starts_with('-') && t.len() >= 2 {
            // Short flag (bundle). A standalone value-taking flag (`-I`, `-n`)
            // consumes the next token; a bundle that embeds the value (`-I{}`,
            // `-n1`) or a boolean bundle (`-0rt`) consumes only itself.
            let last = t.chars().last().unwrap();
            let standalone_value = t.len() == 2 && VALUE_SHORT.contains(last);
            i += if standalone_value { 2 } else { 1 };
            continue;
        }
        return Some(i); // first non-flag token = the command word
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn bash(cmd: &str) -> Decision {
        detect("Bash", Some(&json!({ "command": cmd })))
    }

    // ---- Bash: deny group ----
    #[test]
    fn denies_recursive_and_wildcard_rm() {
        assert!(bash("rm -rf dir").is_deny());
        assert!(bash("rm -fr dir").is_deny());
        assert!(bash("rm -Rf dir").is_deny());
        assert!(bash("rm -r -f dir").is_deny());
        assert!(bash("rm --recursive build").is_deny());
        assert!(bash("rm *").is_deny());
        assert!(bash("rm -f *").is_deny());
        assert!(bash("rm path/*").is_deny());
        assert!(bash("sudo rm -rf /var/data").is_deny());
    }

    // ---- Regression: CA-blastguard-004 (wrapper flag bypass) ----
    #[test]
    fn wrapper_flags_do_not_bypass_detection() {
        assert!(bash("sudo -u root rm -rf /tmp/x").is_deny());
        assert!(bash("nice -n10 rm -rf /tmp/x").is_deny());
        assert!(bash("env -i rm -rf /tmp/x").is_deny());
        assert!(bash("ionice -c3 rm -rf /tmp/x").is_deny());
    }

    #[test]
    fn denies_destructive_git() {
        assert!(bash("git clean -fdx").is_deny());
        assert!(bash("git clean -fd").is_deny());
        assert!(bash("git clean -f -d").is_deny());
        assert!(bash("git reset --hard").is_deny());
        assert!(bash("git reset --hard HEAD~3").is_deny());
        assert!(bash("git checkout -- .").is_deny());
        assert!(bash("git checkout --force").is_deny());
        assert!(bash("git checkout -f").is_deny());
    }

    #[test]
    fn ca_blastguard_07_git_global_option_prefix_still_fires_deny() {
        // CA-blastguard-07: git global options that take a SEPARATE-token value
        // (`-C DIR`, `-c k=v`, `--git-dir PATH`, …) place a non-dash value token
        // right after the option. A naive "first non-dash token" scan misread
        // that value as the subcommand, so every git deny fell through to Allow
        // (fail-open). git_subcommand_index now skips the prefix + its value.
        assert!(bash("git -C DIR reset --hard").is_deny());
        assert!(bash("git -c user.name=x clean -fd").is_deny());
        assert!(bash("git --git-dir PATH checkout --force").is_deny());
        // The stash arm derived its sub-subcommand from a fresh naive scan with
        // the same blindness — resolve it relative to the subcommand index.
        assert!(bash("git -C DIR stash clear").is_deny());
        // Non-value flags and other global-option spellings must still resolve
        // the subcommand correctly (no over- or under-broadening).
        assert!(bash("git --work-tree PATH clean -fdx").is_deny());
        assert!(bash("git --git-dir=PATH reset --hard").is_deny());
        assert!(bash("git -C DIR -c k=v reset --hard").is_deny());
        // Global-prefixed but non-destructive subcommands stay allowed.
        assert_eq!(bash("git -C DIR status"), Decision::Allow);
        assert_eq!(bash("git -C DIR stash list"), Decision::Allow);
    }

    #[test]
    fn ca_blastguard_06_git_stash_discard_is_denied() {
        // CA-blastguard-06: `git stash clear` drops every stash entry and
        // `git stash drop` drops one — both irreversibly delete stashed work,
        // the same hazard class as the already-denied working-tree-discard
        // forms. analyze_git used to fall through to Allow for `stash`.
        assert!(bash("git stash clear").is_deny());
        assert!(bash("git stash drop").is_deny());
        assert!(bash("git stash drop stash@{2}").is_deny());
        // Non-destructive stash subcommands (and the bare `git stash` push)
        // must stay allowed — the fix must not over-broaden.
        assert_eq!(bash("git stash"), Decision::Allow);
        assert_eq!(bash("git stash list"), Decision::Allow);
        assert_eq!(bash("git stash push -m wip"), Decision::Allow);
        assert_eq!(bash("git stash show"), Decision::Allow);
        assert_eq!(bash("git stash pop"), Decision::Allow);
        assert_eq!(bash("git stash apply"), Decision::Allow);
        assert_eq!(bash("git stash save wip"), Decision::Allow);
        assert_eq!(bash("git stash branch topic"), Decision::Allow);
    }

    #[test]
    fn ca_blastguard_05_git_restore_working_tree_discard_is_denied() {
        // CA-blastguard-05: `git restore` overwrites the working tree from the
        // index/source, discarding uncommitted work — same hazard as the
        // already-denied `git checkout -- .`. analyze_git used to fall through
        // to Allow for `restore`.
        assert!(bash("git restore .").is_deny());
        assert!(bash("git restore src/main.rs").is_deny());
        assert!(bash("git restore --worktree file").is_deny());
        assert!(bash("git restore --staged --worktree file").is_deny());
        // `git restore --staged <path>` only unstages; the working tree is left
        // intact, so it stays allowed.
        assert_eq!(bash("git restore --staged file"), Decision::Allow);
    }

    #[test]
    fn denies_truncate_shred_mkfs_dd_chmod_chown_find() {
        assert!(bash("truncate -s0 x").is_deny());
        assert!(bash("truncate -s 0 file").is_deny());
        assert!(bash("shred secret").is_deny());
        assert!(bash("mkfs.ext4 /dev/sdb1").is_deny());
        assert!(bash("dd of=/dev/sda").is_deny());
        assert!(bash("dd if=/dev/zero of=/dev/sda bs=1M").is_deny());
        assert!(bash("chmod -R 777 .").is_deny());
        assert!(bash("chmod --recursive 755 src").is_deny());
        assert!(bash("chown -R root .").is_deny());
        assert!(bash("find . -delete").is_deny());
        assert!(bash("find . -name '*.log' -delete").is_deny());
        assert!(bash("find . -type f -exec rm {} ;").is_deny());
        // fix-blastguard-005: -ok/-okdir are the interactive-confirmation
        // twins of -exec/-execdir (same arbitrary per-match command, just
        // behind a y/n prompt) and must be denied the same way.
        assert!(bash("find . -type f -okdir rm {} ;").is_deny());
    }

    #[test]
    fn denies_truncating_redirect_and_fork_bomb() {
        assert!(bash("echo x > existing").is_deny());
        assert!(bash("cat a > b.txt").is_deny());
        assert!(bash(":(){ :|:& };:").is_deny());
    }

    #[test]
    fn ca_blastguard_04_ampersand_gt_truncating_redirect_is_denied() {
        // CA-blastguard-04: `&>` is the combined stdout+stderr TRUNCATING
        // redirect — it overwrites the target file. The classifier used to skip
        // it as an fd-dup form, bypassing the truncating-redirect DENY.
        assert!(bash("echo x &> target").is_deny());
        assert!(bash("echo x &>target").is_deny()); // no-space form
        assert!(bash("cat a &> b.txt").is_deny());
        // The APPEND form `&>>` does NOT truncate and stays allowed.
        assert_eq!(bash("echo x &>> log.txt"), Decision::Allow);
        // fd-dup forms remain unaffected.
        assert_eq!(bash("cargo test 2>&1"), Decision::Allow);
        assert_eq!(bash("make >&2"), Decision::Allow);
    }

    #[test]
    fn ca_blastguard_08_gt_ampersand_filename_truncating_redirect_is_denied() {
        // CA-blastguard-08: `>&<filename>` is the bash MIRROR of `&>` — it
        // truncates BOTH stdout and stderr into the named file. The classifier
        // used to skip EVERY `>&` form as an fd-dup, letting `cmd >& file`
        // through (fail-OPEN). Only an ALL-DIGIT `>&<n>` is a real fd DUP.
        assert!(bash("echo x >& out.txt").is_deny());
        assert!(bash("echo x >&out.txt").is_deny()); // no-space form
        assert!(bash("cat a >& b.log").is_deny());
        // fd DUPLICATION forms (all-digit target) remain safe/allowed.
        assert_eq!(bash("make >&2"), Decision::Allow);
        assert_eq!(bash("make >&1"), Decision::Allow);
        assert_eq!(bash("cargo test 2>&1"), Decision::Allow);
        assert_eq!(bash("run 1>&2"), Decision::Allow);
        // The `&>` twin behavior is unchanged: truncating denied, append allowed.
        assert!(bash("echo x &> target").is_deny());
        assert_eq!(bash("echo x &>> log.txt"), Decision::Allow);
    }

    #[test]
    fn ca_blastguard_08_gt_ampersand_digit_prefixed_filename_is_denied() {
        // Bypass of the first fix: a `>&` target that merely STARTS with a
        // digit but contains ANY non-digit (`2x`, `2.txt`, `10.log`) is a
        // FILENAME (real bash truncates it), not an fd number. A single-byte
        // "next-is-digit" guard wrongly classified these as fd-dups and let
        // them through — the same fail-open class, narrowed. They must Deny.
        assert!(bash("echo x >&2x").is_deny());
        assert!(bash("echo x >&2.txt").is_deny());
        assert!(bash("echo x >&10.log").is_deny());
        // True fd DUPs — the ENTIRE target token is digits — stay Allowed,
        // including leading-zero runs and multi-fd/prefixed forms.
        assert_eq!(bash("echo x >&2"), Decision::Allow);
        assert_eq!(bash("echo x >&1"), Decision::Allow);
        assert_eq!(bash("echo x >&02"), Decision::Allow); // leading-zero digit run
        assert_eq!(bash("cargo test 2>&1"), Decision::Allow);
        assert_eq!(bash("run 1>&2"), Decision::Allow);
        assert_eq!(bash("foo 2>&1"), Decision::Allow);
        // fd CLOSE (`>&-`) touches no file — non-destructive, must NOT over-deny.
        assert_eq!(bash("echo x >&-"), Decision::Allow);
    }

    #[test]
    fn denies_shell_eval_bypasses() {
        // eval / exec run their inline arguments as a command.
        assert!(bash("eval \"rm -rf /\"").is_deny());
        assert!(bash("eval 'rm -rf /'").is_deny());
        assert!(bash("exec rm -rf /").is_deny());
        // <shell> -c "<payload>" re-analyses the payload.
        assert!(bash("sh -c \"rm -rf /\"").is_deny());
        assert!(bash("bash -c 'shred secret'").is_deny());
        assert!(bash("zsh -c \"rm -rf /var\"").is_deny());
        assert!(bash("bash -lc \"rm -rf /\"").is_deny());
        // find -exec/-execdir on a shell can run any destructive command.
        assert!(bash("find . -type f -exec sh -c 'rm -rf {}' ;").is_deny());
        assert!(bash("find . -execdir bash -c \"rm -rf .\" ;").is_deny());
        // CA-blastguard-001: a NON-shell interpreter with an inline-eval flag
        // (python -c, perl -e, node -e, …) is equally destructive per match and
        // used to fall through to Allow (only sh-family shells were caught).
        assert!(
            bash("find . -type f -exec python3 -c \"import os; os.system('rm -rf /')\" ;")
                .is_deny()
        );
        assert!(bash("find . -exec perl -e 'unlink glob \"*\"' ;").is_deny());
        assert!(
            bash("find . -execdir node -e \"require('fs').rmSync('/',{recursive:true})\" ;")
                .is_deny()
        );
        // …but a plain script-file exec (no inline-eval flag) is NOT over-blocked.
        assert_eq!(bash("find . -exec python3 script.py ;"), Decision::Allow);
        // A benign wrapper in front still resolves to the eval builtin.
        assert!(bash("sudo eval \"rm -rf /\"").is_deny());
        // Nested wrapping is caught up to the recursion bound.
        assert!(bash("sh -c \"sh -c 'rm -rf /'\"").is_deny());
    }

    #[test]
    fn absolute_path_and_leading_whitespace_rm_still_denied() {
        // Regression: basename() already normalises an absolute path, and
        // split_whitespace() drops leading blanks — both stay denied.
        assert!(bash("/bin/rm -rf /data").is_deny());
        assert!(bash("  rm -rf dir").is_deny());
        assert!(bash("\trm -rf dir").is_deny());
    }

    // ---- Bash: allow group ----
    #[test]
    fn allows_benign_commands() {
        assert_eq!(bash("ls"), Decision::Allow);
        assert_eq!(bash("ls -la"), Decision::Allow);
        assert_eq!(bash("cat f"), Decision::Allow);
        assert_eq!(bash("git status"), Decision::Allow);
        assert_eq!(bash("git diff"), Decision::Allow);
        assert_eq!(bash("cargo test"), Decision::Allow);
        assert_eq!(bash("cargo build -p blastguard"), Decision::Allow);
        assert_eq!(bash("mkdir -p a/b"), Decision::Allow);
        assert_eq!(bash("rm notes.txt"), Decision::Allow);
        assert_eq!(bash("rm a.txt b.txt"), Decision::Allow);
        assert_eq!(bash("git checkout main"), Decision::Allow);
        assert_eq!(bash("git checkout -b feature"), Decision::Allow);
        assert_eq!(bash("git clean -n"), Decision::Allow);
        assert_eq!(bash("chmod 644 file"), Decision::Allow);
        assert_eq!(bash("find . -name '*.rs'"), Decision::Allow);
        assert_eq!(bash("rm --help"), Decision::Allow);
    }

    // ---- Regression: CA-blastguard-011 (help-flag bypass) ----
    #[test]
    fn ca_blastguard_011_help_flag_does_not_disable_detection() {
        // On macOS BSD `rm` has no `--help`, so the flag is just another
        // filename operand: `rm -rf <dir> --help` exits 0 and DELETES <dir>.
        // The old `rest.iter().any(is_help_flag)` short-circuit therefore
        // turned `--help` into a universal bypass for every Bash rule.
        assert!(bash("rm -rf /some/path --help").is_deny());
        assert!(bash("rm -rf /some/path -h").is_deny());
        // Help flag BEFORE the destructive operands is equally unsafe (GNU
        // getopt permutes, so `-rf /path` is still parsed and executed).
        assert!(bash("rm --help -rf /some/path").is_deny());
        assert!(bash("rm -h -rf /some/path").is_deny());
        // Interleaved / trailing orderings.
        assert!(bash("rm -rf --help /some/path").is_deny());
        assert!(bash("rm --help *").is_deny());
        // Wrapper-prefixed form (help flag must not re-open the sudo path).
        assert!(bash("sudo rm -rf /var/data --help").is_deny());

        // Other commands in the same rule set shared the identical bypass.
        assert!(bash("git reset --hard --help").is_deny());
        assert!(bash("git clean -fdx --help").is_deny());
        assert!(bash("chmod -R 777 . --help").is_deny());
        assert!(bash("chown -R me:me / --help").is_deny());
        assert!(bash("shred secret --help").is_deny());
        assert!(bash("truncate -s0 x --help").is_deny());
        assert!(bash("dd if=/dev/zero of=/dev/sda --help").is_deny());
        assert!(bash("tee /etc/hosts --help").is_deny());
        assert!(bash("mkfs.ext4 /dev/sdb1 --help").is_deny());
        // Shell-eval smuggling: the payload is re-analysed, and the outer
        // help flag must not suppress that either.
        assert!(bash("eval rm -rf /some/path --help").is_deny());
        assert!(bash("sh -c \"rm -rf /some/path --help\"").is_deny());

        // …while a genuine help invocation (flag is the ONLY argument) stays
        // allowed — the original no-false-positive intent is preserved.
        assert_eq!(bash("rm --help"), Decision::Allow);
        assert_eq!(bash("rm -h"), Decision::Allow);
        assert_eq!(bash("git --help"), Decision::Allow);
        assert_eq!(bash("shred --help"), Decision::Allow);
        assert_eq!(bash("truncate --help"), Decision::Allow);
        assert_eq!(bash("tee --help"), Decision::Allow);
        assert_eq!(bash("chmod --help"), Decision::Allow);
        assert_eq!(bash("sh --help"), Decision::Allow);
    }

    #[test]
    fn allows_benign_shell_wrappers() {
        // Quoted text that merely mentions a destructive command is not run.
        assert_eq!(bash("echo 'eval rm -rf /'"), Decision::Allow);
        // Help and benign payloads stay allowed.
        assert_eq!(bash("sh --help"), Decision::Allow);
        assert_eq!(bash("bash -c \"cargo test\""), Decision::Allow);
        assert_eq!(bash("sh -c 'ls -la'"), Decision::Allow);
        // source/. of an opaque file path cannot be inspected — allowed
        // (re-analyse-inline bias: a bare path is not a destructive command).
        assert_eq!(bash("source .venv/bin/activate"), Decision::Allow);
        assert_eq!(bash(". ~/.bashrc"), Decision::Allow);
        // A shell running a script file (no -c) — file unknowable, allowed.
        assert_eq!(bash("bash build.sh"), Decision::Allow);
        // find -exec with a non-shell, non-rm command.
        assert_eq!(
            bash("find . -name '*.rs' -exec grep todo {} ;"),
            Decision::Allow
        );
    }

    #[test]
    fn allows_rm_of_config_files_even_when_destructive() {
        // Single config file, non-recursive — allowed anyway.
        assert_eq!(bash("rm package.json"), Decision::Allow);
        // Recursive rm whose only target is a config tree.
        assert_eq!(bash("rm -rf .claude"), Decision::Allow);
        assert_eq!(bash("rm -f Cargo.lock"), Decision::Allow);
    }

    // ---- Bash: boundary cases (no false positives) ----
    #[test]
    fn append_and_fd_redirects_are_not_truncation() {
        assert_eq!(bash("echo x >> log.txt"), Decision::Allow);
        assert_eq!(bash("cargo test 2>&1"), Decision::Allow);
        assert_eq!(bash("make >&2"), Decision::Allow);
        // BG-2: `2> err.log` USED to be asserted Allow here. It is not an fd
        // dup — it TRUNCATES `err.log` — so it now denies. What stays allowed
        // is decided by what the redirect does to the target, not by the fd
        // number: dup (`2>&1`), append (`>>`), /dev/null and config files.
        assert!(bash("cargo build 2> err.log").is_deny());
        assert_eq!(bash("cargo build 2> /dev/null"), Decision::Allow);
        // D1 (DELIBERATELY FLIPPED from Allow): the `is_temp_scratch` carve-out
        // that allowed this was a universal bypass (`/tmp/../etc/hosts`) and has
        // been deleted, not patched. A denied temp-log redirect is a false
        // positive; that is the acceptable side of the trade.
        assert!(bash("cargo build 2> /tmp/err.log").is_deny());
    }

    #[test]
    fn redirect_to_devnull_and_config_is_allowed() {
        assert_eq!(bash("echo hi > /dev/null"), Decision::Allow);
        assert_eq!(bash("generate > config.toml"), Decision::Allow);
    }

    #[test]
    fn quoted_destructive_text_is_not_executed() {
        // The dangerous text lives inside an echo string, not as a command.
        assert_eq!(bash("echo 'rm -rf /'"), Decision::Allow);
        assert_eq!(bash("echo \"a > b\""), Decision::Allow);
    }

    // ---- Redirect false-positives: Rust arrows & angle-bracket placeholders ----
    // Regression: descriptive prose containing a Rust return-type arrow (`->`)
    // or an angle-bracket identifier placeholder (`<value>`) must not be read as
    // a truncating `>` redirect (which would force-gate benign condukt tasks).
    #[test]
    fn benign_rust_arrow_is_not_a_redirect() {
        assert_eq!(single_redirect_target("fn foo() -> Bar"), None);
        assert_eq!(bash("implement fn parse() -> Result"), Decision::Allow);
        assert_eq!(bash("map the input -> output pipeline"), Decision::Allow);
    }

    #[test]
    fn benign_angle_bracket_placeholder_is_not_a_redirect() {
        assert_eq!(single_redirect_target("pass --flag <value>"), None);
        assert_eq!(bash("run with --flag <value>"), Decision::Allow);
        assert_eq!(bash("tdd red --task <id> --cmd foo"), Decision::Allow);
        assert_eq!(bash("emit {risk} for <RID>"), Decision::Allow);
    }

    #[test]
    fn non_ascii_angle_bracket_placeholder_is_not_a_redirect() {
        // Regression for the 2026-07-17 false-gate incident: a condukt
        // done_criteria string with a Japanese placeholder (`<同一key>`)
        // followed by prose (`...`) was misread as a truncating redirect
        // whose target was the ellipsis, force-gating an ordinary task as
        // High/irreversible.
        assert_eq!(
            single_redirect_target("2セッションが<同一key>をclaimする"),
            None
        );
        assert_eq!(
            bash("2セッションが<同一key>をclaimする結合テスト"),
            Decision::Allow
        );
        assert_eq!(bash("emit {risk} for <セッションID>"), Decision::Allow);
        let a = crate::classify::classify(
            "2セッション(プロセス)がほぼ同時に同一keyをbeginする結合テストが追加され、\
             片方が必ずskip(exit 1、または明示的な待機後の順序どおりの成功)になり\
             両方が成功することは無いと固定される。<同一key>...を検証する",
        );
        assert_eq!(a.risk, crate::classify::Risk::Low);
        assert!(a.reversible);
        assert!(!a.requires_gate());
    }

    #[test]
    fn benign_lookalike_task_prose_is_low_and_reversible() {
        // The end-to-end path the bug broke: classify() over a task's prose.
        let a = crate::classify::classify("implement fn parse() -> Result and pass --flag <value>");
        assert_eq!(a.risk, crate::classify::Risk::Low);
        assert!(a.reversible);
        assert!(!a.requires_gate());
    }

    #[test]
    fn benign_fix_keeps_real_truncating_redirect_denied() {
        // Guard the other direction: a genuine `> path` redirect stays denied.
        assert!(bash("cat x > /etc/passwd").is_deny());
        assert!(bash("echo x > existing").is_deny());
        // Arrow/placeholder skip must not swallow a real redirect on the line.
        assert!(bash("echo done -> nope; cat x > realfile").is_deny());
        // A non-ASCII redirect TARGET (no preceding `<`) must still be caught —
        // the non-ASCII-byte allowance only extends the backward scan for an
        // already-open `<...>` placeholder, it doesn't exempt real targets.
        assert!(bash("cat x > 日本語.txt").is_deny());
    }

    // ---- Regression: fail-open defects (CA-blastguard-01 / -02) ----
    #[test]
    fn later_segment_truncating_redirect_is_denied() {
        // CA-blastguard-01: a safe early redirect must not blind the gate to a
        // later truncating redirect in a subsequent segment.
        assert!(bash("cat notes.txt > important.txt").is_deny()); // control
        assert!(bash("echo hi > /dev/null; cat notes.txt > important.txt").is_deny());
    }

    #[test]
    fn wildcard_rm_is_not_config_exempt() {
        // CA-blastguard-02: a wildcard operand must not self-match a config glob
        // (`*.toml` matching the `*.toml` allow-glob) and slip past as exempt.
        assert!(bash("rm -rf *.toml").is_deny());
        assert!(bash("rm *.lock").is_deny());
        // Control: a single literal config file stays exempt.
        assert_eq!(bash("rm Cargo.toml"), Decision::Allow);
    }

    // ---- Regression: round-2026W28 CONFIRMED fail-opens (CA-blastguard-01/02/03) ----
    #[test]
    fn explicit_stdout_fd_truncating_redirect_is_denied() {
        // CA-blastguard-03: `1>file` is an explicit stdout truncating redirect,
        // semantically identical to bare `>file` (denied) — must also be denied.
        assert!(bash("echo x 1> existing").is_deny());
        assert!(bash("cat a 1>b.txt").is_deny());
        // BG-2: this test used to assert that only fd 1 truncates and that
        // `2>`/`11>`/`21>`/`10>` stay Allow. That was the fail-open: `N>file`
        // truncates `file` for EVERY fd N. All explicit fds now deny.
        assert!(bash("cargo build 2> err.log").is_deny());
        assert!(bash("echo hi 11> file.txt").is_deny());
        assert!(bash("echo hi 21> out.txt").is_deny());
        assert!(bash("echo hi 10> out.txt").is_deny());
        // No regression: fd-dup forms touch no file and stay allowed.
        assert_eq!(bash("cargo test 2>&1"), Decision::Allow);
        assert_eq!(bash("run 1>&2"), Decision::Allow);
    }

    #[test]
    fn rm_question_and_bracket_globs_are_denied() {
        // CA-blastguard-02b: `?` and `[` globs expand to many files like `*` does,
        // but the main wildcard check only looked for `*` (has_glob_meta existed,
        // unused). They must be denied.
        assert!(bash("rm ?.txt").is_deny());
        assert!(bash("rm [0-9].txt").is_deny());
        assert!(bash("rm file?").is_deny());
        // Control: a literal filename (no glob metachar) stays allowed.
        assert_eq!(bash("rm notes.txt"), Decision::Allow);
    }

    // ---- Regression: round-20260714b CONFIRMED findings (CA-blastguard-006..009) ----

    #[test]
    fn ca_blastguard_006_bare_top_level_interpreter_inline_eval_is_denied() {
        // CA-blastguard-006: a bare top-level code-interpreter invocation with
        // an inline-eval flag (no `find` wrapper) must be denied — the check
        // used to exist only inside analyze_find's -exec/-ok handling.
        assert!(bash("python3 -c \"import os; os.system('rm -rf /')\"").is_deny());
        // Control: a plain script-file invocation (no inline-eval flag) stays
        // allowed — no over-blocking.
        assert_eq!(bash("python3 script.py"), Decision::Allow);
    }

    #[test]
    fn ca_blastguard_007_stacked_short_flag_inline_eval_is_denied() {
        // CA-blastguard-007: is_inline_eval_flag only matched exact tokens
        // (-c, -e, …); a combined short flag like `-ic` (python's -i stacked
        // with -c) must also be recognized as carrying an inline-eval flag.
        assert!(
            bash("find . -exec python3 -ic \"import os; os.system('rm -rf /')\" \\;").is_deny()
        );
        // Control: an unrelated long flag containing 'c'/'e'/'r'/'p' letters
        // (e.g. `--color`) must NOT be mistaken for a stacked eval flag.
        assert_eq!(
            bash("find . -exec grep --color todo {} \\;"),
            Decision::Allow
        );
    }

    #[test]
    fn ca_blastguard_008_versioned_interpreter_basename_is_denied() {
        // CA-blastguard-008: is_code_interpreter only matched exact unversioned
        // basenames; a versioned binary like `python3.12` must also be
        // recognized as a code interpreter.
        assert!(
            bash("find . -exec python3.12 -c \"import os; os.system('rm -rf /')\" \\;").is_deny()
        );
        // Control: a plain script-file invocation with a versioned
        // interpreter (no inline-eval flag) stays allowed.
        assert_eq!(
            bash("find . -exec python3.12 script.py \\;"),
            Decision::Allow
        );
    }

    #[test]
    fn ca_blastguard_009_tee_without_append_truncates_is_denied() {
        // CA-blastguard-009: `tee FILE` (no -a) truncates/overwrites FILE
        // identically to a `>` redirect — must be denied. `tee -a FILE`
        // (append mode) is safe and stays allowed.
        assert!(bash("echo x | tee important.txt").is_deny());
        assert_eq!(bash("echo x | tee -a important.txt"), Decision::Allow);
        assert_eq!(bash("echo x | tee --append important.txt"), Decision::Allow);
    }

    #[test]
    fn xargs_destructive_payload_is_denied() {
        // CA-blastguard-01: xargs runs a command assembled from its trailing args;
        // a destructive payload was passed straight through (no xargs branch).
        assert!(bash("find . | xargs rm -rf").is_deny());
        assert!(bash("find . -type f | xargs -I{} sh -c \"rm -rf {}\"").is_deny());
        assert!(bash("ls | xargs -n1 rm -rf").is_deny());
        // BSD/macOS xargs `-J <replstr>` is value-taking too; its value token must
        // not be mistaken for the command word (would leak the payload past re-analysis).
        assert!(bash("find . | xargs -J % rm -rf %").is_deny());
        assert!(bash("find . -type f | xargs -J % sh -c \"rm -rf %\"").is_deny());
        // Controls: benign xargs payloads stay allowed (no false positive).
        assert_eq!(
            bash("find . -name '*.rs' | xargs grep todo"),
            Decision::Allow
        );
        assert_eq!(bash("ls | xargs -n1 echo"), Decision::Allow);
    }

    // ---- File operations ----
    #[test]
    fn edit_is_always_allowed() {
        assert_eq!(
            detect("Edit", Some(&json!({ "file_path": "src/main.rs" }))),
            Decision::Allow
        );
        assert_eq!(
            detect("MultiEdit", Some(&json!({ "file_path": "src/main.rs" }))),
            Decision::Allow
        );
    }

    #[test]
    fn write_empty_content_to_source_is_denied() {
        assert!(detect(
            "Write",
            Some(&json!({ "file_path": "src/main.rs", "content": "" }))
        )
        .is_deny());
        assert!(detect(
            "Write",
            Some(&json!({ "file_path": "src/main.rs", "content": "   \n" }))
        )
        .is_deny());
    }

    #[test]
    fn write_with_content_or_to_config_is_allowed() {
        assert_eq!(
            detect(
                "Write",
                Some(&json!({ "file_path": "src/main.rs", "content": "fn main() {}" }))
            ),
            Decision::Allow
        );
        // Config files are exempt even when emptied.
        assert_eq!(
            detect(
                "Write",
                Some(&json!({ "file_path": "Cargo.toml", "content": "" }))
            ),
            Decision::Allow
        );
        assert_eq!(
            detect(
                "Write",
                Some(&json!({ "file_path": ".claude/settings.json", "content": "" }))
            ),
            Decision::Allow
        );
    }

    #[test]
    fn write_to_git_internals_is_denied() {
        assert!(detect(
            "Write",
            Some(&json!({ "file_path": ".git/config", "content": "x" }))
        )
        .is_deny());
    }

    #[test]
    fn missing_or_unknown_input_is_allowed() {
        assert_eq!(detect("Bash", None), Decision::Allow);
        assert_eq!(
            detect("Read", Some(&json!({ "file_path": "x" }))),
            Decision::Allow
        );
        assert_eq!(detect("Write", Some(&json!({}))), Decision::Allow);
    }

    // ---- UTF-8 boundary safety (regression: multi-byte text must not panic) ----

    #[test]
    fn multibyte_after_redirect_marker_does_not_panic() {
        // '頻' = U+983B = E9 A0 BB. Byte 0xA0, cast `as char`, is U+00A0
        // (no-break space / whitespace) — the old byte-scan broke mid-char here
        // and `seg[start..j]` panicked ("not a char boundary"). A `>` adjacent
        // to Japanese prose (common in this repo's task text) must classify
        // without crashing.
        let _ = redirect_targets("band>=1・低頻度で再注入");
        let _ = redirect_targets("値を >低頻度 に絞る");
        let _ = bash("echo band>=1・低頻度で再注入");
    }

    #[test]
    fn split_segments_preserves_multibyte_text() {
        // Byte-as-char split used to mangle non-ASCII into Latin-1 mojibake.
        let segs = split_segments("echo 低頻度 && ls 監査");
        assert_eq!(segs.len(), 2);
        assert!(segs[0].contains("低頻度"));
        assert!(segs[1].contains("監査"));
    }

    #[test]
    fn redirect_target_after_multibyte_is_extracted_intact() {
        // A genuine truncating redirect whose target follows multi-byte text is
        // still read, and the extracted token is valid UTF-8 (no mid-char cut).
        assert_eq!(
            single_redirect_target("低頻度 > out.txt"),
            Some("out.txt".to_string())
        );
    }

    #[test]
    fn kebab_case_angle_placeholder_is_not_a_redirect() {
        // `<run-id>` / `<pdo-unit-id>` are placeholder prose, not redirects — the
        // hyphen must not break placeholder recognition (regression: these were
        // force-denied as `>` truncating redirects).
        assert!(single_redirect_target("overwatch begin --key <pdo-unit-id> --title x").is_none());
        assert!(single_redirect_target("condukt heartbeat --run <run-id>").is_none());
        assert!(!bash("echo state --run <session-id> done").is_deny());
    }

    #[test]
    fn hyphenated_real_redirect_still_detected() {
        // A real truncating redirect with a hyphenated target (no preceding `<`)
        // is still caught — allowing hyphens only matches true `<...>` pairs.
        assert_eq!(
            single_redirect_target("foo-bar > out-file.txt"),
            Some("out-file.txt".to_string())
        );
        assert!(bash("cat x > important-data.txt").is_deny());
    }

    // ---- Regression: CA-blastguard-012 (trailing token defeats `-c` payload) ----
    #[test]
    fn trailing_token_after_dash_c_payload_does_not_bypass() {
        // Verified bypass: the payload was join()ed with everything after it, so
        // ANY trailing token left the joined string un-peelable and the payload
        // command word parsed as the literal `"rm` -> Allow.
        assert!(bash(r#"sh -c "rm -rf /some/path""#).is_deny());
        assert!(bash(r#"sh -c "rm -rf /some/path" --help"#).is_deny());
        assert!(bash(r#"sh -c "rm -rf /some/path" arg0"#).is_deny());
        assert!(bash(r#"sh -c "rm -rf /some/path" -h"#).is_deny());
        assert!(bash(r#"bash -c "rm -rf /some/path" x y z"#).is_deny());
        assert!(bash(r#"zsh -c 'rm -rf /some/path' scriptname"#).is_deny());
        // `; true` inside the trailing tokens (segment split happens on the
        // OUTER line, so the trailing `true` segment is separately benign).
        assert!(bash(r#"sh -c "rm -rf /some/path" ; true"#).is_deny());
        // Unquoted multi-word payload keeps its historical denial.
        assert!(bash("sh -c rm -rf /some/path").is_deny());
    }

    #[test]
    fn benign_dash_c_payloads_with_trailing_tokens_still_allowed() {
        // The widened `-c` parsing must not start denying ordinary shell usage
        // where the trailing tokens are just $0/$1/… positional parameters.
        assert!(!bash(r#"sh -c "ls""#).is_deny());
        assert!(!bash(r#"sh -c "ls" myscript"#).is_deny());
        assert!(!bash(r#"bash -c "cargo test -p blastguard" runner"#).is_deny());
        assert!(!bash(r#"sh -c 'echo hello' arg0 arg1"#).is_deny());
    }

    // ---- Regression: CA-blastguard-013 (quoted / escaped command word) ----
    #[test]
    fn quoted_or_escaped_command_word_does_not_bypass() {
        // `\rm` is the standard alias-bypass idiom, so this is reachable in
        // ordinary use — not an exotic construction.
        assert!(bash(r"\rm -rf /some/path").is_deny());
        assert!(bash(r#""rm" -rf /some/path"#).is_deny());
        assert!(bash("'rm' -rf /some/path").is_deny());
        assert!(bash(r#"r""m -rf /some/path"#).is_deny());
        // Same normalisation on the wrapper-skipping path and on nested shells.
        assert!(bash(r"sudo \rm -rf /some/path").is_deny());
        assert!(bash(r"command \rm -rf /some/path").is_deny());
        assert!(bash(r#"sh -c "\rm -rf /some/path" --help"#).is_deny());
        // Other rule arms reached through the same normalisation.
        assert!(bash(r"\shred /some/file").is_deny());
        assert!(bash(r#""git" reset --hard"#).is_deny());
        assert!(bash(r"find . -name '*.log' -exec \rm {} \;").is_deny());
        // A quoted absolute path still normalises to the bare program name.
        assert!(bash(r#""/bin/rm" -rf /some/path"#).is_deny());
    }

    #[test]
    fn command_word_normalisation_does_not_deny_benign_commands() {
        // The single-argument help forms this gate deliberately allows.
        assert!(!bash("rm --help").is_deny());
        assert!(!bash("sh --help").is_deny());
        assert!(!bash(r"\rm --help").is_deny());
        // Ordinary commands whose words contain quotes/escapes or path parts.
        assert!(!bash("echo 'rm -rf /some/path'").is_deny());
        assert!(!bash(r#"grep -n "rm" src/detect.rs"#).is_deny());
        assert!(!bash("./scripts/build.sh").is_deny());
        assert!(!bash(r"printf '%s\n' done").is_deny());
        assert!(!bash("ls -la").is_deny());
        assert!(!bash("cargo test -p blastguard").is_deny());
    }

    #[test]
    fn first_shell_word_unquotes_one_level() {
        assert_eq!(first_shell_word(r"\rm").as_deref(), Some("rm"));
        assert_eq!(first_shell_word(r#""rm""#).as_deref(), Some("rm"));
        assert_eq!(first_shell_word("'rm'").as_deref(), Some("rm"));
        assert_eq!(first_shell_word(r#"r""m"#).as_deref(), Some("rm"));
        assert_eq!(
            first_shell_word(r#""rm -rf /some/path" --help"#).as_deref(),
            Some("rm -rf /some/path")
        );
        assert_eq!(
            first_shell_word("'rm -rf /some/path' arg0").as_deref(),
            Some("rm -rf /some/path")
        );
        assert_eq!(first_shell_word("   ").as_deref(), None);
        assert_eq!(first_shell_word("").as_deref(), None);
        // Only ONE level is removed and no expansion happens.
        assert_eq!(first_shell_word("$(which rm)").as_deref(), Some("$(which"));
    }

    /// The double-quote escape table: inside `"…"` only `\"`, `\\`, `\$` and
    /// '\`' are escapes; every other backslash pair is kept LITERALLY (POSIX),
    /// so `\n` must survive as two characters and not become a newline.
    #[test]
    fn first_shell_word_double_quote_escape_table() {
        assert_eq!(first_shell_word(r#""a\"b""#).as_deref(), Some("a\"b"));
        assert_eq!(first_shell_word(r#""a\\b""#).as_deref(), Some(r"a\b"));
        assert_eq!(first_shell_word(r#""a\$b""#).as_deref(), Some("a$b"));
        assert_eq!(first_shell_word("\"a\\`b\"").as_deref(), Some("a`b"));
        // NOT an escape: backslash and `n` both stay.
        assert_eq!(first_shell_word(r#""a\nb""#).as_deref(), Some(r"a\nb"));
        // A backslash at the very end of a double-quoted run has nothing to
        // escape and is kept.
        assert_eq!(first_shell_word("\"ab\\").as_deref(), Some(r"ab\"));
    }

    /// An UNTERMINATED quote is reachable in practice, not a theoretical case:
    /// tokenisation splits on whitespace and can cut a quoted string in half,
    /// so `first_shell_word` is routinely handed a fragment with no closing
    /// quote. It must consume to end-of-input and still return the word rather
    /// than losing it.
    #[test]
    fn first_shell_word_handles_unterminated_quotes() {
        assert_eq!(
            first_shell_word(r#""rm -rf /x"#).as_deref(),
            Some("rm -rf /x")
        );
        assert_eq!(first_shell_word("'rm -rf /x").as_deref(), Some("rm -rf /x"));
    }

    /// A trailing lone backslash has no following character to escape; it is
    /// kept literal instead of silently dropping the word.
    #[test]
    fn first_shell_word_trailing_lone_backslash() {
        assert_eq!(first_shell_word(r"\").as_deref(), Some(r"\"));
        assert_eq!(first_shell_word(r"rm\").as_deref(), Some(r"rm\"));
    }

    /// The ONLY reason `dash_c_payloads` carries a `!word.is_empty()` guard:
    /// an empty quoted word is `Some("")`, not `None` (a word WAS seen), and
    /// re-analysing an empty command must not happen.
    #[test]
    fn first_shell_word_empty_quoted_word_is_some_empty() {
        assert_eq!(first_shell_word("''").as_deref(), Some(""));
        assert_eq!(first_shell_word(r#""""#).as_deref(), Some(""));
        assert!(dash_c_payloads(&["-c", "''"]).iter().all(|p| !p.is_empty()));
    }

    /// CA-blastguard-014 (fail-open): a shell parses its own OPTIONS before the
    /// command string, so an option token between `-c` and the payload used to
    /// hide the payload completely. All three forms really execute.
    #[test]
    fn ca_blastguard_014_options_between_dash_c_and_payload_are_denied() {
        assert!(bash("bash -c -- 'rm -rf /some/path'").is_deny());
        assert!(bash("bash -c -x 'rm -rf /some/path'").is_deny());
        assert!(bash(r#"sh -c -- "rm -rf /some/path""#).is_deny());
        assert!(bash("sh -c -x -- 'rm -rf /some/path'").is_deny());
        assert!(bash("zsh -c -- 'rm -rf /some/path'").is_deny());
        // Control: the bundled form was already denied and must stay denied.
        assert!(bash("bash -cx 'rm -rf /some/path'").is_deny());
        // No regression on the plain form, or on benign payloads behind the
        // same option shapes.
        assert!(bash("sh -c 'rm -rf /some/path'").is_deny());
        assert_eq!(bash("sh -c -- 'ls'"), Decision::Allow);
        assert_eq!(bash("bash -c -x 'ls -la'"), Decision::Allow);
    }

    /// CA-blastguard-015 (false positive): a help flag plus a pure REDIRECT
    /// carries no operand, so there is nothing to destroy. These are ordinary
    /// usage and blastguard is a hard block.
    #[test]
    fn ca_blastguard_015_help_with_redirect_only_is_allowed() {
        assert_eq!(bash("shred --help 2>&1"), Decision::Allow);
        assert_eq!(bash("shred --help 2>&1 | head"), Decision::Allow);
        assert_eq!(bash("truncate --help 2>/dev/null"), Decision::Allow);
        assert_eq!(bash("tee --help 2>&1"), Decision::Allow);
        assert_eq!(bash("shred --help > /dev/null"), Decision::Allow);
        assert_eq!(bash("shred -h 2>&1"), Decision::Allow);
        // Help flag among other FLAGS only — still no operand.
        assert_eq!(bash("shred -u --help"), Decision::Allow);
        // And the CA-blastguard-011 denials must survive untouched: an OPERAND
        // is present, so the help flag proves nothing.
        assert!(bash("rm -rf /some/path --help").is_deny());
        assert!(bash("rm --help *").is_deny());
        assert!(bash("rm -rf /some/path -h").is_deny());
        assert!(bash("rm --help -rf /some/path").is_deny());
        assert!(bash(r#"sh -c "rm -rf /some/path" --help"#).is_deny());
        assert!(bash("shred --help /some/path").is_deny());
        assert!(bash("truncate --help -s 0 /some/path").is_deny());
        // A truncating redirect is still denied earlier by detect_bash, so the
        // help rule cannot launder one.
        assert!(bash("shred --help > /some/path").is_deny());
        // Bare `rm --help` / `sh --help` keep working.
        assert_eq!(bash("rm --help"), Decision::Allow);
        assert_eq!(bash("sh --help"), Decision::Allow);
    }

    /// CA-blastguard-016 (false positive): searching for the literal string
    /// `rm` is normal work; only a plausible COMMAND position may be
    /// normalised.
    #[test]
    fn ca_blastguard_016_find_rm_scan_only_at_command_positions() {
        assert_eq!(
            bash(r"find . -name '*.md' -exec grep -l 'rm' {} \;"),
            Decision::Allow
        );
        assert_eq!(bash(r#"find . -name "rm" -print"#), Decision::Allow);
        assert_eq!(
            bash(r"find . -path './rm/*' -exec cat {} \;"),
            Decision::Allow
        );
        assert_eq!(
            bash(r"find . -name '*.rs' -exec grep -n 'rm -rf' {} \;"),
            Decision::Allow
        );
        // Real command positions stay denied.
        assert!(bash(r"find . -type f -exec rm {} \;").is_deny());
        assert!(bash(r"find . -name '*.log' -exec \rm {} \;").is_deny());
        assert!(bash(r"find . -exec /bin/rm -rf {} \;").is_deny());
        assert!(bash(r"find . -okdir rm {} \;").is_deny());
        assert!(bash(r"find . -exec sh -c 'rm -rf {}' \;").is_deny());
        // A benign FIRST -exec must not hide a destructive later one — in both
        // terminator forms. `+` keeps both clauses in ONE segment; `\;` is a
        // `;` to `split_segments`, so the second clause arrives as its own
        // segment whose command word is `-exec` (see the BG-3 arm in
        // `analyze_segment`).
        assert!(bash("find . -exec grep -l 'rm' {} + -exec rm {} +").is_deny());
        assert!(bash(r"find . -exec grep -l 'rm' {} \; -exec rm {} \;").is_deny());
        // Wrapper-resolved command position (widening: `sudo rm` is
        // unambiguously the program `rm`, exactly as elsewhere in this file).
        assert!(bash(r"find . -exec sudo rm -rf {} \;").is_deny());
    }

    // ---- Regression: BG-1 / BG-2 / BG-3 ----
    //
    // BG-2: every `N>` is a truncating redirect, whatever the fd number and
    // whether the target is attached or space-separated. The previous round
    // skipped all non-stdout fds, so `shred --help 2> /some/path` (which
    // truncates `/some/path`) was ALLOW while the `>` and `1>` twins denied.
    #[test]
    fn bg2_every_explicit_fd_redirect_truncates() {
        // Space-separated, any fd — all truncate the target.
        assert!(bash("shred --help 2> /some/path").is_deny());
        assert!(bash("shred --help 0> /some/path").is_deny());
        assert!(bash("shred --help 3> /some/path").is_deny());
        assert!(bash("shred --help 9> /some/path").is_deny());
        // Controls that already denied before the fix.
        assert!(bash("shred --help >  /some/path").is_deny());
        assert!(bash("shred --help 1> /some/path").is_deny());
        // The plain-command proof that rule 2 (not the help rule) is what was
        // missing: `echo` has no rule of its own, so only rule 2 can catch it.
        assert!(bash("echo x 2> /some/path").is_deny());
        assert!(bash("echo x 3>/some/path").is_deny());
    }

    #[test]
    fn bg2_fd_dup_and_safe_targets_stay_allowed() {
        // fd DUP touches no file.
        assert_eq!(bash("shred --help 2>&1"), Decision::Allow);
        assert_eq!(bash("tee --help 2>&1"), Decision::Allow);
        assert_eq!(bash("cmd 3>&2"), Decision::Allow);
        // /dev/null and config files are safe targets at every fd.
        assert_eq!(bash("truncate --help 2>/dev/null"), Decision::Allow);
        assert_eq!(bash("gen 2> config.toml"), Decision::Allow);
        // D1 (DELIBERATELY FLIPPED from Allow): temp-directory targets are no
        // longer a safe class. `is_temp_scratch` was a raw prefix test that
        // `/tmp/../<anything>` walked straight out of, disabling the whole
        // truncating-redirect rule; it is deleted rather than normalised.
        assert!(bash("cargo test 2> /tmp/log").is_deny());
        assert!(bash("cargo test > /tmp/log").is_deny());
        assert!(bash("cargo test 2> /var/tmp/log").is_deny());
        assert!(bash("cargo test 2> /tmpfile").is_deny());
        // Append is not truncation at any fd.
        assert_eq!(bash("cmd 2>> err.log"), Decision::Allow);
    }

    #[test]
    fn bg2_help_rule_operand_skip_stays_sound() {
        // With rule 2 corrected, `has_operand`'s redirect skip only ever hides
        // redirects rule 2 already vetted. Genuine operands still deny.
        assert!(bash("shred --help /some/path").is_deny());
        assert!(bash("rm -rf /some/path --help").is_deny());
        assert!(bash("rm --help *").is_deny());
        assert!(bash("rm --help -rf /some/path").is_deny());
        // No operand at all → genuinely harmless help invocations.
        assert_eq!(bash("rm --help"), Decision::Allow);
        assert_eq!(bash("sh --help"), Decision::Allow);
    }

    // BG-3: `find -exec` must reach the command word BEHIND an exec-wrapper,
    // and the `\;` terminator must not defeat the multi-exec scan.
    #[test]
    fn bg3_find_exec_wrappers_are_denied() {
        assert!(bash("find . -exec timeout 5 rm -rf {} +").is_deny());
        assert!(bash("find . -exec xargs rm -rf {} +").is_deny());
        assert!(bash("find . -exec stdbuf -o0 rm -rf {} +").is_deny());
        assert!(bash("find . -exec flock /tmp/l rm -rf {} +").is_deny());
        assert!(bash("find . -exec setsid rm -rf {} +").is_deny());
        assert!(bash("find . -exec chroot /newroot rm -rf {} +").is_deny());
        // Control: the wrapper that already worked.
        assert!(bash("find . -exec sudo rm -rf {} +").is_deny());
        // Separate-token flag values must not be mistaken for the command.
        assert!(bash("find . -exec timeout -s KILL 5 rm -rf {} +").is_deny());
        assert!(bash("find . -exec stdbuf -o 0 rm -rf {} +").is_deny());
    }

    #[test]
    fn bg3_exec_wrappers_are_denied_at_top_level_too() {
        // The same wrapper gap existed outside `find` — `command_index` is the
        // shared command-word resolver.
        assert!(bash("timeout 5 rm -rf /some/path").is_deny());
        assert!(bash("stdbuf -o0 rm -rf /some/path").is_deny());
        assert!(bash("flock /tmp/lock rm -rf /some/path").is_deny());
        assert!(bash("setsid rm -rf /some/path").is_deny());
        assert!(bash("chroot /newroot rm -rf /some/path").is_deny());
        // Wrapping something harmless stays harmless.
        assert_eq!(bash("timeout 30 cargo test"), Decision::Allow);
        assert_eq!(bash("flock /tmp/lock ls -la"), Decision::Allow);
    }

    #[test]
    fn bg3_semicolon_terminated_multi_exec_is_denied() {
        // `\;` splits the line, so the destructive clause arrives as a segment
        // whose command word is `-exec`.
        assert!(bash(r"find . -name x -exec grep rm {} \; -exec rm -rf {} \;").is_deny());
        // The `+`-terminated twin (the loop itself) keeps working.
        assert!(bash("find . -name x -exec grep rm {} +  -exec rm -rf {} +").is_deny());
        // Every exec predicate spelling routes back through analyze_find.
        assert!(bash(r"find . -name x -print \; -execdir rm -rf {} \;").is_deny());
        assert!(bash(r"find . -name x -print \; -ok rm -rf {} \;").is_deny());
        assert!(bash(r"find . -name x -print \; -okdir rm -rf {} \;").is_deny());
        // A wrapper inside the split-off clause is reached too.
        assert!(bash(r"find . -name x -print \; -exec timeout 5 rm -rf {} \;").is_deny());
    }

    #[test]
    fn bg3_find_exec_data_tokens_stay_allowed() {
        // CA-blastguard-016 must NOT regress: searching for the literal string
        // `rm` is ordinary work, in both terminator forms.
        assert_eq!(
            bash(r"find . -name '*.md' -exec grep -l 'rm' {} \;"),
            Decision::Allow
        );
        assert_eq!(
            bash("find . -name '*.md' -exec grep -l 'rm' {} +"),
            Decision::Allow
        );
        assert_eq!(bash(r#"find . -name "rm" -print"#), Decision::Allow);
        assert_eq!(
            bash(r"find . -exec grep -rn 'rm -rf' {} \;"),
            Decision::Allow
        );
        // `find -delete` behaviour is unchanged by the re-analysis.
        assert!(bash("find . -delete").is_deny());
        assert!(bash("find . -name '*.tmp' -delete").is_deny());
    }

    // BG-1: the payload of `-c` can sit behind a value-taking option (whose
    // ARGUMENT does not start with `-`) or behind a `+`-prefixed option.
    #[test]
    fn bg1_dash_c_payload_behind_value_taking_options() {
        assert!(bash("bash -c -o pipefail 'rm -rf /some/path'").is_deny());
        assert!(bash("bash -c -O extglob  'rm -rf /some/path'").is_deny());
        assert!(bash("bash -c +o history  'rm -rf /some/path'").is_deny());
        assert!(bash("bash -c +O extglob  'rm -rf /some/path'").is_deny());
        assert!(bash("sh -c -o errexit 'rm -rf /some/path'").is_deny());
        // Multiple options stacked before the payload.
        assert!(bash("bash -c -o pipefail -x -- 'rm -rf /some/path'").is_deny());
        // Controls that already denied.
        assert!(bash("bash -c -e          'rm -rf /some/path'").is_deny());
        assert!(bash("bash -c -- 'rm -rf /some/path'").is_deny());
        assert!(bash("bash -c -x 'rm -rf /some/path'").is_deny());
        assert!(bash(r#"sh -c "rm -rf /some/path" --help"#).is_deny());
        assert!(bash(r#"sh -c "rm -rf /some/path" ; true"#).is_deny());
    }

    #[test]
    fn bg1_benign_dash_c_invocations_stay_allowed() {
        assert_eq!(bash(r#"sh -c "ls""#), Decision::Allow);
        assert_eq!(bash("bash -c 'echo hi'"), Decision::Allow);
        assert_eq!(bash("bash -c -o pipefail 'cargo test'"), Decision::Allow);
        assert_eq!(bash("sh -c ''"), Decision::Allow);
        assert_eq!(bash("bash -c"), Decision::Allow);
    }

    // ---- D1: the `is_temp_scratch` carve-out was a universal bypass ----
    // `exclude::normalize` does not resolve `..`, so a `/tmp/../` prefix made
    // ANY target look like temp scratch and disabled rule 2 entirely. Each row
    // below was a VERIFIED live ALLOW against the built hook binary.
    #[test]
    fn d1_temp_prefix_traversal_no_longer_bypasses_redirect_rule() {
        for c in [
            "echo pwned > /tmp/../etc/hosts",
            "echo x > /var/tmp/../../etc/hosts",
            "echo x > /private/tmp/../../Users/yuki/.zshrc",
            "echo x > /var/folders/../../etc/hosts",
            "echo x > /tmp//../etc/hosts",
            // The `/private` twins of the same traversal.
            "echo x > /private/var/tmp/../../etc/hosts",
            "echo x > /private/var/folders/../../etc/hosts",
        ] {
            assert!(bash(c).is_deny(), "expected deny: {c}");
        }
    }

    #[test]
    fn d1_plain_temp_redirects_are_denied_not_carved_out() {
        // The carve-out is GONE, not narrowed. These are false positives and
        // are the accepted cost — do not re-add a temp-directory exemption.
        for c in [
            "cargo test 2> /tmp/log",
            "cargo test > /tmp/log",
            "cargo test 2> /var/tmp/log",
            "make 2> /var/folders/xy/z/T/err",
            "cargo test 2> /private/tmp/log",
        ] {
            assert!(bash(c).is_deny(), "expected deny: {c}");
        }
        // Non-truncating ways to capture output are still allowed, so there is
        // always a rephrasing available.
        assert_eq!(bash("cargo test 2>> /tmp/log"), Decision::Allow);
        assert_eq!(bash("cargo test 2> /dev/null"), Decision::Allow);
    }

    // ---- D2: flock's `-c` is a shell command string, not an opaque value ----
    #[test]
    fn d2_flock_dash_c_payload_is_reanalysed() {
        assert!(bash("flock /tmp/l -c 'rm -rf /Users/yuki/src'").is_deny());
        assert!(bash("flock -c 'rm -rf /Users/yuki/src' /tmp/l").is_deny());
        // Long-form spelling of the same option.
        assert!(bash("flock /tmp/l --command 'rm -rf /Users/yuki/src'").is_deny());
        // A benign flock -c payload stays allowed.
        assert_eq!(bash("flock /tmp/l -c 'cargo test'"), Decision::Allow);
        assert_eq!(bash("flock -c 'ls -la' /tmp/l"), Decision::Allow);
    }

    /// 138607bc: the ATTACHED long form `--command=<payload>` glues the value to
    /// the flag token by `=`. `payloads_after` only inspected tokens AFTER the
    /// flag, so the inline payload was never extracted and this was a verified
    /// depth-0 Allow bypass. The value must be reconstructed and re-analysed.
    #[test]
    fn d2_flock_attached_long_command_payload_is_reanalysed() {
        assert!(
            bash("flock /tmp/l --command='rm -rf /Users/yuki/src'").is_deny(),
            "attached --command= with a destructive payload must be denied"
        );
        assert!(bash("flock --command='rm -rf /Users/yuki/src' /tmp/l").is_deny());
        // Double-quoted attached payload too.
        assert!(bash("flock /tmp/l --command=\"rm -rf /Users/yuki/src\"").is_deny());
        // A benign attached payload stays allowed (no over-block).
        assert_eq!(bash("flock /tmp/l --command='cargo test'"), Decision::Allow);
    }

    /// 138607bc twin: the GLUED short form `-c<payload>` (no space) hides the
    /// value in the same token, after `c`. getopt allows this and it was the
    /// same bypass class as `--command=`.
    #[test]
    fn d2_flock_glued_short_command_payload_is_reanalysed() {
        assert!(
            bash("flock /tmp/l -c'rm -rf /Users/yuki/src'").is_deny(),
            "glued -c'…' with a destructive payload must be denied"
        );
        // Benign glued payload stays allowed.
        assert_eq!(bash("flock /tmp/l -c'cargo test'"), Decision::Allow);
    }

    // ---- D3: long-form wrapper flags with SEPARATE values ----
    // `wrapper_flag_takes_value` returned false for any flag whose length was
    // not 2, so the VALUE token was eaten as the wrapper's leading operand and
    // the real command word was misresolved. Every row was a VERIFIED live
    // ALLOW; the short-form twins denied correctly, which is why the previous
    // round's tests missed this entirely.
    #[test]
    fn d3_long_form_wrapper_flags_do_not_bypass_detection() {
        for c in [
            "timeout --kill-after 10 5 rm -rf /Users/yuki/src",
            "timeout --signal KILL 5 rm -rf /Users/yuki/src",
            "chroot --userspec root /newroot rm -rf /Users/yuki/src",
            "stdbuf --output 0 rm -rf /Users/yuki/src",
            "flock --wait 5 /tmp/l rm -rf /Users/yuki/src",
            "sudo --user root rm -rf /Users/yuki/src",
            "env --unset FOO rm -rf /Users/yuki/src",
            "nice --adjustment 5 rm -rf /Users/yuki/src",
        ] {
            assert!(bash(c).is_deny(), "expected deny: {c}");
        }
    }

    #[test]
    fn d3_long_form_wrapper_flags_inside_find_exec_too() {
        // The same resolver backs `find -exec`, so the gap existed there too.
        for c in [
            "find . -exec timeout --kill-after 10 5 rm -rf {} +",
            "find . -exec sudo --user root rm -rf {} +",
            "find . -exec stdbuf --output 0 rm -rf {} +",
        ] {
            assert!(bash(c).is_deny(), "expected deny: {c}");
        }
    }

    #[test]
    fn d3_benign_long_form_wrapper_invocations_stay_allowed() {
        // Widening the candidate set must not deny ordinary wrapped work.
        for c in [
            "timeout --kill-after 10 5 cargo test",
            "timeout --signal KILL 30 cargo build",
            "sudo --user root ls -la",
            "env --unset FOO cargo test",
            "nice --adjustment 5 cargo build --release",
            "stdbuf --output 0 grep -rn 'rm -rf' src/",
            "flock --wait 5 /tmp/l cargo test",
            "chroot --userspec root /newroot ls",
        ] {
            assert_eq!(bash(c), Decision::Allow, "expected allow: {c}");
        }
    }

    // ---- ASK-2: an unrecognised head in front of a destructive command line ----

    #[test]
    fn unknown_wrapper_ask_covers_every_unrecognized_head() {
        // The rule this is named for: the wrapper list cannot be completed, so a
        // local script / just recipe / Makefile shim in front of a genuinely
        // destructive command line is the unknown middle — ask, do not allow.
        for c in [
            "my-cleanup-wrapper rm -rf /some/path",
            "./deploy.sh git reset --hard",
            "just shred secret",
        ] {
            let d = bash(c);
            assert!(d.is_ask(), "expected ask for {c}, got {d:?}");
            // And with nobody to answer it must land on refusal, never allow.
            assert!(d.hardened().is_deny());
        }
    }

    #[test]
    fn a_deny_on_the_line_outranks_the_unknown_wrapper_ask() {
        // Ranking Deny > Ask: where the ordinary rules reach a verdict of their
        // own, the unknown head must not DOWNGRADE it to a question. Both of
        // these also satisfy the ask rule's shape, so they prove the ordering
        // rather than merely the absence of an ask.
        for c in [
            // Redirect deny from the outer line, unknown head in front.
            "my-wrapper rm -rf /some/path > existing",
            // Second segment denies on its own; the first only asks.
            "my-wrapper rm -rf /some/path; rm -rf /other/path",
        ] {
            let d = bash(c);
            assert!(
                d.is_deny(),
                "a known-destructive line must stay a deny: {d:?}"
            );
        }
    }

    #[test]
    fn the_ask_rule_examines_only_one_tail_by_design() {
        // Documented narrowness, pinned so it stays a deliberate limit rather
        // than drifting into an accident: exactly ONE tail is re-analysed, and
        // only a DENY from it asks. A nested unknown head (`just` → `nuke` →
        // `shred`) therefore does NOT chain into an ask — that case was, and
        // remains, the pre-existing Allow. Fanning out per position is what made
        // the recursion exponential in D4, so widening this needs a bounded
        // design, not a one-line change.
        assert_eq!(bash("just nuke shred secret"), Decision::Allow);
    }

    #[test]
    fn unknown_wrapper_ask_does_not_fire_on_redirects_from_the_outer_line() {
        // Regression. The tail used to be re-joined from EVERY remaining token,
        // redirect punctuation included, so the outer line's own redirect was
        // re-parsed and blamed on the head — turning ordinary work into asks
        // about `cargo` / `make` / `echo`. A redirect is judged by the outer
        // analysis regardless of the head, so dropping it here loses nothing.
        for c in [
            "cargo test 2>&1",
            "make >&2",
            "echo x >&2",
            "run 1>&2",
            "cargo test >> log.txt",
            "cmd 3>&2",
        ] {
            assert_eq!(bash(c), Decision::Allow, "expected allow: {c}");
        }
        // The destructive redirect forms still deny — via the redirect rule,
        // not via the wrapper ask.
        for c in ["echo x > existing", "echo x &> target", "make >& out.txt"] {
            assert!(bash(c).is_deny(), "expected deny: {c}");
        }
    }

    #[test]
    fn unknown_wrapper_ask_skips_a_whole_quoted_string_not_just_its_first_token() {
        // Regression. Skipping only the token that OPENS the quote let the scan
        // resume INSIDE the string: `echo 'eval rm -rf /'` skipped `'eval` and
        // then read `rm -rf /'` as a command line, asking about `echo` over text
        // that is an argument and is never executed.
        for c in [
            "echo 'eval rm -rf /'",
            "echo 'rm -rf /'",
            "echo \"git reset --hard\"",
            "mytool -x 'rm -rf' file",
            "grep -rn 'shred secret' src/",
        ] {
            assert_eq!(bash(c), Decision::Allow, "expected allow: {c}");
        }
        // But a string that is actually HANDED TO A SHELL is still re-analysed
        // by the arm that owns that construct — this exemption is about text
        // sitting in argument position, not about quoting as a bypass.
        assert!(bash("sh -c 'rm -rf /some/path'").is_deny());
        assert!(bash("eval 'rm -rf /some/path'").is_deny());
    }

    // ---- D4: `find -exec` re-analysis was exponential ----
    // `analyze_find` re-analyses the whole tail for EVERY `-exec` position, and
    // the tail still contains the other `-exec` tokens: branching factor N,
    // depth 8, so O(N^8). Measured before the fix on ONE invocation: n=20 →
    // 2.2s, n=24 → 8.2s, n=28 → 30s, n=32 (503 bytes) → >60s, killed.
    #[test]
    fn d4_exponential_find_exec_is_bounded_and_denies() {
        let mut cmd = String::from("find . ");
        for _ in 0..32 {
            cmd.push_str("-exec find . ");
        }
        cmd.push_str("-exec echo {} ");
        for _ in 0..33 {
            cmd.push_str("+ ");
        }
        let start = std::time::Instant::now();
        let d = bash(&cmd);
        let elapsed = start.elapsed();
        // Availability: a PreToolUse hook must never hang the user's turn.
        assert!(
            elapsed < std::time::Duration::from_secs(2),
            "analysis took {elapsed:?}, expected well under 2s"
        );
        // Correctness: budget exhaustion BLOCKS. A command too complex to
        // analyse must never be waved through unanalysed.
        //
        // It is an `Ask`, not a `Deny`: "we did not finish looking" is not a
        // verdict about the command. With no human to answer, the hook hardens
        // it back to the deny this used to be (`Decision::hardened`), so this is
        // never weaker than before — but asserting `is_deny()` here would pin
        // the wrong half of that contract. What must hold is that it is NEVER
        // an Allow.
        assert!(d.is_blocking(), "unfinished analysis must block, got {d:?}");
        // WHICH bound saves this input is not the invariant. This command is
        // both 33-wide (fan-out → budget) and 33-deep (nesting → depth cap);
        // both are "we did not finish looking", both Ask, and both harden to the
        // pre-existing Deny. Pinning one exact reason pinned an implementation
        // detail of which limit is observed first — when `depth_exhausted` gave
        // the depth cap a verdict of its own (it previously resolved to Allow),
        // the depth reason started winning here, with no change to the verdict.
        //
        // The budget bound is NOT left untested by this loosening: a
        // fan-out-only, shallow-nesting command still reports the budget reason
        // verbatim, pinned in `tests/depth_limit_fail_open.rs`
        // (`budget_bound_is_still_reachable_with_its_own_reason`,
        // `budget_resets_between_top_level_calls_measured_on_a_budget_bound_input`).
        let hardened = d.clone().hardened();
        assert!(
            hardened.is_deny(),
            "with no human available the ask must harden to the pre-existing \
             deny, got {hardened:?}"
        );
        let reason = match &hardened {
            Decision::Deny(r) | Decision::Ask(r) => r.clone(),
            Decision::Allow => String::new(),
        };
        assert!(
            reason.contains("safety budget") || reason.contains("recursion depth limit"),
            "the deny must say analysis did not finish, and say WHICH limit was \
             hit (budget or depth) — a reason naming neither would mean this \
             input is being blocked for some unrelated cause. got: {reason}"
        );
    }

    #[test]
    fn d4_analysis_limits_do_not_leak_between_top_level_calls() {
        // Renamed from `d4_budget_resets_between_top_level_calls`: this input is
        // 33 deep as well as 33 wide, so since the depth cap started blocking it
        // is the DEPTH limit that answers here, not the budget. The name claimed
        // a budget property the case no longer demonstrates. The property it
        // DOES pin — no per-call analysis limit leaks into the next call — is
        // still worth having, so it keeps the case under an honest name.
        // A genuinely budget-bound version lives in
        // `tests/depth_limit_fail_open.rs`.
        let mut cmd = String::from("find . ");
        for _ in 0..32 {
            cmd.push_str("-exec find . ");
        }
        cmd.push_str("-exec echo {} +");
        // Blocking (an Ask that hardens to Deny — see the test above), and the
        // point of this case: whatever limit it consumed must not leak into the
        // benign calls below.
        assert!(bash(&cmd).is_blocking());
        assert_eq!(bash("ls -la"), Decision::Allow);
        assert_eq!(bash("cargo test"), Decision::Allow);
    }

    #[test]
    fn bg_baseline_matrix_stays_intact() {
        // The unchanged half of the verification matrix, kept as one guard.
        for c in [
            "rm -rf /some/path",
            r"\rm -rf /some/path",
            "'rm' -rf /some/path",
            r#""rm" -rf /some/path"#,
            "/bin/rm -rf /some/path",
            "shred --help > /some/path",
        ] {
            assert!(bash(c).is_deny(), "expected deny: {c}");
        }
        for c in [
            "ls -la",
            "echo 'rm -rf /some/path'",
            r#"grep -n "rm" src/detect.rs"#,
        ] {
            assert_eq!(bash(c), Decision::Allow, "expected allow: {c}");
        }
    }
}
