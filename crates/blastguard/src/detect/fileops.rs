use super::{
    basename, exclude, is_bare_redirect_op, is_redirect_token, is_short_flag,
    protected_disarm_deny, protected_glob_deny, protected_landing_block, protected_path_block,
    protected_tree_deny, system_path_block, Ctx, Decision,
};

/// `unlink` / `rmdir`: the single-file and empty-directory twins of `rm`.
///
/// Round 2. `unlink FILE` is `rm FILE` with a different spelling of the same
/// syscall, and `rmdir .githooks` removes the hook directory outright — neither
/// had an arm, so both reached `_ => Allow`. Ordinary targets stay allowed: a
/// non-recursive single-target delete is below this crate's destructive bar
/// (same rule `analyze_rm` applies), so only the protected-target case is a
/// verdict.
pub(super) fn analyze_unlink_rmdir(cmd: &str, rest: &[&str], ctx: &Ctx<'_>) -> Decision {
    for op in positional_operands(rest, &[]) {
        if let Some(deny) = protected_disarm_deny(ctx, cmd, op) {
            return deny;
        }
        if let Some(deny) = protected_tree_deny(ctx, cmd, op) {
            return deny;
        }
        if let Some(deny) = protected_glob_deny(&format!("{cmd} operand"), op) {
            return deny;
        }
        if let Some(deny) = home_asset_deny(ctx, cmd, op) {
            return deny;
        }
    }
    Decision::Allow
}

/// Deny for a single-entry delete (`rm` without `-r`, `unlink`, `rmdir`) whose
/// operand is an irreplaceable `$HOME`-level asset — a credential store entry
/// (`~/.ssh/id_rsa`) or a home-level dotfile (backlog 6067a527). Deletion
/// policy: system configuration and irreplaceable assets are never deleted;
/// work files stay deletable, so every other operand is `None`.
pub(super) fn home_asset_deny(ctx: &Ctx<'_>, action: &str, operand: &str) -> Option<Decision> {
    let path = ctx
        .scope
        .irreplaceable_home_asset(operand, ctx.base_for(action))?;
    Some(Decision::deny(format!(
        "{action} {operand} deletes `{path}`, an irreplaceable home-level asset (credential \
store or home-level config) that cannot be restored, so blastguard refuses"
    )))
}

/// Deny for a single-file delete whose operand IS a `.git` entry (not a path
/// below one): in a linked worktree that entry is the `gitdir:` file, and
/// removing it disconnects the work tree from its repository (backlog
/// eba15868). `None` for every other operand.
pub(super) fn dotgit_link_deny(action: &str, operand: &str) -> Option<Decision> {
    let norm = exclude::normalize(operand);
    let norm = norm.trim_end_matches('/');
    if norm == ".git" || norm.ends_with("/.git") {
        Some(Decision::deny(format!(
            "{action} {operand} removes a `.git` entry — in a linked worktree that is the \
`gitdir:` file connecting the checkout to its repository, so blastguard refuses"
        )))
    } else {
        None
    }
}

/// The OUTPUT file of a `sort` command (`-o FILE`, `-oFILE`, `--output=FILE`,
/// `--output FILE`, or a clustered `-bo FILE`), if one is present.
///
/// Round 4. `sort` reads by default and writes ONLY when told to via `-o`; the
/// output flag is the entire hazard, so it is what this locates. `o` is the only
/// short flag `sort` spells with the letter `o`, so its presence in a short-flag
/// cluster unambiguously means "output", and it consumes a value (attached after
/// the `o`, else the next token).
fn sort_output_file<'a>(rest: &[&'a str]) -> Option<&'a str> {
    let mut i = 0;
    while i < rest.len() {
        let t = rest[i];
        if let Some(v) = t.strip_prefix("--output=") {
            return Some(v);
        }
        if t == "--output" {
            return rest.get(i + 1).copied();
        }
        if is_short_flag(t) {
            if let Some(opos) = t.find('o') {
                let after = &t[opos + 1..];
                if !after.is_empty() {
                    return Some(after);
                }
                return rest.get(i + 1).copied();
            }
        }
        i += 1;
    }
    None
}

