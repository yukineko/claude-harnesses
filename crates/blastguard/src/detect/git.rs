use super::{
    confined_ask, has_short, irreversible_git_subcommand, is_short_flag, normalized_command,
    positional_operands, protected_disarm_deny, protected_glob_deny, protected_tree_deny, Ctx,
    Decision, Determination, VerdictAcc,
};

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
pub(super) fn git_subcommand_index(rest: &[&str]) -> Option<usize> {
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

/// The shared reason for every way of repointing `core.hooksPath`. One wording
/// so [`crate::rule_id`] gives the whole class one signature.
///
/// The second clause is there for the one-argument READ, `git config
/// core.hooksPath`, which the `config` arm denies on purpose (see there). Every
/// field occurrence of this rule measured on 2026-09-24 (backlog 326edfee) was
/// that read, retried until it tripped the repeat signal. The verdict stays the
/// same. The reason now names the spelling that is actually allowed, so the
/// caller can read the value without asking.
const HOOKSPATH_REASON: &str =
    "git config core.hooksPath repoints every git hook at once, disabling the repo's hook gates \
     — to only READ the current value, use `git config --get core.hooksPath` \
     (the bare one-argument form is refused because it is not told apart from a write)";

/// Config assignments carried by git's GLOBAL options, in every spelling git
/// accepts: `-c k=v`, `-ck=v` (glued), `--config-env k=v`, `--config-env=k=v`.
///
/// `git_subcommand_index` already knew these options exist — it skips past them
/// to find the subcommand — but nothing looked at the value it skipped. That is
/// how `git -c core.hooksPath=/tmp/evil status` reached Allow while
/// `git config core.hooksPath …` was denied: the same setting, applied for the
/// duration of one command instead of written to a file, through a code path
/// whose only job was to ignore it.
fn git_global_config_assignments<'a>(globals: &[&'a str]) -> Vec<&'a str> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < globals.len() {
        let t = globals[i];
        if t == "-c" || t == "--config-env" {
            if let Some(v) = globals.get(i + 1) {
                out.push(*v);
            }
            i += 2;
            continue;
        }
        if let Some(v) = t.strip_prefix("--config-env=") {
            out.push(v);
        } else if let Some(v) = t.strip_prefix("-c") {
            // Glued short form `-ccore.hooksPath=x`. `-C`/`--config-env=` are
            // not reachable here (`strip_prefix` is case-sensitive and the
            // long form was handled above).
            if !v.is_empty() {
                out.push(v);
            }
        }
        i += 1;
    }
    out
}

/// True when any global `-c`/`--config-env` assignment sets `core.hooksPath`.
/// Git config keys are case-insensitive, so the key is folded before comparing.
fn sets_hookspath_inline(globals: &[&str]) -> bool {
    git_global_config_assignments(globals).iter().any(|a| {
        a.split('=')
            .next()
            .unwrap_or("")
            .trim()
            .eq_ignore_ascii_case("core.hookspath")
    })
}

/// One pathspec of `git checkout [<tree-ish>] -- <pathspec>`: Allow only when
/// git reports nothing at or below it that the overwrite would destroy; Deny
/// when it reports uncommitted changes; Ask whenever that cannot be observed.
fn checkout_pathspec_verdict(ctx: &Ctx<'_>, op: &str) -> Decision {
    let undetermined = |why: &str| {
        Decision::ask(format!(
            "git checkout -- `{op}` overwrites the file from git and would destroy any \
uncommitted changes in it, and blastguard cannot tell whether it has any ({why})"
        ))
    };
    let lexical = match ctx.scope.lexical_absolute(op, ctx.base_for("git")) {
        Determination::Known(l) => l,
        Determination::Undetermined(_) => return undetermined("not a placeable literal path"),
    };
    let Some(real) = ctx.scope.resolve(&lexical) else {
        return undetermined("the path could not be resolved");
    };
    let Some(probe) = ctx.scope.git_tree_probe() else {
        return undetermined("no git probe in this session model");
    };
    let facts = match probe(&real, &crate::deletion::build_candidates(&real)) {
        Determination::Known(f) => f,
        Determination::Undetermined(_) => return undetermined("git could not be asked"),
    };
    if facts.hidden_index_state {
        return undetermined("an index entry is assume-unchanged / skip-worktree / unmerged");
    }
    if facts.status.iter().all(|(xy, _)| xy == "!!") {
        return Decision::Allow;
    }
    let examples: Vec<String> = facts
        .status
        .iter()
        .filter(|(xy, _)| xy != "!!")
        .take(3)
        .map(|(xy, p)| format!("`{p}` ({xy})"))
        .collect();
    Decision::deny(format!(
        "git checkout -- `{op}` would destroy uncommitted changes (e.g. {}). Commit or stash \
them first.",
        examples.join(", ")
    ))
}

