use super::{
    has_glob_meta, has_unresolvable_expansion, is_relative_operand, is_shell_identifier,
    line_changes_cwd_before, referenced_variable_name, resolve_expanded_command_word,
    split_segments_with_separators, CommandWordOrigin, Ctx, CwdState,
};
use crate::exclude;
use crate::model::Decision;
use crate::scope::Placement;
use harness_core::verdict::Determination;

/// Re-express a redirect target relative to where its segment really runs.
///
/// `Ok(target)` is the text every axis should judge: unchanged for an
/// absolute, `~`, or expansion-valued target and for a relative one in a
/// segment no `cd` precedes (its base is the line's own starting directory,
/// `ctx.raw_base`, exactly as before); `<dir>/<target>` normalised for a
/// relative target after a `cd` this analysis resolved to `dir` — the same
/// re-expression [`rewrite_relative_operands`] applies to verb operands, so
/// the result is again relative to the line's starting directory (or
/// absolute).
///
/// `Err(raw)` means the target cannot be placed: it is relative, and either
/// the cwd before its segment is [`CwdState::Unknown`] (`cd $X`, `popd`) or the
/// per-segment scan could not be corroborated (`seg_idx` is `None`) on a line
/// that changes directory. Judging it against the starting directory would
/// probe and place the WRONG file, so the caller must not (CLAUDE.md §3).
pub(super) fn place_redirect_target(
    cmd: &str,
    seg_idx: Option<usize>,
    seg_cwds: &[CwdState],
    target: String,
) -> Result<String, String> {
    if !is_relative_operand(&target) || has_unresolvable_expansion(&target) {
        return Ok(target);
    }
    let state = match seg_idx {
        Some(idx) => seg_cwds.get(idx).cloned().unwrap_or(CwdState::Unknown),
        None if line_changes_cwd_before(cmd, usize::MAX) => CwdState::Unknown,
        None => CwdState::Root,
    };
    match state {
        CwdState::Root => Ok(target),
        CwdState::Known(dir) => Ok(exclude::normalize(&format!(
            "{}/{}",
            dir.trim_end_matches('/'),
            target
        ))),
        CwdState::Unknown => Err(target),
    }
}

/// The spelling of a redirect target that [`physical_landing`] should resolve:
/// `<dir>/<target>` with the target's `..` left UNcollapsed when a `cd` placed
/// the segment in `dir`, otherwise the target unchanged.
///
/// [`place_redirect_target`] collapses `..` lexically, which is right for the
/// `cd` chain itself (a shell's `cd` is logical by default: `cd link && cd ..`
/// returns to where it started) but wrong for the redirect: the kernel opens
/// `../x` from the directory the `cd` really entered, so `cd link3 && echo x >
/// ../settings.json` with `link3 -> .claude/sub` writes `.claude/settings.json`,
/// not `settings.json`. Keeping `dir` (logical) and the raw target lets the
/// resolver follow `link3` before it applies `..`. Only meaningful when
/// [`place_redirect_target`] returned `Ok` for the same target.
pub(super) fn physical_redirect_spelling(
    seg_idx: Option<usize>,
    seg_cwds: &[CwdState],
    target: &str,
) -> String {
    if !is_relative_operand(target) || has_unresolvable_expansion(target) {
        return target.to_string();
    }
    match seg_idx.and_then(|idx| seg_cwds.get(idx)) {
        // `cd link && cd ..` tracks as `Known("")`: the line's own base, so
        // the target is already relative to it (never `/<target>`).
        Some(CwdState::Known(dir)) if !dir.is_empty() => {
            format!("{}/{}", dir.trim_end_matches('/'), target)
        }
        _ => target.to_string(),
    }
}