/// `sort`: Deny only the OUTPUT-onto-a-protected-path form.
///
/// Round 4 (adversarial verifier). `sort -o .githooks/pre-commit /dev/null` was
/// ALLOW because `sort` sat in `READ_ONLY_COMMANDS` and the unknown-verb rule
/// exempts that list without inspecting operands — but `sort -o FILE` writes
/// FILE in place (proven: truncated a real file to 0 bytes). A protected output
/// target is a positively recognised write hazard, exactly like a `cp`/`mv`
/// destination, so it is a Deny. Pure reads (`sort .githooks/pre-commit`, no
/// `-o`) and non-protected outputs stay Allow — `sort`'s read semantics are
/// known, so there is no need to Ask.
pub(super) fn analyze_sort(rest: &[&str], ctx: &Ctx<'_>) -> Decision {
    match sort_output_file(rest) {
        Some(out) => protected_path_block(ctx, "sort -o", out)
            .or_else(|| protected_glob_deny("sort -o", out))
            .unwrap_or(Decision::Allow),
        None => Decision::Allow,
    }
}

/// Separate-form value options of `uniq` (`-f N`, `-s N`, `-w N` and their long
/// spellings). Listed so [`positional_operands`] does not read the numeric VALUE
/// of one of these as a file operand.
const UNIQ_VALUE_FLAGS: &[&str] = &[
    "-f",
    "--skip-fields",
    "-s",
    "--skip-chars",
    "-w",
    "--check-chars",
];

/// `uniq`: Deny only the OUTPUT-operand-onto-a-protected-path form.
///
/// Round 4 (adversarial verifier). `uniq /dev/null .githooks/pre-commit` was
/// ALLOW for the same reason as `sort` — but `uniq [INPUT [OUTPUT]]` writes its
/// SECOND positional operand (proven: truncated a real file). Only the 2nd
/// operand is a write target; the 1st (INPUT) is read, so a protected INPUT
/// stays Allow and only a protected OUTPUT is a Deny.
pub(super) fn analyze_uniq(rest: &[&str], ctx: &Ctx<'_>) -> Decision {
    let operands = positional_operands(rest, UNIQ_VALUE_FLAGS);
    if let Some(out) = operands.get(1) {
        if let Some(deny) = protected_path_block(ctx, "uniq output", out)
            .or_else(|| protected_glob_deny("uniq output", out))
        {
            return deny;
        }
    }
    Decision::Allow
}

/// True when `tok` contains an unescaped shell glob metacharacter (`*`, `?`,
/// `[`) — i.e. it is a wildcard pattern that can expand to many paths, not a
/// single literal filename.
pub(super) fn has_glob_meta(tok: &str) -> bool {
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

/// True when `tok` is one of `chmod`'s OWN option flags rather than a mode.
///
/// This cannot reuse [`positional_operands`]: chmod's MODE argument routinely
/// begins with `-` (`chmod -x FILE` SUBTRACTS the exec bit), so a generic
/// "starts with `-` ⇒ flag" scan drops the mode and then reads the FILE as the
/// mode — silently moving the real target out of view, which is the fail-open
/// shape this whole class of rules exists to prevent. The option set is
/// therefore spelled out and everything else is treated as data.
///
/// Every letter listed is one chmod does NOT accept inside a mode (mode letters
/// are `rwxXst` and `ugoa`), so no mode can be mistaken for an option: `-x`,
/// `-w`, `-rwx`, `-X`, `-s`, `-t` all contain a mode letter and fall through.
fn is_chmod_option(tok: &str) -> bool {
    const LONG: &[&str] = &[
        "--recursive",
        "--changes",
        "--silent",
        "--quiet",
        "--verbose",
        "--preserve-root",
        "--no-preserve-root",
        "--dereference",
        "--no-dereference",
        "--help",
        "--version",
    ];
    if LONG.contains(&tok) || tok.starts_with("--reference=") {
        return true;
    }
    is_short_flag(tok)
        && tok.len() > 1
        && tok[1..]
            .chars()
            .all(|c| matches!(c, 'R' | 'f' | 'v' | 'c' | 'h' | 'H' | 'L' | 'P'))
}

/// Split a `chmod` argument list into its MODE and its file operands.
///
/// Returns `None` for the mode when none could be located; callers must treat
/// that as "unparseable", not as "harmless" (see the `chmod` arm).
pub(super) fn chmod_mode_and_targets<'a>(rest: &[&'a str]) -> (Option<&'a str>, Vec<&'a str>) {
    spec_and_targets(rest, is_chmod_option)
}

/// Split `chown`/`chgrp` arguments into the OWNER spec and the file operands.
pub(super) fn chown_owner_and_targets<'a>(rest: &[&'a str]) -> (Option<&'a str>, Vec<&'a str>) {
    spec_and_targets(rest, is_chown_option)
}