pub(super) fn analyze_git(rest: &[&str], ctx: &Ctx<'_>) -> Decision {
    let idx = match git_subcommand_index(rest) {
        Some(i) => i,
        // No subcommand: still inspect the globals. A bare
        // `git -c core.hooksPath=…` does nothing on its own, but "there is no
        // subcommand to classify" is not a reason to stop looking at what the
        // line does set.
        None => {
            return if sets_hookspath_inline(rest) {
                Decision::deny(HOOKSPATH_REASON)
            } else {
                Decision::Allow
            }
        }
    };
    // Global options sit BEFORE the subcommand; `-c` there applies the setting
    // to whatever subcommand follows, so it is checked for every git command,
    // not just `git config`.
    if sets_hookspath_inline(&rest[..idx]) {
        return Decision::deny(HOOKSPATH_REASON);
    }
    let sub = normalized_command(rest[idx]);
    // Object-store destruction, checked before the per-subcommand arms below.
    //
    // These matter more here than anywhere else in this file: the recoverability
    // rule in `crate::reversible` decides that a tracked, clean file is safe to
    // overwrite BECAUSE git holds a copy. These three commands are the ones
    // that throw that copy away. Leaving them at Allow — which is where they
    // were, measured 2026-09-09 against 0.2.58 — meant the gate would silently
    // dismantle the very thing its other verdicts rely on.
    if let Some(reason) = irreversible_git_subcommand(&sub, &rest[idx + 1..]) {
        return Decision::deny(reason);
    }
    match sub.as_str() {
        "clean" => {
            let has_f = has_short(rest, 'f') || rest.contains(&"--force");
            let has_d = has_short(rest, 'd');
            let has_x = has_short(rest, 'x');
            // `git clean` never touches a tracked file or `.git` itself, so
            // unlike `rm` it may accept a safe root as its own target — which
            // is the common form, since the pathspec-less `git clean -fdx`
            // means "this repository".
            let pathspec = positional_operands(&rest[idx + 1..], &["-e", "--exclude"]);
            let clean_targets: Vec<&str> = if pathspec.is_empty() {
                vec!["."]
            } else {
                pathspec
            };
            let clean_confined = ctx.confined_root_or_root_itself("git", &clean_targets);
            if has_f && (has_d || has_x) {
                match &clean_confined {
                    Some(root) => confined_ask("git clean -f with -d/-x", root, &clean_targets),
                    None => {
                        Decision::deny("git clean -f with -d/-x deletes untracked files & dirs")
                    }
                }
            } else if has_f {
                // Round 2: the `&& (has_d || has_x)` conjunction read as if `-d`
                // were what made the command destructive. It is not — it only
                // widens the delete to DIRECTORIES. Plain `git clean -f` already
                // deletes every untracked FILE under the pathspec, irreversibly
                // (they are untracked, so git has no copy). `-n`/`--dry-run`
                // carries no `f` and is unaffected.
                match &clean_confined {
                    Some(root) => confined_ask("git clean -f", root, &clean_targets),
                    None => Decision::deny("git clean -f deletes untracked files irreversibly"),
                }
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
            // Round 3, MIRROR GAP. The `restore` arm below denies
            // `git restore <path>` UNCONDITIONALLY, because it overwrites the
            // working tree from the index and throws away uncommitted work.
            // `git checkout -- <path>` is that same operation under its older
            // spelling — git's own docs describe `restore` as the split-out
            // half of `checkout` — yet this arm only caught `--force` and the
            // whole-tree `-- .` form. Fixing one spelling and leaving its twin
            // open is the recurring failure this crate keeps re-learning, so
            // the two arms are brought into agreement rather than patched with
            // one more special case.
            //
            // Target classification runs FIRST so a protected path gets the
            // disarm reason (and its own rule id) rather than the generic one.
            for op in positional_operands(&rest[idx + 1..], &[]) {
                if let Some(deny) = protected_disarm_deny(ctx, "git checkout", op) {
                    return deny;
                }
                if let Some(deny) = protected_tree_deny(ctx, "git checkout", op) {
                    return deny;
                }
            }
            if let Some(pos) = rest.iter().position(|t| *t == "--") {
                if rest[pos + 1..].iter().any(|t| *t == "." || *t == "./") {
                    return Decision::deny("git checkout -- . discards all working-tree changes");
                }
            }
            // Over-block narrowing (round 2 follow-up). The earlier round denied
            // `git checkout -- <ANY path>` unconditionally, to mirror the
            // `restore` arm. But `git restore <path>` blanket-denying an
            // *ordinary* tracked file is itself a pre-existing over-block
            // (backlog 628b6594), and "discard my uncommitted edits to one file"
            // is one of the most common git operations there is — blanket-denying
            // it is the kind of friction that gets a gate switched off, a worse
            // outcome than the narrow hole. Protected pathspecs are already
            // denied by the target-classification loop above (with the disarm
            // reason + rule id); the whole-tree `-- .` form is caught just above.
            // Everything else — `git checkout -- src/main.rs`, `-- Cargo.toml`,
            // `git checkout HEAD~1 -- src/main.rs` — falls through to Allow, its
            // pre-round-1 behaviour. The two mirrors are brought into agreement
            // on the PROTECTED condition, not by unifying upward to blanket deny.
            // Round 2: the whole-tree rule above is gated on finding a literal
            // `--`, so the separator-less spelling escaped it — `git checkout .`
            // and `git checkout HEAD .` were ALLOW while `git checkout -- .` was
            // DENY, for the same operation. Only `.`/`./` is matched here: a
            // pathspec and a branch name are indistinguishable without the
            // separator (`git checkout main` must stay Allow), but `.` is never
            // a branch name.
            for op in positional_operands(&rest[idx + 1..], &[]) {
                if op == "." || op == "./" {
                    return Decision::deny(
                        "git checkout . discards all working-tree changes under the current \
directory",
                    );
                }
            }
            // Memory rule: `git checkout -- <file>` overwrites the file from
            // the index/tree-ish and destroys its uncommitted changes. Judged by
            // what it destroys: a file git reports clean is Allow, a file with
            // uncommitted changes is Deny, and anything that cannot be probed
            // (no git probe in this session model, a non-literal path, a
            // `git -C`/`--git-dir`/`--work-tree` that moves the base) is Ask.
            if let Some(pos) = rest.iter().position(|t| *t == "--") {
                let moved_base = rest[..idx].iter().any(|t| {
                    ["-C", "--git-dir", "--work-tree"]
                        .iter()
                        .any(|f| t == f || t.starts_with(&format!("{f}=")))
                        || (t.starts_with("-C") && t.len() > 2)
                });
                let mut acc = VerdictAcc::default();
                for op in &rest[pos + 1..] {
                    let v = if moved_base {
                        Decision::ask(format!(
                            "git checkout -- `{op}`: the repository base is moved by a global \
option, so blastguard cannot tell whether the file has uncommitted changes it would discard"
                        ))
                    } else {
                        checkout_pathspec_verdict(ctx, op)
                    };
                    if let Some(deny) = acc.record(v) {
                        return deny;
                    }
                }
                return acc.finish();
            }
            Decision::Allow
        }
        // Round 2, MIRROR GAP (the same one twice). `restore` and `switch` are
        // the two halves git split `checkout` into; round 1 adopted `restore`'s
        // twin and left `switch`'s, so `git switch -f main` and
        // `git switch --discard-changes main` had no arm at all and reached
        // `_ => Allow` — both throw away uncommitted working-tree changes, which
        // is precisely what the `checkout --force` arm above denies.
        //
        // A plain `git switch <branch>` REFUSES to run with a dirty tree, so it
        // is not the same command and stays allowed; likewise `-c/--create`.
        "switch" => {
            if rest.contains(&"--force")
                || rest.contains(&"--discard-changes")
                || has_short(rest, 'f')
            {
                Decision::deny("git switch --force/--discard-changes discards working-tree changes")
            } else {
                Decision::Allow
            }
        }
        // Round 2: `git rm` had no arm, so `git rm .githooks/pre-commit`,
        // `git rm -rf .githooks`, `git rm -r -f .claude` and
        // `git rm --cached .claude/settings.json` all reached `_ => Allow`.
        // `.githooks/**` is tracked in this repo, so `git rm` really does delete
        // it from the working tree — the same one-command disarm as the
        // already-denied `rm -rf .githooks`, spelled through git.
        //
        // `--cached` is denied too: it leaves the bytes on disk but removes the
        // file from the repo, so the next clone/checkout has no gate. Ordinary
        // targets are untouched (`git rm src/old.rs` stays Allow) — only the
        // TARGET is classified, never the blast radius.
        "rm" => {
            for op in positional_operands(&rest[idx + 1..], &[]) {
                if let Some(deny) = protected_disarm_deny(ctx, "git rm", op) {
                    return deny;
                }
                if let Some(deny) = protected_tree_deny(ctx, "git rm", op) {
                    return deny;
                }
                if let Some(deny) = protected_glob_deny("git rm operand", op) {
                    return deny;
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
        "config" => {
            // `git config core.hooksPath <dir>` repoints EVERY git hook at once
            // — it is the single-command equivalent of overwriting every file
            // under `.git/hooks/`, and it leaves no trace in the working tree.
            // `--unset`/`--unset-all` is the same hazard in the other
            // direction (it disarms the repo's opt-in `.githooks` tree), so it
            // is denied too.
            //
            // Read-only forms are allowed: they change nothing. Anything that
            // mentions the key WITHOUT one of those flags is treated as a write
            // (the bare one-argument read included) — an unrecognised shape of
            // a hook-repointing command resolves to the restrictive side.
            let mentions_hookspath = rest[idx + 1..].iter().any(|t| {
                let t = normalized_command(t).to_ascii_lowercase();
                t == "core.hookspath" || t.starts_with("core.hookspath=")
            });
            let read_only = rest[idx + 1..].iter().any(|t| {
                matches!(
                    *t,
                    "--get" | "--get-all" | "--get-regexp" | "--get-urlmatch" | "--list" | "-l"
                )
            });
            if mentions_hookspath && !read_only {
                Decision::deny(HOOKSPATH_REASON)
            } else {
                Decision::Allow
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
                return Decision::deny("git stash clear/drop irreversibly deletes stashed changes");
            }
            // Round 2: the scan above is `.find(|t| !t.starts_with('-'))`, so it
            // SKIPS every flag — and the flags are where the destruction is.
            // `git stash -a`/`--all` and `git stash push -u` sweep untracked
            // (and, for `-a`, ignored) files out of the working tree. That is
            // the functional equal of the already-denied `git clean -fdx`, just
            // with the debris parked in a stash entry a later `stash drop`
            // erases. Every one of these was ALLOW on 0.2.19.
            //
            // Backlog b942eb2c: the flags are judged on the argv words bash
            // would hand to git (`stash_argv_words`), not on the
            // whitespace-split tokens, so `"-u"`, `--"all"`, `\-u` and
            // `$'-u'` are the `-u`/`--all` they become.
            let args = &rest[idx + 1..];
            const SWEEP_REASON: &str = "git stash -u/-a removes untracked (and with -a, ignored) \
files from the working tree, like git clean";
            match stash_argv_words(&args.join(" ")) {
                Determination::Known(words) => {
                    let outside = stash_words_outside_message(words);
                    if outside.iter().any(|w| is_stash_sweep_flag(w)) {
                        Decision::deny(SWEEP_REASON)
                    } else if outside.iter().any(|w| has_brace_expansion(w)) {
                        Decision::ask(
                            "blastguard cannot determine the argument words of this git stash \
(bash brace expansion such as a comma list can produce -u/-a, which removes untracked files \
from the working tree)",
                        )
                    } else {
                        Decision::Allow
                    }
                }
                // Where the argv words are cannot be determined, every
                // reading of every token is a flag candidate (no message
                // value is skipped); if none of them sweeps, the line is
                // still not provably harmless, so it is put to a human.
                Determination::Undetermined(_) => {
                    let any_reading_sweeps = args.iter().any(|t| {
                        let stripped: String = t
                            .chars()
                            .filter(|c| !matches!(c, '\'' | '"' | '\\' | '$'))
                            .collect();
                        is_stash_sweep_flag(t)
                            || is_stash_sweep_flag(&stripped)
                            || super::words::first_shell_word(t)
                                .is_some_and(|w| is_stash_sweep_flag(&w))
                    });
                    if any_reading_sweeps {
                        Decision::deny(SWEEP_REASON)
                    } else {
                        Decision::ask(
                            "blastguard cannot determine the argument words of this git stash \
(an unclosed quote, a command substitution or an escaped line break), so it cannot rule out \
-u/-a, which removes untracked files from the working tree",
                        )
                    }
                }
            }
        }
        _ => Decision::Allow,
    }
}

/// True for a stash argv word that sweeps untracked files out of the working
/// tree: `--all`, `--include-untracked`, any non-empty prefix of either that
/// git's option parser accepts as an abbreviation (`--al`, `--include-u`,
/// with or without an `=value` tail), or a short-flag bundle holding `a` or
/// `u`. No other `git stash push` long option (patch, staged, keep-index,
/// no-keep-index, quiet, message, pathspec-from-file, pathspec-file-nul)
/// starts with `a` or `i`, so every such prefix is unambiguous; and were it
/// ambiguous, treating it as a sweep is the restrictive side anyway.
fn is_stash_sweep_flag(w: &str) -> bool {
    if let Some(rest) = w.strip_prefix("--") {
        let name = rest.split('=').next().unwrap_or("");
        return !name.is_empty()
            && ("all".starts_with(name) || "include-untracked".starts_with(name));
    }
    is_short_flag(w) && (w.contains('a') || w.contains('u'))
}

/// True when the word holds a bash brace expansion (comma list or `..`
/// sequence), which can turn into a sweep flag at runtime. A plain
/// `stash@` followed by `{0}` is not one.
fn has_brace_expansion(w: &str) -> bool {
    let mut rest = w;
    while let Some(open) = rest.find('{') {
        let after = &rest[open + 1..];
        match after.find('}') {
            Some(close) => {
                let inner = &after[..close];
                if inner.contains(',') || inner.contains("..") {
                    return true;
                }
                rest = &after[close + 1..];
            }
            // Unbalanced brace: not provably literal.
            None => return true,
        }
    }
    false
}

/// The stash argv words minus the VALUE of a separate-word `-m` / `--message`
/// option. Backlog d0d5cc67 (#1526): `git stash push -m "stray cargo fmt
/// --all reformat"` used to put the message word `--all` into the flag scan.
///
/// `words` are whole argv words (`stash_argv_words`), so the word after an
/// exact `-m` / `--message` word is the value git takes, whatever it looks
/// like (`"-m" -u` stashes with message `-u`). A word that merely STARTS with
/// `-m` (`-mMSG`, or the single word `-m x'` from `"-m x'"`) is a glued
/// value and drops nothing; `--message=MSG` likewise.
fn stash_words_outside_message(words: Vec<String>) -> Vec<String> {
    let mut out = Vec::with_capacity(words.len());
    let mut it = words.into_iter();
    while let Some(w) = it.next() {
        let takes_value = w == "-m" || w == "--message";
        out.push(w);
        if takes_value {
            it.next();
        }
    }
    out
}

/// Split the stash argument text into the argv words bash would produce,
/// with quote removal applied, WITHOUT any expansion.
///
/// Covered exactly: whitespace word splitting (quote-aware, so a quote opened
/// in one whitespace token continues into the next), `'…'`, `"…"` (escapes
/// `\"` `\\` `\$` and backquote; any other backslash is kept), `\x` outside
/// quotes, `$'…'` ANSI-C quoting (`\a \b \e \E \f \n \r \t \v \\ \' \" \?`,
/// octal `\nnn`, `\xHH`, `\uHHHH`, `\UHHHHHHHH`, `\cX`; any other escape is
/// kept with its backslash, as bash does), and `$"…"` read as `"…"` (the
/// locale translation it requests is a runtime message catalog that is not
/// on the command line).
///
/// NOT expanded, left as literal text: `$VAR`, `~`, globs — their value is
/// runtime state, the same reach limit as `normalized_command`.
///
/// `Undetermined` (the caller resolves it restrictively): an unclosed quote,
/// a `$(`, `${` or backquote (their nested quoting is not modelled), a
/// backslash before whitespace or at the very end (`text` was re-joined
/// with single spaces, so whether that was an escaped space or a line
/// continuation that glues two words is no longer known), and an ANSI-C
/// numeric escape with no digits or an invalid code point.
fn stash_argv_words(text: &str) -> Determination<Vec<String>> {
    let chars: Vec<char> = text.chars().collect();
    let mut words = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        i += 1;
        match c {
            c if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut cur));
                    in_word = false;
                }
            }
            '\'' => {
                in_word = true;
                let Some(end) = chars[i..].iter().position(|&d| d == '\'') else {
                    return Determination::undetermined("stash: unclosed single quote");
                };
                cur.extend(&chars[i..i + end]);
                i += end + 1;
            }
            '"' => {
                in_word = true;
                match read_double_quoted(&chars, i, &mut cur) {
                    Some(next) => i = next,
                    None => {
                        return Determination::undetermined(
                            "stash: undeterminable double-quoted word",
                        )
                    }
                }
            }
            '\\' => {
                in_word = true;
                match chars.get(i) {
                    Some(&d) if !d.is_whitespace() => {
                        cur.push(d);
                        i += 1;
                    }
                    _ => {
                        return Determination::undetermined(
                            "stash: backslash before whitespace or at end",
                        )
                    }
                }
            }
            '$' if chars.get(i) == Some(&'\'') => {
                in_word = true;
                match read_ansi_c(&chars, i + 1, &mut cur) {
                    Some(next) => i = next,
                    None => return Determination::undetermined("stash: undeterminable $'…' word"),
                }
            }
            '$' if chars.get(i) == Some(&'"') => {
                in_word = true;
                match read_double_quoted(&chars, i + 1, &mut cur) {
                    Some(next) => i = next,
                    None => {
                        return Determination::undetermined("stash: undeterminable $\"…\" word")
                    }
                }
            }
            '$' if matches!(chars.get(i), Some('(' | '{')) => {
                return Determination::undetermined("stash: command or parameter substitution");
            }
            '`' => return Determination::undetermined("stash: backquote substitution"),
            c => {
                in_word = true;
                cur.push(c);
            }
        }
    }
    if in_word {
        words.push(cur);
    }
    Determination::known(words)
}