/// True ONLY when a truncating-redirect `target` (already placed by
/// [`place_redirect_target`]) names an existing file whose truncation is
/// disposable temp work (user ruling 2026-10-28): ALL of
///   * no glob meta and no shell expansion in the spelling;
///   * no path component is exactly `.git`, in the spelling or in the real
///     path, and the normalised spelling is not a protected path
///     ([`exclude::is_protected_path`]; the caller has already refused those,
///     this re-check keeps the rule closed on its own);
///   * [`SafeRoots::classify_temp`] places it `Inside` a FIXED temp root
///     (the real `/tmp` or `/private/tmp` — never `$TMPDIR`, never a cache
///     root, never the root itself), which also refuses a residual `..`,
///     any symlink between the root and the target (the target included) and a
///     non-canonical payload cwd;
///   * no directory from the target's real parent up to that root holds a
///     `.git` entry ([`SafeRoots::no_git_entry_up_to`]; any IO error answers
///     false), so a file inside a git work tree or git dir under tmp keeps the
///     verdict it had before this rule;
///   * its real path is an existing regular file with exactly one hard link
///     ([`SafeRoots::is_single_link_regular_file`]): a second link would let
///     the truncation destroy bytes under a name outside the root.
///
/// Every other answer — including every `Undetermined` — is `false`, and the
/// caller keeps the verdict it had before this rule (the confined Ask or the
/// Deny). There is no default that answers `true`.
pub(super) fn temp_disposable_redirect_target(target: &str, ctx: &Ctx<'_>) -> bool {
    if has_glob_meta(target) || has_unresolvable_expansion(target) {
        return false;
    }
    let has_git_component = |p: &str| p.split('/').any(|c| c == ".git");
    if has_git_component(target) || exclude::is_protected_path(&exclude::normalize(target)) {
        return false;
    }
    let Determination::Known(Placement::Inside { root, .. }) =
        ctx.scope.classify_temp(target, ctx.raw_base.as_deref())
    else {
        return false;
    };
    let absolute = if target.starts_with('/') {
        exclude::normalize(target)
    } else {
        match ctx.raw_base.as_deref() {
            Some(base) if base.starts_with('/') => {
                exclude::normalize(&format!("{}/{target}", base.trim_end_matches('/')))
            }
            _ => return false,
        }
    };
    let Some(real) = ctx.scope.resolve(&absolute) else {
        return false;
    };
    !has_git_component(&real)
        && !exclude::is_protected_path(&real)
        && ctx.scope.no_git_entry_up_to(&real, &root)
        && ctx.scope.is_single_link_regular_file(&real)
}

/// The Ask for a relative redirect target [`place_redirect_target`] could not
/// place. `op` is the operator as typed (`>` or `>>`).
pub(super) fn unplaceable_redirect_ask(op: &str, target: &str) -> Decision {
    Decision::ask(format!(
        "'{op} {target}' is a RELATIVE redirect after an earlier cd/pushd/popd whose target \
directory blastguard could not statically resolve — it cannot tell whether this lands in a \
system directory or on a protected gate/config path, and refuses to guess"
    ))
}

pub(super) fn redirect_target_is_safe(target: &str, ctx: &Ctx<'_>) -> bool {
    let t = exclude::normalize(target);
    // A protected path is never "safe" to truncate, even though most of them
    // (`.claude/settings.json`, `deny.toml`, …) also match `is_config_file`.
    // Checked first so the config-file exemption cannot re-open the hole.
    // A protected-NAMED file inside a linked worktree checkout is the
    // checkout's own file (`Ctx::in_worktree_checkout`) and is judged like any
    // other path from here on — it is not thereby "safe", it just loses the
    // protected-path precedence.
    if exclude::is_protected_path(&t) && !ctx.in_worktree_checkout(target) {
        return false;
    }
    matches!(t.as_str(), "/dev/null" | "/dev/stdout" | "/dev/stderr") || exclude::is_config_file(&t)
}