/// Split `rest` into the leading spec operand (a chmod MODE, a chown OWNER) and
/// the file operands, skipping option flags and redirect punctuation.
///
/// `--reference=FILE` takes the spec from another file, so the command has NO
/// spec operand and the first non-option token is already a TARGET. Consuming it
/// as the spec anyway made the target invisible to every protected-path check
/// downstream — measured 2026-09-07 on the deployed rules:
/// `chmod --reference=/tmp/x .githooks/pre-commit` was `Allow`, while the same
/// command spelled `chmod 000 .githooks/pre-commit` was a Deny. One off-by-one,
/// both verbs, the entire disarm class walked through it.
fn spec_and_targets<'a>(
    rest: &[&'a str],
    is_option: fn(&str) -> bool,
) -> (Option<&'a str>, Vec<&'a str>) {
    let mut spec: Option<&'a str> = None;
    // `--reference=` already supplied the spec, so no operand is one.
    let mut spec_taken = rest.iter().any(|t| t.starts_with("--reference="));
    // Tracked separately from `spec_taken`: with `--reference=` the spec is
    // taken before the scan starts, and option flags must still be skipped
    // rather than collected as file operands.
    let mut seen_positional = false;
    let mut targets: Vec<&'a str> = Vec::new();
    let mut end_of_options = false;
    let mut i = 0;
    while i < rest.len() {
        let t = rest[i];
        if t.is_empty() {
            i += 1;
        } else if t == "--" {
            end_of_options = true;
            i += 1;
        } else if is_bare_redirect_op(t) {
            // Operator plus the target token it owns (mirrors
            // `positional_operands`, so "what counts as an operand" stays one
            // definition).
            i += 2;
        } else if is_redirect_token(t) || (!end_of_options && !seen_positional && is_option(t)) {
            // Redirect punctuation the shell consumes, or one of the command's
            // own option flags: neither is a spec and neither is a file operand.
            i += 1;
        } else {
            seen_positional = true;
            if spec_taken {
                targets.push(t);
            } else {
                spec = Some(t);
                spec_taken = true;
            }
            i += 1;
        }
    }
    (spec, targets)
}

/// `chown`/`chgrp` accept every flag [`is_chmod_option`] lists, plus `--from=`
/// (chown only). Kept as a superset rather than a separate table so a flag added
/// to one verb cannot silently become a positional operand on the other.
fn is_chown_option(tok: &str) -> bool {
    tok.starts_with("--from=") || is_chmod_option(tok)
}

/// True when `mode` takes executability AWAY from the file's owner.
///
/// The predicate is about the MODE, deliberately, not about "a chmod touched a
/// protected path": re-asserting the exec bit is how a hook gets INSTALLED
/// (`chmod +x .githooks/pre-commit`, `chmod 755`), and denying that would block
/// the legitimate direction of the very same command.
///
/// Only the OWNER's execute bit is consulted for octal modes: a git hook or a
/// `.claude/hooks/*` script is run by the user who owns the checkout, so
/// `chmod 700` leaves it executable while `644`/`444`/`000` do not.
///
/// A mode this function cannot parse returns `true` — the restrictive side per
/// CLAUDE.md §3. The result is only ever consulted for a target already known to
/// be protected, so the price of that default is a false positive on an exotic
/// mode spelling aimed at a gate file; the price of the permissive default would
/// be a silent disarm.
pub(super) fn mode_removes_exec(mode: &str) -> bool {
    if !mode.is_empty() && mode.chars().all(|c| c.is_ascii_digit()) {
        let digits: Vec<u32> = mode.chars().filter_map(|c| c.to_digit(8)).collect();
        // An `8` or `9` is not octal at all — unparseable, so restrictive.
        if digits.len() != mode.len() {
            return true;
        }
        return match digits.len() {
            // `755` / `0755`: the owner triad is the third digit from the end.
            3 | 4 => digits[digits.len() - 3] & 1 == 0,
            _ => true,
        };
    }
    // Symbolic, comma-separated clauses: `u-x`, `a-x`, `go-w,u+x`, `a=r`.
    let mut removes = false;
    for clause in mode.split(',') {
        let Some(op_pos) = clause.rfind(['+', '-', '=']) else {
            // No operator: not a symbolic mode we understand.
            return true;
        };
        let op = clause.as_bytes()[op_pos];
        let perms = &clause[op_pos + 1..];
        let grants_exec = perms.contains('x') || perms.contains('X') || perms.contains('s');
        match op {
            // `-x`, `a-x`, `u-x`: subtracting the exec bit.
            b'-' if grants_exec => removes = true,
            // `a=r`: `=` replaces the whole set, so a set without `x` clears it.
            b'=' if !grants_exec => removes = true,
            b'+' | b'-' | b'=' => {}
            _ => return true,
        }
    }
    removes
}