/// Body of a `"…"` starting at `chars[i]` (just past the opening quote),
/// appended to `out`. Returns the index just past the closing quote, or
/// `None` when undeterminable: no closing quote, a `$(` / `${` / backquote
/// inside, or a backslash before whitespace (possibly a line continuation).
fn read_double_quoted(chars: &[char], mut i: usize, out: &mut String) -> Option<usize> {
    while let Some(&d) = chars.get(i) {
        i += 1;
        match d {
            '"' => return Some(i),
            '\\' => {
                let e = *chars.get(i)?;
                if e.is_whitespace() {
                    return None;
                }
                i += 1;
                if !matches!(e, '"' | '\\' | '$' | '`') {
                    out.push('\\');
                }
                out.push(e);
            }
            '`' => return None,
            '$' if matches!(chars.get(i), Some('(' | '{')) => return None,
            d => out.push(d),
        }
    }
    None
}

/// Body of a `$'…'` starting at `chars[i]` (just past the opening quote),
/// decoded as bash's ANSI-C quoting and appended to `out`. Returns the index
/// just past the closing quote, or `None` when undeterminable (no closing
/// quote, a numeric escape without digits, an invalid code point, `\c` at the
/// end).
fn read_ansi_c(chars: &[char], mut i: usize, out: &mut String) -> Option<usize> {
    // Up to `max` digits of `radix` starting at `chars[*i]`; None if none.
    fn digits(chars: &[char], i: &mut usize, radix: u32, max: usize) -> Option<u32> {
        let mut v: u32 = 0;
        let mut n = 0;
        while n < max {
            let Some(d) = chars.get(*i).and_then(|c| c.to_digit(radix)) else {
                break;
            };
            v = v.checked_mul(radix)?.checked_add(d)?;
            *i += 1;
            n += 1;
        }
        (n > 0).then_some(v)
    }
    while let Some(&d) = chars.get(i) {
        i += 1;
        if d == '\'' {
            return Some(i);
        }
        if d != '\\' {
            out.push(d);
            continue;
        }
        let e = *chars.get(i)?;
        i += 1;
        let decoded = match e {
            'a' => '\u{07}',
            'b' => '\u{08}',
            'e' | 'E' => '\u{1b}',
            'f' => '\u{0c}',
            'n' => '\n',
            'r' => '\r',
            't' => '\t',
            'v' => '\u{0b}',
            '\\' | '\'' | '"' | '?' => e,
            '0'..='7' => {
                i -= 1;
                char::from_u32(digits(chars, &mut i, 8, 3)? & 0xff)?
            }
            'x' => char::from_u32(digits(chars, &mut i, 16, 2)?)?,
            'u' => char::from_u32(digits(chars, &mut i, 16, 4)?)?,
            'U' => char::from_u32(digits(chars, &mut i, 16, 8)?)?,
            'c' => {
                let x = *chars.get(i)?;
                i += 1;
                char::from_u32(if x == '?' { 0x7f } else { (x as u32) & 0x1f })?
            }
            other => {
                out.push('\\');
                other
            }
        };
        out.push(decoded);
    }
    None
}