// D1 (verified universal bypass, REMOVED — do not re-add): a `is_temp_scratch`
// predicate used to allow any redirect target under `/tmp`, `/var/tmp`,
// `/var/folders` and their `/private` twins, so that `cargo test 2> /tmp/log`
// would not be denied. It was a raw `starts_with` prefix test, and
// at the time `exclude::normalize` did not resolve `..`, so `/tmp/../etc/hosts`,
// `/var/tmp/../../etc/hosts`, `/private/tmp/../../Users/yuki/.zshrc`,
// `/var/folders/../../etc/hosts` and `/tmp//../etc/hosts` all passed the
// prefix test and thereby disabled the ENTIRE truncating-redirect rule for
// ANY target on the line.
//
// It was NOT replaced with a `..`-resolving version, and 0.2.20 did not
// reinstate it even though `normalize` DOES resolve `..` now. Those are two
// separate questions: resolving `..` removes one of the ways the carve-out
// could be fooled, it does not make a convenience carve-out on the permissive
// side of a gate a good idea (`~`, `$TMPDIR` and symlinks were all still
// unresolvable here).
//
// 0.2.51 UPDATE — read this together with `crate::scope`, which answers the
// four objections above by name (`..` lexically resolved with any residue
// Undetermined; `~`/`$VAR`/globs/`{}` refused as non-paths; `$TMPDIR` read from
// the hook's own environment instead of expanded out of the command line;
// symlinks resolved through an injected canonicaliser). The outcome differs
// too, and that is the part that matters here: the deleted predicate produced
// an **Allow**, whereas a confined placement produces an **Ask**, which
// `crate::interactive` hardens straight back to a Deny wherever no human can
// answer. So `> /tmp/log` is no longer a flat refusal in an interactive
// session — it is a one-keypress question — while remaining a Deny in every
// headless/agent context, which is where the old carve-out's fail-open would
// have mattered. `> $TMPDIR/log` and `> /tmp/../etc/hosts` are still refused:
// the first is an unexpanded variable, the second normalises to `/etc/hosts`
// and lands outside every safe root.

/// Resolve a redirect TARGET that is a variable expansion — alone (`> "$P"`)
/// or followed by a literal suffix (`> $P/x.log`) — to the literal path it
/// will name, when an assignment EARLIER IN THE SAME COMMAND determines it.
/// `None` means "not resolvable" and the caller must keep judging the raw
/// token exactly as before.
///
/// The suffix form was not handled until 0.2.98 (backlog af1565aa), and the
/// gap was visible in the only place it matters: `P=<dir>/absent.log; : > $P`
/// reached **Allow** while `P=<dir>; : > $P/absent.log` — the same write —
/// reached **Deny**, because the second target never cleared
/// [`referenced_variable_name`], which requires the WHOLE word to be the
/// reference. The axes then received the unplaceable raw token and the line
/// fell through to the flat Deny, exactly the "one effect, two verdicts,
/// decided by spelling" defect the bare form was fixed for.
///
/// `seg_idx` is the segment the occurrence being judged lives in, supplied by
/// [`redirect_target_occurrences`]. It is NOT derived from the token text:
/// two redirects can spell the same token and name different files.
///
/// Why this exists. Every downstream axis a redirect target is judged on —
/// protected-path, system-directory, recoverability, confinement — is a
/// question about a PATH, and `"$P"` is not a path. Before this, all four
/// silently failed to place the target and the line fell through to the flat
/// Deny, so `P=/tmp/x.log; echo hi > "$P"` was refused while the literal
/// `echo hi > /tmp/x.log` was allowed — the same effect, decided oppositely by
/// spelling alone. Measured on this repo's own traffic (183 unique commands,
/// 2026-09-14): 4 denies, ALL of this class, none of them a real hazard.
///
/// It does NOT weaken the gate, because it only ever hands the axes a MORE
/// specific path than the unplaceable token they get today:
/// - a resolved target that is protected / in a system dir / unrecoverable is
///   still blocked, now by the axis that actually names the reason;
/// - anything this cannot resolve returns `None` and keeps today's verdict.
///
/// The believability rules are NOT reimplemented here. This delegates to
/// [`resolve_expanded_command_word`], so a redirect target inherits the same
/// contract a command word has: a later rebinding the scan cannot read, an
/// array assignment, a conditionally-executed assignment, a value that is
/// itself an expansion, an empty value, and an assignment that never reaches
/// the parent shell all resolve to something other than `Literal` and
/// therefore to `None` here. Those are the cases where "which file is this"
/// has no answer, and per CLAUDE.md §3 no answer is not a permissive answer.
pub(super) fn resolve_redirect_target_at(
    cmd: &str,
    seg_idx: usize,
    target: &str,
) -> Option<String> {
    // A literal path: nothing to resolve, nothing to change. Checked via the
    // whole-word form FIRST so the pre-existing bare case keeps reaching
    // `resolve_expanded_command_word` with the exact token it always did.
    if referenced_variable_name(target).is_some() {
        return match resolve_expanded_command_word(cmd, seg_idx, target) {
            CommandWordOrigin::Literal(value) => Some(value),
            // `AssignedButUnknowable` and `NoReachingAssignment` are both "this
            // token does not name a path I can see" - the restrictive side.
            _ => None,
        };
    }
    // Reference + literal suffix. The believability rules are NOT duplicated:
    // only the REFERENCE goes to `resolve_expanded_command_word`, so a guarded
    // assignment, an array, a value that is itself an expansion, a later
    // unreadable rebind and an empty value all still resolve to something other
    // than `Literal` and therefore to `None` here.
    let (reference, suffix) = reference_with_literal_suffix(target)?;
    match resolve_expanded_command_word(cmd, seg_idx, &reference) {
        CommandWordOrigin::Literal(value) => Some(format!("{value}{suffix}")),
        _ => None,
    }
}