/// True when `mode` takes the READ bit AWAY from the file's owner.
///
/// Round 4 (adversarial verifier). The twin of [`mode_removes_exec`]: a shell
/// script hook is executed via its shebang, so the interpreter must READ the
/// file — clearing read disarms the hook exactly as clearing exec does.
/// `chmod -r hook.sh` (mode `-wx--x--x`) makes `./hook.sh` fail exit 126 on this
/// host despite the exec bit still being set, and `chmod 311 hook.sh` (owner
/// triad `3` = `-wx`, read bit 4 clear) is the octal spelling of the same.
///
/// The structure mirrors [`mode_removes_exec`] so the two stay in lockstep: the
/// OWNER read bit (value 4) for octal modes, and a symbolic clause that
/// subtracts `r` or a `=` replacement that omits it. A mode this function cannot
/// parse returns `true` — the restrictive side per CLAUDE.md §3, and only ever
/// consulted for a target already known to be protected.
pub(super) fn mode_removes_read(mode: &str) -> bool {
    if !mode.is_empty() && mode.chars().all(|c| c.is_ascii_digit()) {
        let digits: Vec<u32> = mode.chars().filter_map(|c| c.to_digit(8)).collect();
        // An `8`/`9` is not octal at all — unparseable, so restrictive.
        if digits.len() != mode.len() {
            return true;
        }
        return match digits.len() {
            // `755` / `0755`: the owner triad is the third digit from the end;
            // the read bit is value 4.
            3 | 4 => digits[digits.len() - 3] & 4 == 0,
            _ => true,
        };
    }
    // Symbolic, comma-separated clauses.
    let mut removes = false;
    for clause in mode.split(',') {
        let Some(op_pos) = clause.rfind(['+', '-', '=']) else {
            return true;
        };
        let op = clause.as_bytes()[op_pos];
        let perms = &clause[op_pos + 1..];
        let grants_read = perms.contains('r');
        match op {
            // `-r`, `a-r`, `u-r`: subtracting the read bit.
            b'-' if grants_read => removes = true,
            // `a=x`: `=` replaces the whole set, so a set without `r` clears it.
            b'=' if !grants_read => removes = true,
            b'+' | b'-' | b'=' => {}
            _ => return true,
        }
    }
    removes
}

/// Positional operands of a simple command: tokens that are neither the
/// command's own flags, nor shell redirection punctuation, nor the separate-form
/// VALUE of one of `value_flags`.
///
/// The redirect skipping mirrors [`has_operand`] exactly, so "what counts as a
/// target" stays one definition. `value_flags` must list ONLY options that
/// really take a separate value: a wrong entry swallows the following token, and
/// if that token was the destination the check silently sees nothing — the
/// fail-open shape this whole class of rules exists to prevent.
pub(super) fn positional_operands<'a>(rest: &[&'a str], value_flags: &[&str]) -> Vec<&'a str> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < rest.len() {
        let t = rest[i];
        if t.is_empty() {
            i += 1;
        } else if is_bare_redirect_op(t) {
            // Operator plus the target token it owns.
            i += 2;
        } else if is_redirect_token(t) {
            i += 1;
        } else if value_flags.contains(&t) {
            i += 2;
        } else if t.starts_with('-') && t.len() > 1 {
            i += 1;
        } else {
            out.push(t);
            i += 1;
        }
    }
    out
}

/// Separate-form value options of `cp`/`mv`/`install`/`ln`. Deliberately short:
/// only options that genuinely consume the next token (see
/// [`positional_operands`]). `-Z`/`--context` are omitted because their value is
/// optional and `=`-attached — listing them would eat an operand.
pub(super) const COPY_VALUE_FLAGS: &[&str] = &[
    "-t",
    "--target-directory",
    "-S",
    "--suffix",
    "-m",
    "--mode",
    "-o",
    "--owner",
    "-g",
    "--group",
    "--strip-program",
];

/// How a long option of `cp`/`mv`/`install`/`ln` takes its value (getopt's
/// `no_argument` / `required_argument` / `optional_argument`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum LongArg {
    No,
    /// `--opt=V` or `--opt V`.
    Required,
    /// `--opt=V` only; a following word is NOT its value.
    Optional,
}