/// Split a redirect target into the single variable reference it BEGINS with
/// and the literal suffix that follows, when the word has exactly that shape.
///
/// `None` means "not that shape", which the caller treats as unresolvable —
/// never as "no suffix". The two are different answers and collapsing them
/// would hand the caller a path the shell will not produce.
///
/// What the suffix may be is deliberately narrow, because the value is
/// CONCATENATED onto a resolved path and anything the shell would further
/// transform makes that concatenation a different file:
///
/// * no `$` or backtick (via [`has_unresolvable_expansion`]) — a second
///   expansion is a second unknown, and resolving half of a two-unknown target
///   would answer about a path that does not exist;
/// * no quote character and no backslash — `"$P"'/x'` really does concatenate
///   in bash, but reading alternating quote runs is a second parser and this
///   function is not it, so the shape is refused rather than guessed at;
/// * no glob metacharacter (`*`, `?`, `[`) — a glob can name MANY files or
///   none, so there is no single path to hand the axes. `bash -c 'ls /etc/*'`
///   expanding to many words is the whole point of a glob; a verdict about one
///   of them would be a verdict about the wrong file.
///
/// The unbraced/braced distinction is not cosmetic and was measured rather than
/// assumed (`bash -c`, 2026-10-02):
///
/// ```text
/// P=/tmp; echo $Pfoo    -> (empty)    # a reference to `Pfoo`, NOT $P + "foo"
/// P=/tmp; echo $P_x     -> (empty)    # `_` continues the name too
/// P=/tmp; echo $P/foo   -> /tmp/foo
/// P=/tmp; echo $P.bak   -> /tmp.bak
/// P=/tmp; echo ${P}foo  -> /tmpfoo    # the braces end the name
/// ```
///
/// So for the unbraced form the name is the LONGEST identifier run after `$`,
/// which makes the suffix start with a non-identifier character by
/// construction — there is no separate rule to get wrong. For the braced form
/// the suffix may start with anything, including a letter.
pub(super) fn reference_with_literal_suffix(word: &str) -> Option<(String, String)> {
    // One level of double quoting, stripped the way `referenced_variable_name`
    // strips it: `"$P/x"` and `"$P"/x` expand identically, so both must reach
    // the same answer. An interior quote is refused above.
    if word.len() >= 2 && word.starts_with('"') && word.ends_with('"') {
        let mid = &word[1..word.len() - 1];
        if mid.contains('"') {
            return None;
        }
        return reference_with_literal_suffix(mid);
    }
    // `"$P"/x`: the reference is quoted and the suffix is not. Split at the
    // closing quote and let the recursive call above judge the quoted half.
    if let Some(rest) = word.strip_prefix('"') {
        let close = rest.find('"')?;
        let quoted_reference = &word[..close + 2];
        let suffix = &rest[close + 1..];
        let name = referenced_variable_name(quoted_reference)?;
        return literal_suffix(suffix).map(|sfx| (format!("${name}"), sfx));
    }
    let after = word.strip_prefix('$')?;
    if let Some(braced) = after.strip_prefix('{') {
        let close = braced.find('}')?;
        let name = &braced[..close];
        if !is_shell_identifier(name) {
            return None;
        }
        let suffix = &braced[close + 1..];
        return literal_suffix(suffix).map(|sfx| (format!("${{{name}}}"), sfx));
    }
    let end = after
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .unwrap_or(after.len());
    let name = &after[..end];
    if !is_shell_identifier(name) {
        return None;
    }
    literal_suffix(&after[end..]).map(|sfx| (format!("${name}"), sfx))
}

/// The suffix, when it is text the shell will pass through unchanged.
///
/// An EMPTY suffix returns `None` on purpose: that is the whole-word case, and
/// [`resolve_redirect_target_at`] has already handled it with the raw token.
/// Returning `Some("")` here would send the same word down the suffix path with
/// a synthesised reference token, which is a second spelling of one case for no
/// gain.
///
/// A `..` COMPONENT is refused because concatenation cannot name the file the
/// write lands in. `..` is resolved by the KERNEL against the directory a
/// component REALLY is, not against the text, so one symlink before it makes the
/// lexical spelling point somewhere else entirely. Measured non-destructively
/// (`scratchpad/meas_kernel_dotdot.py`, 2026-10-02) with `proj/esc` a symlink to
/// `other/deep`:
///
/// ```text
/// printf WRITTEN > <base>/proj/esc/../victim.txt
///   lexical spelling names <base>/proj/victim.txt   -> UNTOUCHED
///   the write landed in    <base>/other/victim.txt  <- the physical parent
/// ```
///
/// And the gap that follows from it is live, in the LITERAL path, independent of
/// this function (`scratchpad/meas_symlink_dotdot.py`, same day, deployed 0.2.97
/// and this build agreeing):
///
/// ```text
/// esc -> /
/// : > <base>/proj/esc/../../etc/fstab   realpath /etc/fstab  -> Allow   <-- hole
/// : > <base>/proj/esc/etc/fstab         (no `..`)            -> Deny    <-- caught
/// ```
///
/// So `no_symlink_below` catches a symlink in the spelling but not a symlink
/// consumed by a following `..`, and resolving a `..` suffix would carry that
/// Allow onto the variable spelling, turning a Deny into an Allow on a command
/// that truncates `/etc/fstab` for real. That is the one direction this change
/// must never move, so the suffix stays unresolved and keeps the verdict the
/// unexpanded token already earns (backlog 3ca56588 tracks the literal-path
/// hole; when it is closed this refusal is worth revisiting).
///
/// What this paragraph said in its first draft was WRONG and is recorded here
/// because the wrong version is the more tempting one: it claimed "the
/// downstream axes compare paths without normalising it". They do normalise —
/// `scope::normalize_abs` collapses `..`, `SafeRoots::lexical_absolute` answers
/// `undetermined` on any residue, and a purely lexical `..` is judged
/// identically whether it is spelled literally or through a variable. The cost
/// of this refusal is therefore a genuine false positive, not a free win:
/// `P=<dir>/sub; : > $P/../out.log` stays Deny while the literal is Allow. That
/// is spelling-dependence in the STRICT direction, which §3 prefers to the
/// permissive one, and it is the whole of what is being traded away.
pub(super) fn literal_suffix(suffix: &str) -> Option<String> {
    if suffix.is_empty() {
        return None;
    }
    if has_unresolvable_expansion(suffix) {
        return None;
    }
    if suffix.contains(['"', '\'', '\\', '*', '?', '[']) {
        return None;
    }
    if suffix.split('/').any(|component| component == "..") {
        return None;
    }
    Some(suffix.to_string())
}