/// Per-verb option tables: (value-taking short letters, boolean short
/// letters, long options). GNU coreutils and BSD/macOS letters are unioned; a
/// letter whose arity DIFFERS between the two (`install -S`: GNU suffix value,
/// BSD boolean) is in neither set, so it is unparseable (Ask).
fn copy_option_table(
    cmd: &str,
) -> (
    &'static str,
    &'static str,
    &'static [(&'static str, LongArg)],
) {
    use LongArg::{No, Optional, Required};
    match cmd {
        "cp" => (
            "St",
            "abcdfHilLnNPpRrsTuvXxZ",
            &[
                ("--archive", No),
                ("--attributes-only", No),
                ("--backup", Optional),
                ("--copy-contents", No),
                ("--debug", No),
                ("--dereference", No),
                ("--force", No),
                ("--interactive", No),
                ("--link", No),
                ("--no-dereference", No),
                ("--no-clobber", No),
                ("--preserve", Optional),
                ("--no-preserve", Required),
                ("--parents", No),
                ("--recursive", No),
                ("--reflink", Optional),
                ("--remove-destination", No),
                ("--sparse", Required),
                ("--strip-trailing-slashes", No),
                ("--symbolic-link", No),
                ("--suffix", Required),
                ("--target-directory", Required),
                ("--no-target-directory", No),
                ("--update", Optional),
                ("--verbose", No),
                ("--keep-directory-symlink", No),
                ("--one-file-system", No),
                ("--context", Optional),
                ("--help", No),
                ("--version", No),
            ],
        ),
        "mv" => (
            "St",
            "bfhinTuvZ",
            &[
                ("--backup", Optional),
                ("--debug", No),
                ("--exchange", No),
                ("--force", No),
                ("--interactive", No),
                ("--no-clobber", No),
                ("--no-copy", No),
                ("--strip-trailing-slashes", No),
                ("--suffix", Required),
                ("--target-directory", Required),
                ("--no-target-directory", No),
                ("--update", Optional),
                ("--verbose", No),
                ("--context", No),
                ("--help", No),
                ("--version", No),
            ],
        ),
        "install" => (
            "BfgmoNt",
            "bcCdDMpPsTUvZ",
            &[
                ("--backup", Optional),
                ("--compare", No),
                ("--debug", No),
                ("--directory", No),
                ("--group", Required),
                ("--mode", Required),
                ("--owner", Required),
                ("--preserve-timestamps", No),
                ("--strip", No),
                ("--strip-program", Required),
                ("--suffix", Required),
                ("--target-directory", Required),
                ("--no-target-directory", No),
                ("--verbose", No),
                ("--preserve-context", No),
                ("--context", Optional),
                ("--help", No),
                ("--version", No),
            ],
        ),
        // ln
        _ => (
            "St",
            "bdFfhiLnPrsTvw",
            &[
                ("--backup", Optional),
                ("--directory", No),
                ("--force", No),
                ("--interactive", No),
                ("--logical", No),
                ("--no-dereference", No),
                ("--physical", No),
                ("--relative", No),
                ("--symbolic", No),
                ("--suffix", Required),
                ("--target-directory", Required),
                ("--no-target-directory", No),
                ("--verbose", No),
                ("--help", No),
                ("--version", No),
            ],
        ),
    }
}

/// The argv of a `cp`/`mv`/`install`/`ln` line as getopt would split it.
struct CopyArgs<'a> {
    /// Positional operands, in order.
    operands: Vec<&'a str>,
    /// The `-t DIR` / `--target-directory DIR` value, in any spelling: bare,
    /// glued (`-tDIR`), inside a short cluster (`-rt DIR`, `-rtDIR`), `=`-joined
    /// or abbreviated (`--target DIR`).
    target_dir: Option<String>,
}