/// Pair every truncating-redirect target on the line with the index of the
/// segment it occurs in, so each occurrence is resolved against the bindings
/// that actually reach IT.
///
/// Resolving by token text alone is a fail-open, found by adversarial review
/// with bash evidence that both writes land:
///
///   P=<proj>/ok.log; echo a > "$P"; P=/etc/fstab; echo b > "$P"   -> Allow
///
/// Every occurrence of the same spelling received the FIRST occurrence's
/// answer, so a line that rebinds the name between two redirects had its
/// second redirect judged as the first one's file. A here-document BODY
/// supplies such a decoy first hit too (a body is data, not a redirect), so
/// the decoy need not even be a live command.
///
/// Returns `None` - meaning "resolve nothing on this line, keep the
/// pre-existing verdict for every target" - unless the per-segment scan
/// reproduces the line-level scan of [`redirect_targets`] exactly, in order.
/// The two scanners can disagree (segment splitting cuts on `&`, `|`, `;` and
/// newlines, which also delimit fd-dup syntax), and a per-segment scan that
/// dropped or reordered a target would hand an occurrence the wrong segment's
/// bindings - the very defect this function exists to close. Per CLAUDE.md
/// section 3, a scan that cannot be corroborated is not a permissive scan.
/// Pinned by tests/redirect_target_resolution.rs.
pub(super) fn redirect_target_occurrences(cmd: &str) -> Option<Vec<(usize, String)>> {
    target_occurrences_by(cmd, redirect_targets)
}

/// The APPEND-redirect twin of [`redirect_target_occurrences`], with the same
/// corroboration contract (`None` = the per-segment scan disagrees with the
/// line-level one, so resolve nothing).
///
/// a83802ad: the append path used to judge the RAW token only, so
/// `P=/etc/fstab; echo x >> $P` was Allowed while `> $P` on the same line was
/// denied by the system-directory axis — the defect class
/// [`resolve_redirect_target_at`] closed for truncation, left open on its twin.
pub(super) fn append_target_occurrences(cmd: &str) -> Option<Vec<(usize, String)>> {
    target_occurrences_by(cmd, append_redirect_targets)
}

pub(super) fn target_occurrences_by(
    cmd: &str,
    scan: fn(&str) -> Vec<String>,
) -> Option<Vec<(usize, String)>> {
    let line_level = scan(cmd);
    let mut per_segment: Vec<(usize, String)> = Vec::new();
    for (seg_idx, seg) in split_segments_with_separators(cmd).iter().enumerate() {
        for target in scan(&seg.text) {
            per_segment.push((seg_idx, target));
        }
    }
    if per_segment.len() != line_level.len() {
        return None;
    }
    if per_segment
        .iter()
        .zip(line_level.iter())
        .any(|((_, a), b)| a != b)
    {
        return None;
    }
    Some(per_segment)
}

/// Where a redirect-adjacent token ends.
///
/// Used by BOTH scans inside [`redirect_targets`] — the fd-dup token scan
/// (`>&<tok>`) and the redirect *target* scan — because they must agree on the
/// token boundary. They did not: only the target scan stopped at `)`, so
/// `x=$(printf hi 2>&1)` yielded the fd token `1)` (not all digits, therefore
/// "not an fd dup") but the target `1`, and blastguard denied a command that
/// touches no file. Any terminator added here applies to both by construction.
///
/// `)` is a terminator because `$(cmd 2>/dev/null)` / `$(cmd 2>&1)` are the
/// universal idioms for redirecting inside a command substitution; the closing
/// paren is shell syntax, never part of the filename or the fd number.
pub(super) fn is_redirect_token_end(b: u8) -> bool {
    b.is_ascii_whitespace() || b == b';' || b == b'|' || b == b'&' || b == b'>' || b == b')'
}

/// Step over the blanks between a redirect operator and its word, starting at
/// `j`. Only the shell's blanks (space, tab) are crossed, NEVER a newline:
/// a redirect word cannot sit on the next line (`echo x >` + newline +
/// `f` is a syntax error in bash and zsh, so nothing is opened).
///
/// Crossing the newline was backlog 0fccf1eb: a here-document body line
/// ending in `>` (`Co-Authored-By: A <n@a.com>`) followed by the closing
/// delimiter line was read as the redirect `> EOF`, and on a line with an
/// earlier `cd` the per-segment scan (which splits at that newline) could not
/// corroborate it, so the bogus target was reported as unplaceable.
pub(super) fn skip_redirect_word_gap(bytes: &[u8], mut j: usize) -> usize {
    while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t') {
        j += 1;
    }
    j
}

/// True when the redirect word `bytes[start..end]` is empty because the line
/// ends right there (a newline, or the end of the text). That `>` names no
/// file: it is either here-document / quoted data or a shell syntax error, and
/// in neither case does the shell open anything. Any other empty word (`> ;`,
/// `> |`, `>\r`) keeps the empty target it always produced.
pub(super) fn redirect_word_missing_at_line_end(bytes: &[u8], start: usize, end: usize) -> bool {
    start == end && bytes.get(end).is_none_or(|b| *b == b'\n')
}

/// Every single `>` truncating-redirect target on the line, in order, outside
/// quotes. Skips `>>`, `&>>`, `>&<digit>` (fd dup), quoted `>`, Rust
/// arrows (`->`) and angle-bracket placeholders (`<value>`); catches every
/// explicit-fd truncating form (`2>f`, `0>f`, `3>f`, `1>f`) and the
/// truncating `>&<filename>` mirror of `&>`. Scanning the *whole* line (rather
/// than pre-split segments) keeps the fd-dup context (`2>&1`, `>&2`) intact
/// while still catching a truncating redirect in any later segment.
pub(super) fn redirect_targets(seg: &str) -> Vec<String> {
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
                    // Same terminator set as the target scan below — see
                    // `is_redirect_token_end`. The two MUST agree: when this
                    // scan ran longer than that one, `2>&1)` produced the
                    // fd token `1)` (not all-digits, so "not a dup") while the
                    // target scan produced the filename `1`, and a command
                    // that only dups a file descriptor was denied.
                    if is_redirect_token_end(bytes[k]) {
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
            j = skip_redirect_word_gap(bytes, j);
            let start = j;
            while j < bytes.len() {
                if is_redirect_token_end(bytes[j]) {
                    break;
                }
                j += 1;
            }
            let tok = &seg[start..j];
            // A bare `-` target is an fd CLOSE (`>&-` / `>& -`), not a file —
            // bash touches no file, so it is non-destructive.
            //
            // An EMPTY word at a line end (`x>` + newline, or `x>` at the end
            // of the text) is no redirect either: see `skip_redirect_word_gap`.
            if tok != "-" && !redirect_word_missing_at_line_end(bytes, start, j) {
                targets.push(tok.to_string());
            }
            i = j;
            continue;
        }
        i += 1;
    }
    targets
}

/// Every APPEND redirect target on the line (`>> f`, `&>> f`), in order,
/// outside quotes. The exact complement of [`redirect_targets`], which skips
/// these because appending does not truncate.
///
/// Callers must NOT treat a hit as destructive on its own — an append is
/// non-truncating and stays allowed for ordinary targets. It exists so that a
/// PROTECTED target (a settings/hook/policy file) is still reachable by the
/// gate: smuggling one extra line into `.githooks/pre-commit` needs no
/// truncation at all.
pub(super) fn append_redirect_targets(seg: &str) -> Vec<String> {
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
        if c == b'>' && !in_s && !in_d && bytes.get(i + 1) == Some(&b'>') {
            // Same target-token scan as `redirect_targets`: ASCII-only
            // delimiters, so `seg[start..j]` never splits a multi-byte char.
            let mut j = skip_redirect_word_gap(bytes, i + 2);
            let start = j;
            while j < bytes.len() {
                let cj = bytes[j];
                if cj.is_ascii_whitespace()
                    || cj == b';'
                    || cj == b'|'
                    || cj == b'&'
                    || cj == b'>'
                    || cj == b')'
                {
                    break;
                }
                j += 1;
            }
            if start < j {
                targets.push(seg[start..j].to_string());
            }
            i = j.max(i + 2);
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
pub(super) fn is_angle_placeholder_close(bytes: &[u8], gt: usize) -> bool {
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