/// Parse `rest` the way getopt does for `cmd` (backlog ef7c9fbe rework: the
/// old word heuristic read `cp -rt DIR SRC` with `DIR` as a SOURCE, because
/// only a bare `-t` / a leading `-tX` was recognised).
///
/// Short clusters are scanned letter by letter: a value letter takes the rest
/// of the cluster, else the next word. Long options resolve by exact name or
/// unique prefix. `Err(why)` for anything this cannot split with certainty —
/// an unknown or ambiguous option, a missing value, a value on a no-argument
/// long option, a second target directory — and the caller must not judge on
/// a guessed split.
fn parse_copy_args<'a>(cmd: &str, rest: &[&'a str]) -> Result<CopyArgs<'a>, String> {
    let (value_short, bool_short, longs) = copy_option_table(cmd);
    let mut operands = Vec::new();
    let mut target_dir: Option<String> = None;
    let mut set_target = |v: String| -> Result<(), String> {
        if target_dir.replace(v).is_some() {
            return Err(format!("`{cmd}` is given more than one target directory"));
        }
        Ok(())
    };
    let mut opts_done = false;
    let mut i = 0;
    while i < rest.len() {
        let t = rest[i];
        i += 1;
        if t.is_empty() {
            continue;
        }
        if is_bare_redirect_op(t) {
            i += 1;
            continue;
        }
        if is_redirect_token(t) {
            continue;
        }
        if opts_done || !t.starts_with('-') || t == "-" {
            operands.push(t);
            continue;
        }
        if t == "--" {
            opts_done = true;
            continue;
        }
        if t.starts_with("--") {
            let (name, glued) = match t.split_once('=') {
                Some((n, v)) => (n, Some(v)),
                None => (t, None),
            };
            let resolved = match longs.iter().find(|(l, _)| *l == name) {
                Some(hit) => *hit,
                None => {
                    let hits: Vec<_> = longs.iter().filter(|(l, _)| l.starts_with(name)).collect();
                    match hits.as_slice() {
                        [one] => **one,
                        [] => return Err(format!("unrecognised `{cmd}` option `{name}`")),
                        _ => return Err(format!("ambiguous `{cmd}` option `{name}`")),
                    }
                }
            };
            let value = match (resolved.1, glued) {
                (LongArg::No, Some(_)) => {
                    return Err(format!("`{cmd}` option `{}` takes no value", resolved.0))
                }
                (LongArg::No | LongArg::Optional, None) => None,
                (_, Some(v)) => Some(v.to_string()),
                (LongArg::Required, None) => match rest.get(i) {
                    Some(v) => {
                        i += 1;
                        Some((*v).to_string())
                    }
                    None => return Err(format!("`{cmd}` option `{}` has no value", resolved.0)),
                },
            };
            if resolved.0 == "--target-directory" {
                set_target(value.unwrap_or_default())?;
            }
            continue;
        }
        // Short cluster.
        let cluster = &t[1..];
        for (pos, c) in cluster.char_indices() {
            if value_short.contains(c) {
                let tail = &cluster[pos + c.len_utf8()..];
                let value = if tail.is_empty() {
                    match rest.get(i) {
                        Some(v) => {
                            i += 1;
                            (*v).to_string()
                        }
                        None => return Err(format!("`{cmd} -{c}` has no value")),
                    }
                } else {
                    tail.to_string()
                };
                if c == 't' {
                    set_target(value)?;
                }
                break;
            }
            if !bool_short.contains(c) {
                return Err(format!("unrecognised `{cmd}` option `-{c}`"));
            }
        }
    }
    Ok(CopyArgs {
        operands,
        target_dir,
    })
}

/// The legacy word heuristic (kept ONLY as the fallback when
/// [`parse_copy_args`] cannot split the line, so a Deny it finds still stands).
fn legacy_copy_args<'a>(rest: &[&'a str]) -> CopyArgs<'a> {
    CopyArgs {
        operands: positional_operands(rest, COPY_VALUE_FLAGS),
        target_dir: target_directory(rest),
    }
}

/// The `-t DIR` / `--target-directory=DIR` destination directory, if given.
/// Legacy heuristic: only a bare `-t` or a leading `-tX` (see
/// [`legacy_copy_args`]); [`parse_copy_args`] is the real parser.
fn target_directory(rest: &[&str]) -> Option<String> {
    let mut i = 0;
    while i < rest.len() {
        let t = rest[i];
        if t == "-t" || t == "--target-directory" {
            return rest.get(i + 1).map(|v| (*v).to_string());
        }
        if let Some(v) = t.strip_prefix("--target-directory=") {
            return Some(v.to_string());
        }
        if let Some(v) = t.strip_prefix("-t") {
            if !v.is_empty() && !v.starts_with('-') {
                return Some(v.to_string());
            }
        }
        i += 1;
    }
    None
}

/// `cp` / `mv` / `install` / `ln`: the DESTINATION is what gets overwritten —
/// and, for `mv` alone, the SOURCE is what disappears.
///
/// The source/destination asymmetry is the whole design of this function and is
/// modelled explicitly rather than assumed:
///
///   * `cp`, `install`, `ln` READ their sources and leave them in place, so a
///     protected source is a backup, not a hazard — `cp .claude/settings.json
///     /tmp/settings.backup.json` must stay Allow, and denying it would be a
///     false positive on ordinary work;
///   * `mv` UNLINKS its sources. Moving `.githooks/pre-commit` to `/tmp`, or
///     renaming `.claude/settings.json` to `settings.json.disabled` in the same
///     directory, disarms the gate exactly as thoroughly as deleting it — and
///     neither is visible to a destination-only rule, because the DESTINATION
///     in both cases is an unprotected path.
///
/// Three destination shapes are handled, because a rule that only understood
/// the two-operand form would leave the other two as free bypasses:
///   * `cp SRC DEST` — the last operand is the destination;
///   * `cp -t DIR SRC…` — every operand is a source and the file created is
///     `DIR/<basename SRC>`;
///   * `cp SRC… DIR/` — same, with the directory written last.
///
/// A RECURSIVE copy is handled separately from all three: its landing set is a
/// whole tree whose shape lives on disk, so the per-file `DIR/<basename SRC>`
/// model does not describe it (and for a `SRC/` with a trailing slash the model
/// collapses to `DIR/` itself, which matches nothing). See
/// [`protected_landing_block`].
///
/// Every destination shape above is ALSO judged on the system-directory axis
/// ([`system_path_block`]), after the protected-path axis and in the same order
/// the redirect rule uses. This function used to consult only the protected
/// axis, so `cp evil /etc/paths.d/zz`, `install -m755 evil /usr/local/bin/x`
/// and `mv evil /Library/LaunchDaemons/e.plist` were ALLOW while the redirect
/// into the very same path was DENY: the same file created, a different
/// verdict depending only on the verb (backlog d5613105).
pub(super) fn analyze_copy_move(cmd: &str, rest: &[&str], ctx: &Ctx<'_>) -> Decision {
    let args = match parse_copy_args(cmd, rest) {
        Ok(a) => a,
        // The split is unknown, so no verdict below may be trusted to have
        // judged the real destination. A Deny the legacy heuristic still finds
        // stands (never weaker than before); anything else is Ask.
        Err(why) => {
            let legacy = copy_move_verdict(cmd, rest, ctx, &legacy_copy_args(rest));
            if legacy.is_deny() {
                return legacy;
            }
            return Decision::ask(format!(
                "blastguard cannot determine the operands of this `{cmd}` ({why}), so it cannot \
tell which path it writes (or, for mv, removes) and refuses to guess"
            ));
        }
    };
    let verdict = copy_move_verdict(cmd, rest, ctx, &args);
    // Backlog ef7c9fbe: a recursive `cp` creates a whole tree; outside the
    // managed places an otherwise-Allowed one is raised to Ask (see
    // `scratch_place`). A Deny/Ask from the rules above always stands.
    if cmd == "cp" && is_recursive_copy(rest) {
        return super::scratch_place::cp_recursive_verdict(
            ctx,
            verdict,
            args.target_dir,
            &args.operands,
        );
    }
    verdict
}

/// The protected-path / system-directory verdict of [`analyze_copy_move`],
/// before the scratch-place rule, on an already-split argv.
fn copy_move_verdict(cmd: &str, rest: &[&str], ctx: &Ctx<'_>, args: &CopyArgs<'_>) -> Decision {
    let action = format!("{cmd} destination");
    let operands = args.operands.clone();

    // Directory-destination forms: resolve the file each source would create.
    let (dir, sources): (Option<String>, &[&str]) = match args.target_dir.clone() {
        Some(d) => (Some(d), &operands[..]),
        None => match operands.split_last() {
            Some((last, init)) if last.ends_with('/') && !init.is_empty() => {
                (Some((*last).to_string()), init)
            }
            _ => (None, &[]),
        },
    };

    // ---- source side: only `mv` removes what it names ----
    if cmd == "mv" {
        let moved: &[&str] = if dir.is_some() {
            sources
        } else {
            // `mv SRC… DEST`: everything but the last operand is a source.
            operands.split_last().map(|(_, init)| init).unwrap_or(&[])
        };
        for src in moved {
            if let Some(deny) = protected_disarm_deny(ctx, "mv source", src) {
                return deny;
            }
            // Moving the CONTAINER away takes every protected file in it.
            if let Some(deny) = protected_tree_deny(ctx, "mv source", src) {
                return deny;
            }
            // Round 2: `mv .claude/* /tmp/` was ALLOW. The two checks above are
            // handed a token containing `*`, which cannot match a literal path
            // glob, and unlike `analyze_rm` this loop has no wildcard backstop
            // underneath it. See `protected_glob_deny`.
            if let Some(deny) = protected_glob_deny("mv source", src) {
                return deny;
            }
            // Round 2: `find … -exec mv {} /tmp/ \;` re-analyses to the tail
            // `mv {} /tmp/`, whose source operand is the literal placeholder
            // `{}` — a path known only to `find` at run time. Matching it
            // against the protected globs is guaranteed to miss, so the
            // placeholder has to be recognised as what it is: an operand
            // blastguard cannot resolve, in a position that unlinks whatever it
            // resolves to. Ask, per CLAUDE.md §3.
            if src.contains("{}") {
                return Decision::ask(format!(
                    "mv source {src} is a find -exec placeholder standing for every matched \
path — blastguard cannot tell what it expands to, and mv unlinks it, so it refuses to guess"
                ));
            }
        }
    }

    // ---- destination side ----
    // A recursive copy lands an unmodelled set of files; ask about the landing
    // DIRECTORY before falling back to the per-file model below, which cannot
    // describe it.
    if is_recursive_copy(rest) {
        let landing = dir
            .clone()
            // `cp -r SRC DEST` without a trailing slash on DEST: with `-r` the
            // last operand is still a destination directory, so the trailing
            // slash must not be what decides whether the rule fires.
            .or_else(|| match operands.split_last() {
                Some((last, init)) if !init.is_empty() => Some((*last).to_string()),
                _ => None,
            });
        if let Some(landing) = landing {
            if let Some(block) = protected_landing_block(ctx, &action, &landing) {
                return block;
            }
            if let Some(deny) = system_path_block(&action, &landing, ctx) {
                return deny;
            }
        }
    }

    if let Some(dir) = dir {
        if let Some(deny) = protected_path_block(ctx, &action, &dir) {
            return deny;
        }
        if let Some(deny) = protected_glob_deny(&action, &dir) {
            return deny;
        }
        let base = dir.trim_end_matches('/');
        for src in sources {
            let landed = format!("{base}/{}", basename(src));
            if let Some(deny) = protected_path_block(ctx, &action, &landed) {
                return deny;
            }
        }
        // SYSTEM-DIRECTORY axis, after the protected one (the redirect rule's
        // order). The directory itself is judged, and so is each file landed
        // in it, so `/usr/local/bin/` and `/usr/local/bin/x` cannot disagree.
        if let Some(deny) = system_path_block(&action, &dir, ctx) {
            return deny;
        }
        for src in sources {
            let landed = format!("{base}/{}", basename(src));
            if let Some(deny) = system_path_block(&action, &landed, ctx) {
                return deny;
            }
        }
        return Decision::Allow;
    }

    match operands.last() {
        Some(dest) => protected_path_block(ctx, &action, dest)
            .or_else(|| protected_glob_deny(&action, dest))
            .or_else(|| system_path_block(&action, dest, ctx))
            .unwrap_or(Decision::Allow),
        // One operand or none: nothing is being written over (`cp a` is an
        // error, `mv -t DIR` with no source does nothing).
        None => Decision::Allow,
    }
}

/// True when a `cp`/`install` line copies whole directory TREES (`-r`, `-R`,
/// `-a`/`--archive`, `--recursive`, or any cluster containing `r`/`R`/`a`).
///
/// `mv` and `ln` have no recursive mode, so the predicate simply never fires for
/// them; there is no need to special-case the command name.
fn is_recursive_copy(rest: &[&str]) -> bool {
    rest.iter().any(|t| {
        *t == "--recursive"
            || *t == "--archive"
            || (is_short_flag(t) && (t.contains('r') || t.contains('R') || t.contains('a')))
    })
}

/// Separate-form value options of `sed`. `-e`/`-f` supply the SCRIPT, so their
/// value is not a file sed writes to.
const SED_VALUE_FLAGS: &[&str] = &["-e", "--expression", "-f", "--file", "-l", "--line-length"];

/// `sed -i` rewrites its operands IN PLACE — a full-content overwrite of an
/// existing file, the same hazard as `>` or `tee`. Without `-i`, sed only reads
/// and writes to stdout, so it stays Allow.
///
/// EVERY operand is checked, including the leading script in the
/// `sed -i SCRIPT FILE` form. Checking the script too can only add candidates
/// that fail the protected match anyway (`s/a/b/` is not a path), whereas
/// getting the script/file split wrong in the other direction — BSD's
/// `sed -i '' SCRIPT FILE` takes a separate suffix argument, GNU's `-i.bak`
/// attaches it — would drop a real target and fail open.
pub(super) fn analyze_sed(rest: &[&str], ctx: &Ctx<'_>) -> Decision {
    let in_place = rest.iter().any(|t| {
        *t == "--in-place"
            || t.starts_with("--in-place=")
            // `-i`, `-i.bak` (attached suffix) and clusters like `-ni`.
            || (is_short_flag(t) && t.contains('i'))
    });
    if !in_place {
        return Decision::Allow;
    }
    for operand in positional_operands(rest, SED_VALUE_FLAGS) {
        if let Some(deny) = protected_path_block(ctx, "sed -i target", operand) {
            return deny;
        }
    }
    Decision::Allow
}
