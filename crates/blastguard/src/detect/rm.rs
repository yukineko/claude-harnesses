use super::{
    confined_ask, dotgit_link_deny, exclude, glob_literal_prefix, has_glob_meta,
    has_unresolvable_expansion, home_asset_deny, is_short_flag, protected_disarm_deny,
    protected_tree_deny, Ctx, Decision, Determination, Placement, SafeRoots,
};

mod operand_words;
use operand_words::{
    expand_operand_with, has_unquoted_glob, place_literal, rm_operand_words, OperandSpelling,
    Placed, VarLookup,
};

/// The protected-path Deny for an operand of a non-wildcard `rm` that carries
/// QUOTED glob characters (`".claude/*"`): the per-operand protected check in
/// `analyze_rm_placed` skips every glob-bearing token, and the wildcard Deny
/// that backs that skip does not apply to a quoted glob. The quote-removed
/// spelling is judged both as the literal file it names and, restrictively, as
/// the pattern it looks like ([`protected_glob_deny`](super::protected::protected_glob_deny)).
fn quoted_glob_protected_deny(ctx: &Ctx<'_>, rest: &[&str]) -> Option<Decision> {
    for word in rm_operand_words(rest) {
        if !has_glob_meta(&word) {
            continue;
        }
        let OperandSpelling::Paths(paths) = expand_operand_with(&word, ctx.scope.home_real(), None)
        else {
            continue; // unresolvable: `rm_system_harm_deny` makes it an Ask
        };
        for p in &paths {
            if let Some(deny) = protected_disarm_deny(ctx, "rm", p)
                .or_else(|| super::protected::protected_glob_deny("rm", p))
            {
                return Some(deny);
            }
        }
    }
    None
}

/// Whether an `rm` argument list asks for recursion (`-r`/`-R`/`--recursive`).
fn rm_is_recursive(rest: &[&str]) -> bool {
    rest.iter()
        .any(|t| (is_short_flag(t) && (t.contains('r') || t.contains('R'))) || *t == "--recursive")
}

/// The system-harm Deny for an `rm` in `tokens` (one segment, at `seg_idx` of
/// the command line `line`) whose operand is spelled with an expansion the
/// line's own text resolves to a path in a system directory, else `Allow`.
///
/// [`analyze_rm`] does not resolve expansions: an operand it cannot read is an
/// Ask (user ruling 2026-11-07, "Ask, deny known paths"). This pass names the
/// known paths among them, so they stay a system-harm Deny:
///
///   * `$NAME` / `${NAME}` with a literal assignment in an EARLIER segment of
///     the same line that reaches this one unconditionally and is not rebound
///     — exactly the believability rules of
///     [`resolve_expanded_command_word`](super::vars::resolve_expanded_command_word)
///     (`X=/etc/passwd; rm $X`, `X=/etc/passwd && rm "$X"`);
///   * `$(…)` / a backtick whose body is `echo` / `printf %s` of literal words
///     (`rm $(echo /etc/passwd)`).
///
/// Every other expansion is left unresolved here (and so stays an Ask in
/// [`analyze_rm`]); this function only ever adds a Deny.
///
/// The non-`rm` system-file mutators (`unlink`, `rmdir`, `mv`, `sed -i`,
/// `perl -i`, `chmod`, `chown`, `chgrp`, `rsync`) get the same resolution for
/// the files they modify: [`super::system_paths::known_mutation_harm`], so
/// `X=/etc/passwd; unlink $X` is the same system-harm Deny as
/// `unlink /etc/passwd`, not the expansion Ask.
pub(super) fn known_path_harm(
    tokens: &[&str],
    line: &str,
    seg_idx: usize,
    ctx: &Ctx<'_>,
) -> Decision {
    use super::vars::{resolve_expanded_command_word, CommandWordOrigin};
    let vars =
        |name: &str| match resolve_expanded_command_word(line, seg_idx, &format!("${{{name}}}")) {
            CommandWordOrigin::Literal(v) => Some(v),
            _ => None,
        };
    let resolve = |word: &str| match expand_operand_with(word, ctx.scope.home_real(), Some(&vars)) {
        OperandSpelling::Paths(p) => Some(p),
        OperandSpelling::Unresolvable(_) => None,
    };
    for idx in super::words::command_candidates(tokens) {
        let cmd = super::words::normalized_command(tokens[idx]);
        let rest = &tokens[idx + 1..];
        if cmd != "rm" {
            let deny = super::system_paths::known_mutation_harm(&cmd, rest, ctx, &resolve);
            if deny.is_deny() {
                return deny;
            }
            continue;
        }
        let words = rm_operand_words(rest);
        if !words.iter().any(|w| has_unresolvable_expansion(w)) {
            continue;
        }
        if let SystemHarm::Deny(deny) =
            rm_system_harm(ctx, &words, rm_is_recursive(rest), Some(&vars))
        {
            return deny;
        }
    }
    Decision::Allow
}

pub(super) fn analyze_rm(rest: &[&str], ctx: &Ctx<'_>) -> Decision {
    let mut unplaced: Option<String> = None;
    let verdict = analyze_rm_placed(rest, ctx, &mut unplaced);
    // A literal operand the system-harm check could not place (see
    // [`SystemHarm::Unplaced`]) is not an Allow by default: when every other
    // rule would have let the rm through, ask instead. Any Deny / Ask from the
    // other rules stands as it is.
    match (verdict, unplaced) {
        (Decision::Allow, Some(why)) => Decision::ask(why),
        (verdict, _) => verdict,
    }
}

fn analyze_rm_placed(rest: &[&str], ctx: &Ctx<'_>, unplaced: &mut Option<String>) -> Decision {
    let recursive = rm_is_recursive(rest);
    // Round 4 (adversarial verifier): `-d`/`--dir` removes an EMPTY directory.
    // It is NOT recursive, so it never triggered the container check below, and
    // `rm -d .claude/hooks` / `rm --dir .git/hooks` / `rm -d .claude` were ALLOW.
    // `-d` only succeeds when the directory is empty, but that is a real state
    // (fresh scaffold, inline-hook repos, a dir emptied by other means), and
    // removing the container takes every protected path it would hold with it.
    // `d` is the only rm short flag spelled with that letter, so a cluster
    // containing it unambiguously means `--dir`.
    let dir_flag = rest
        .iter()
        .any(|t| (is_short_flag(t) && t.contains('d')) || *t == "--dir");
    let operands: Vec<&str> = rest
        .iter()
        .filter(|t| !t.starts_with('-'))
        .copied()
        .collect();
    // A glob character inside quotes is not expanded by the shell: plain
    // `rm "*.tmp"` removes the ONE file named `*.tmp`. That is also the text a
    // loop body `for f in *.tmp; do rm "$f"; done` is judged as once the loop
    // value is substituted back in (`compound::loop_variants`), and there the
    // glob belongs to `for`: each `rm` receives one entry it matched. So a
    // plain (non-`-r`, non-`-d`) rm whose glob characters are all quoted is not
    // a wildcard rm; its quote-removed operands are still checked against the
    // protected trees below (`quoted_glob_protected_deny`).
    let wildcard = operands.iter().any(|o| has_glob_meta(o))
        && (recursive || dir_flag || rm_operand_words(rest).iter().any(|w| has_unquoted_glob(w)));
    if !wildcard {
        if let Some(deny) = quoted_glob_protected_deny(ctx, rest) {
            return deny;
        }
    }

    // PROTECTED PRECEDENCE, ahead of BOTH exits below. This arm was the last
    // place in the crate where `is_config_file` was consulted without
    // `is_protected_path` in front of it — the very precedence 0.2.18
    // established for every write path — and it let deletion through twice
    // over:
    //
    //   * the "below the destructive bar" early return measures how MANY files
    //     vanish and says nothing about WHICH, so a single
    //     `rm .claude/settings.json` (one file, no wildcard) was Allow;
    //   * the config-file exemption then caught the recursive form, because
    //     `.claude/**` sits in ALLOW_GLOBS — so `rm -rf .claude` and even
    //     `rm -rf /Users/yuki/.claude` were Allow while `rm -rf .githooks` was
    //     correctly denied. The difference was never intended; it is just which
    //     tree happens to be on the allowlist.
    //
    // Deleting a gate's config is the most complete disarm there is, so the
    // target is classified BEFORE the blast-radius question is asked at all.
    for operand in &operands {
        // A wildcard operand is not a literal path; matching it against the
        // protected globs would be a guess in the permissive direction (it can
        // only ever fail to match and thereby look clean), so it is left to the
        // rules below: `tmp_glob_confined` (the only one that can Allow it)
        // runs its own protected check on every name it could expand to, and
        // everything else asks or denies it.
        if has_glob_meta(operand) {
            continue;
        }
        if let Some(deny) = protected_disarm_deny(ctx, "rm", operand) {
            return deny;
        }
        // The CONTAINER case: `.claude` matches no protected glob but is where
        // the protected files live. Asked for a recursive rm (`-r`) OR a
        // directory rm (`-d`/`--dir`) — both delete the directory itself. A
        // plain non-recursive `rm <dir>` (neither flag) fails outright, so there
        // is nothing to resolve restrictively for it.
        if recursive || dir_flag {
            let action = if recursive { "recursive rm" } else { "rm -d" };
            if let Some(deny) = protected_tree_deny(ctx, action, operand) {
                return deny;
            }
        }
    }

    // SYSTEM-HARM axis (backlog fb338793, user ruling 2026-11-07:
    // 「いかなる理由であれシステムに害になるコマンドはdenyすべし」). Ahead of the
    // "below the destructive bar" exit for ONE file, which measures how many
    // files vanish and not which: `rm /etc/passwd`, `rm -f /bin/ls` and
    // `rm -f /usr/lib/libSystem.B.dylib` were Allow on 0.2.287. Applies to
    // recursive and wildcard operands too, so every `rm` into a system
    // directory carries the system-harm marker whichever later rule would
    // have refused it. See [`rm_system_harm_deny`].
    match rm_system_harm_deny(ctx, &rm_operand_words(rest), recursive) {
        SystemHarm::Deny(deny) => return deny,
        SystemHarm::Unplaced(why) => *unplaced = Some(why),
        SystemHarm::Clear => {}
    }

    if !recursive && !wildcard {
        // Backlog eba15868: ONE file can still be the whole checkout. A linked
        // worktree's `.git` is a `gitdir:` FILE, so `rm <wt>/.git` succeeds
        // without `-r` and severs the work tree from its repository — while
        // `Write`/`>` onto the same path is already refused as git internals.
        // Only the recursive arm below has a deletion classifier for `.git`
        // directories, so the non-recursive arm answers here.
        if let Some(deny) = operands.iter().find_map(|o| dotgit_link_deny("rm", o)) {
            return deny;
        }
        // Backlog 6067a527: one file can be irreplaceable (`rm ~/.ssh/id_rsa`).
        if let Some(deny) = operands.iter().find_map(|o| home_asset_deny(ctx, "rm", o)) {
            return deny;
        }
        // Single, non-recursive rm of named files — below the destructive bar.
        return Decision::Allow;
    }

    // WORKTREE STORAGE axis (user ruling 2026-09-30, backlog 873651b9: "remove
    // the hook that restricts deleting worktrees"). A recursive rm whose EVERY
    // operand is a literal path resolving strictly inside a worktree storage
    // root (`$HOME/.condukt/worktrees`, `<parent>/.harness-worktrees`) is
    // Allow — no confirmation. Placed AFTER the protected-path precedence above
    // (so `.git/hooks`, `.claude`, … under a worktree storage root still Deny,
    // unless `Ctx::in_worktree_checkout` places them strictly inside a linked
    // worktree checkout — user ruling 2026-10-03) and only for
    // `-r`; the root itself, anything above it, a glob, a symlink out, ANY
    // operand spelled with a `..` component (even one that stays inside — see
    // the `..` rule of `worktree_rm_eligible`), and any operand that cannot be
    // resolved all fall through to the unchanged logic below. The root counts
    // only if it is a real,
    // non-symlinked directory at judge time (`scope`), and the Allow is only
    // available when the WHOLE top-level command is bare `rm`/`cd` segments
    // (`worktree_rm_eligible`), so no other segment can swap the root before
    // this rm runs; if the command contains any `cd`, every rm operand must
    // also be absolute, because a `cd` may fail or run in a subshell (`&`,
    // `|`) and a relative operand would then name a path that was never
    // judged. See `worktree_rm_eligible` and `worktree_confined`.
    if recursive && worktree_confined(ctx, &operands) {
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

    // DELETION PRINCIPLE (backlog 3aa215e1, user ruling 2026-09-30): a
    // recursive rm of LITERAL operands is judged by WHAT it destroys — see
    // `crate::deletion` for the class table. Placed after the protected-path
    // precedence, the worktree-storage Allow and the config-file exemption
    // (all unchanged), and before the confined Ask / shape Deny it replaces
    // for the cases it can decide. Only for a command `deletion_rm_eligible`
    // accepts as a whole; anything else, and every operand it cannot decide,
    // keeps the logic below.
    if recursive && !wildcard && ctx.deletion_rm_eligible.literal {
        match crate::deletion::judge_rm(ctx.scope, &operands, ctx.base_for("rm")) {
            crate::deletion::RmJudgement::Allow => return Decision::Allow,
            crate::deletion::RmJudgement::Deny(reason) => return Decision::deny(reason),
            crate::deletion::RmJudgement::FallThrough => {}
        }
    }

    // /tmp WILDCARD axis (user ruling 2026-11-02, verbatim
    // 「そもそも/tmp以下はrm -frを削除していい。worktreeなどと同じあつかいにして」,
    // backlog 486fb7de / #1531). A wildcard can only expand to entries of
    // its literal parent directory, so when that parent provably is a real
    // directory strictly inside the real fixed `/tmp` root (or the root itself
    // with a literal name head of 4+ characters, ruling 2026-11-05), the
    // blast radius is bounded exactly like a literal operand there. The same
    // holds for a parent strictly inside a linked worktree checkout under a
    // worktree storage root (user ruling 2026-11-07, managed disposable
    // place). Every condition and every refusal is in
    // [`tmp_glob_confined`]; anything it cannot decide keeps the logic below.
    if wildcard && tmp_glob_confined(ctx, &operands) {
        return Decision::Allow;
    }

    // LOCATION axis (see `crate::scope`), placed deliberately:
    //   * AFTER the protected-path precedence at the top of this function, so
    //     deleting a gate config stays a flat Deny wherever it lives;
    //   * AFTER the config-file exemption above, which is an Allow and
    //     therefore outranks the Ask below;
    //   * BEFORE the shape-only Deny, which is exactly the verdict it replaces
    //     — and only for targets that resolved strictly INSIDE a safe root.
    //     `Placement::IsRoot` is not accepted here: `rm -rf <project>` takes
    //     `.git` and every gate config with it.
    let action = if recursive {
        "recursive rm (-r)"
    } else {
        "wildcard rm"
    };
    if let Some(targets) = placeable_targets(&operands) {
        if let Some(root) = ctx.confined_root("rm", &targets) {
            return confined_ask(action, &root, &operands);
        }
    }

    if recursive {
        Decision::deny("recursive rm (-r) can delete an entire directory tree")
    } else {
        Decision::deny("rm with a wildcard can delete many files at once")
    }
}

/// `Deny` (system harm, [`Decision::deny_system_harm`]) when an `rm` operand
/// names `/`, or a path at or below a system directory
/// ([`crate::scope::is_inside_system_dir`]: `/bin`, `/etc`, `/usr`,
/// `/System`, `/Library`, …), and the session does not own it (a project
/// checked out under `/opt` is a safe root and escapes, exactly as for
/// `system_path_block`).
///
/// Judged on the operand as written, made absolute against the segment's
/// base, and again with its PARENT directory resolved through symlinks (the
/// final component is what `rm` unlinks, so it is not followed): `rm
/// <proj>/esc/etc/passwd` with `esc -> /` unlinks `/etc/passwd`. A wildcard
/// operand is judged by its literal directory prefix (`/usr/lib/*` cannot
/// expand outside `/usr/lib`; `/*` is `/`).
///
/// Each operand is first read as a shell word ([`operand_words`], backlog
/// 3c750df2): quotes removed, brace lists expanded, `~`/`$HOME` expanded, and
/// a home-relative climb above home also judged rooted at `/`. An operand
/// whose text contains any other expansion (`$X`, `$(…)`, a backtick) is NOT
/// skipped: what it deletes is unknown, so it is [`SystemHarm::Unplaced`]
/// (user ruling 2026-11-07, "Ask, deny known paths": an Ask for the main
/// agent, a Deny for a subagent — fail closed, never Allow). The expansions
/// the line's own text resolves are judged by
/// [`known_path_harm`] instead, so a known system path stays a
/// Deny. A literal operand that is not a filename (a control character, a
/// `{}` placeholder) is [`SystemHarm::Unplaced`] too: every rule below still
/// runs, and an Allow they would give becomes an Ask ([`analyze_rm`]). A
/// relative operand with no known working directory is NOT judged here (kept
/// as before this check existed; see `operand_words::Placed::RelativeWithoutBase`).
pub(super) fn rm_system_harm_deny(
    ctx: &Ctx<'_>,
    operands: &[String],
    recursive: bool,
) -> SystemHarm {
    rm_system_harm(ctx, operands, recursive, None)
}

/// [`rm_system_harm_deny`], with `vars` resolving the expansions
/// [`expand_operand_with`] models.
fn rm_system_harm(
    ctx: &Ctx<'_>,
    operands: &[String],
    recursive: bool,
    vars: Option<VarLookup<'_>>,
) -> SystemHarm {
    let mut unplaced: Option<String> = None;
    for word in operands {
        let paths = match expand_operand_with(word, ctx.scope.home_real(), vars) {
            OperandSpelling::Paths(p) => p,
            OperandSpelling::Unresolvable(why) => {
                unplaced = unplaced.or(Some(format!(
                    "this rm operand `{word}` cannot be resolved to a path ({why}), so what it \
deletes is unknown — blastguard cannot tell whether it is an OS file or directory, so it asks \
rather than assume it harmless"
                )));
                continue;
            }
        };
        for p in &paths {
            match rm_path_system_harm_deny(ctx, word, p, recursive) {
                SystemHarm::Deny(deny) => return SystemHarm::Deny(deny),
                SystemHarm::Unplaced(why) => unplaced = unplaced.or(Some(why)),
                SystemHarm::Clear => {}
            }
        }
    }
    unplaced.map_or(SystemHarm::Clear, SystemHarm::Unplaced)
}

/// The system-harm answer for an `rm` operand list.
pub(super) enum SystemHarm {
    /// An operand lands in a system directory.
    Deny(Decision),
    /// No operand lands in a system directory, but one could not be resolved
    /// (an expansion this crate does not resolve) or not placed as an absolute
    /// path (a control character, a `{}` placeholder), so "outside every
    /// system directory" is not established for it.
    /// [`analyze_rm`] turns what would otherwise be an Allow into an Ask with
    /// this reason.
    Unplaced(String),
    /// Every operand was placed, and none is in a system directory.
    Clear,
}

/// One shell-expanded `rm` operand path `op` (spelled `word`) against the
/// system directories; see [`rm_system_harm_deny`].
fn rm_path_system_harm_deny(ctx: &Ctx<'_>, word: &str, op: &str, recursive: bool) -> SystemHarm {
    let spelled: String = if has_glob_meta(op) {
        match glob_literal_prefix(op) {
            Some(p) if p.is_empty() && op.starts_with('/') => "/".to_string(),
            Some(p) if !p.is_empty() => p,
            // A relative glob with no literal directory (`*.o`, `a*/b`)
            // expands inside the working directory.
            _ => ".".to_string(),
        }
    } else {
        op.to_string()
    };
    let lexical = match place_literal(&spelled, ctx.base_for("rm")) {
        Placed::Absolute(l) => l,
        // Kept as before this check existed; see `Placed::RelativeWithoutBase`.
        Placed::RelativeWithoutBase => return SystemHarm::Clear,
        Placed::NotLiteral(why) => {
            return SystemHarm::Unplaced(format!(
                "rm operand `{word}` could not be placed as an absolute path ({why}), so \
blastguard cannot establish that it is outside every system directory"
            ));
        }
    };
    let parent_real = lexical.rsplit_once('/').and_then(|(parent, name)| {
        let parent = if parent.is_empty() { "/" } else { parent };
        ctx.scope.resolve(parent).map(|p| {
            let p = match p.strip_prefix("/System/Volumes/Data/") {
                Some(rest) => format!("/{rest}"),
                None => p,
            };
            if name.is_empty() {
                p
            } else {
                format!("{}/{name}", p.trim_end_matches('/'))
            }
        })
    });
    let is_system = |p: &str| p == "/" || crate::scope::is_inside_system_dir(p);
    let hit = if is_system(&lexical) {
        Some(lexical.clone())
    } else {
        parent_real.filter(|r| is_system(r))
    };
    let Some(hit) = hit else {
        return SystemHarm::Clear;
    };
    if ctx.confined_root("rm", &[lexical.as_str()]).is_some() {
        return SystemHarm::Clear;
    }
    let shown = if hit == lexical {
        format!("`{word}`")
    } else {
        format!("`{word}` (which lands on `{hit}`)")
    };
    // The shape clause keeps the wording `crate::rule_id` already files
    // recursive / wildcard rm under; a single named file gets its own.
    let shape = if recursive {
        "recursive rm (-r) can delete an entire directory tree, and this one"
    } else if has_glob_meta(op) {
        "rm with a wildcard can delete many files at once, and this one"
    } else {
        "this rm"
    };
    SystemHarm::Deny(Decision::deny_system_harm(format!(
        "{shape} deletes from a system directory: {shown} — removing an OS file or \
directory (a binary, a library, `/etc` configuration) can break the machine itself, so \
blastguard refuses it whatever the flags and however few files it names"
    )))
}

impl Ctx<'_> {
    /// True ONLY when `path` is a protected-NAMED path that is the checkout's
    /// own file: spelled ABSOLUTE, no glob/expansion, on a command line that is
    /// a single plain command ([`checkout_exempt_eligible`]), and placed by
    /// [`SafeRoots::classify_worktree_checkout`] as
    /// [`Placement::Inside`] a linked worktree checkout under a worktree
    /// storage root (user ruling 2026-10-03:
    /// 「worktreeの編集や削除を拒むのはむしろ不正なのでなおしてほしい」).
    ///
    /// Every other answer — relative spelling, an ineligible command line,
    /// `IsRoot`, `Outside`, and every `Undetermined` — is `false`, i.e. the
    /// protected-path rule applies exactly as before. There is no default that
    /// answers `true`.
    ///
    /// Relative operands are refused because the base they are judged against
    /// is this analysis' model of the cwd, and a cwd change it does not model
    /// (`env -C`, `git -C`, a `cd` that fails) would place a main-tree file
    /// inside a checkout. An absolute operand's placement depends on no base.
    /// (After a `cd D &&` that [`literal_cd_and_rest`] accepts, a relative
    /// redirect target, and a relative operand of the verbs the cwd walk
    /// rewrites ([`super::verb_targets`]), reach here already re-expressed as the
    /// absolute `D/<operand>`; any other relative operand is still relative
    /// and still refused by the check below.)
    /// The placement (symlinks included) is observed at JUDGE time; that is
    /// why the whole line must be one plain command — see
    /// [`checkout_exempt_eligible`].
    pub(super) fn in_worktree_checkout(&self, path: &str) -> bool {
        if !self.checkout_exempt_eligible
            || !path.starts_with('/')
            || has_glob_meta(path)
            || has_unresolvable_expansion(path)
        {
            return false;
        }
        match self.scope.classify_worktree_checkout(path, None) {
            Determination::Known(Placement::Inside { .. }) => true,
            Determination::Known(Placement::IsRoot { .. } | Placement::Outside { .. })
            | Determination::Undetermined(_) => false,
        }
    }
}

/// Whole-command precondition for [`Ctx::in_worktree_checkout`]: may a
/// protected-NAMED path inside a linked worktree checkout lose the
/// protected-path rule on this command line at all?
///
/// The checkout placement (no symlink below the checkout, the checkout's own
/// `.git` file) is observed when the hook runs. A command line that can MUTATE
/// the tree before its own later write runs defeats that observation:
/// `mv <wt>/.githooks <wt>/old && ln -s <main>/.githooks <wt>/.githooks &&
/// echo x > <wt>/.githooks/pre-push` judges a real directory and writes into
/// the main tree's gates. So the exemption is available only to a line that is
/// ONE plain command:
///
///   * the empty line (the Write / Edit / MultiEdit / NotebookEdit tools,
///     which perform exactly one write);
///   * otherwise no `;`, `&`, `|`, `(`, `)`, `{`, `}`, `<`, `$`, backtick,
///     backslash or line break anywhere (no second command, subshell, process
///     or command substitution, expansion, here-doc, or escaped separator);
///   * and the first word is one of [`CHECKOUT_EXEMPT_VERBS`] — verbs that
///     neither create symlinks nor run other programs (no `ln`, `find -exec`,
///     `xargs`, shells, `env`, `git`, `sudo`, assignments).
///
/// Over-matching only withholds the exemption, which is the restrictive
/// direction: the protected-path rule then applies exactly as before.
///
/// [`Ctx::new`] ALSO evaluates this on `REST` alone for a line `cd D && REST`
/// accepted by [`literal_cd_and_rest`]: a `cd` neither creates, links nor moves
/// anything, and `&&` guarantees `REST` runs only in `D`, so `REST` is still
/// the one plain command that mutates the tree. Its relative targets have been
/// placed at `D/<target>` by then, so [`Ctx::in_worktree_checkout`] still sees
/// only absolute paths.
pub(super) fn checkout_exempt_eligible(command: &str) -> bool {
    if command.trim().is_empty() {
        return true;
    }
    if command.chars().any(|c| {
        matches!(
            c,
            ';' | '&' | '|' | '(' | ')' | '{' | '}' | '<' | '$' | '`' | '\\' | '\n' | '\r'
        )
    }) {
        return false;
    }
    command
        .split_whitespace()
        .next()
        .is_some_and(|w| CHECKOUT_EXEMPT_VERBS.contains(&w))
}

/// First words [`checkout_exempt_eligible`] accepts: each performs its own
/// writes/deletes on its named operands and nothing else.
const CHECKOUT_EXEMPT_VERBS: &[&str] = &[
    "rm", "rmdir", "unlink", "chmod", "chown", "chgrp", "sed", "gsed", "tee", "cp", "mv",
    "install", "echo", "printf", "cat", "sort", "uniq", "touch", "truncate",
];

/// The command `REST` of a line `cd D && REST` whose `cd` provably ties `REST`
/// to the directory `D`, or `None`.
///
/// `cd D && CMD` used to be judged stricter than `CMD` with its relative
/// operands written as `D/<operand>`: the three whole-command eligibility
/// predicates ([`worktree_rm_eligible`], [`deletion_rm_eligible`],
/// [`checkout_exempt_eligible`]) refuse a relative operand whenever ANY `cd` is
/// on the line, because with `;`, `||`, `|` or `&` a `cd` may fail or run in a
/// different shell while the later command still runs in the ORIGINAL cwd (see
/// the cd rule of [`worktree_rm_eligible`]). With `&&` alone neither can
/// happen: a failed `cd` aborts the rest, and nothing runs in another shell.
/// For exactly that shape [`Ctx::new`] also evaluates the three predicates on
/// `REST` alone — `REST`'s relative operands have already been re-expressed as
/// `D/<operand>` by the cwd walk ([`advance_cwd_and_rewrite`],
/// [`place_redirect_target`]) by the time any per-operand rule judges them, so
/// the verdict is the one the absolute spelling gets. Each predicate still has
/// to accept `REST` on its own terms; this only removes the `cd` from the text
/// they read.
///
/// `Some(REST)` only when ALL of these hold; anything else is `None` and every
/// predicate reads the whole line exactly as before:
///   * the line starts with exactly `cd `, then one word `D`, then `&&`, then a
///     non-empty `REST` (no leading spaces, no `cd` option, no `cd -`, no
///     second `cd` operand);
///   * `D` is absolute (starts with `/`) and every character is a Unicode
///     alphanumeric or one of `/ . _ - + , @ %` — no quote, `$`, backtick,
///     glob, `~`, brace, escape or separator — and no component of `D` is
///     exactly `.` or `..` (the cwd walk collapses those lexically, the kernel
///     physically);
///   * `D` resolves, through the injected resolver, to an existing real
///     directory ([`SafeRoots::resolves_to_existing_dir`]; no resolver = `None`);
///   * `REST` contains no `;`, `|`, newline, carriage return or `..`, and every
///     `&` in it is part of an `&&` (so `2>&1`, `&>` and a trailing `&` also
///     answer `None`). Every command of `REST` therefore runs only after the
///     `cd` succeeded, in the same shell, in `D`.
///
/// Over-matching here would be a weakening, so every rule is a refusal: an
/// unusual-but-harmless spelling simply keeps its old (stricter) verdict.
pub(super) fn literal_cd_and_rest<'c>(scope: &SafeRoots, command: &'c str) -> Option<&'c str> {
    let after_cd = command.strip_prefix("cd ")?;
    let (dir, after_dir) = after_cd.split_once(' ')?;
    let rest = after_dir.trim_start_matches(' ').strip_prefix("&&")?;
    let rest = rest.trim_start_matches(' ');
    let dir_char_ok =
        |c: char| c.is_alphanumeric() || matches!(c, '/' | '.' | '_' | '-' | '+' | ',' | '@' | '%');
    if !dir.starts_with('/')
        || !dir.chars().all(dir_char_ok)
        || dir.split('/').any(|c| c == "." || c == "..")
    {
        return None;
    }
    if rest.trim().is_empty()
        || rest.contains([';', '|', '\n', '\r'])
        || rest.contains("..")
        || rest.replace("&&", "").contains('&')
    {
        return None;
    }
    if !scope.resolves_to_existing_dir(dir) {
        return None;
    }
    Some(rest)
}

/// Whole-command precondition for the worktree-storage `Allow` (backlog
/// 873651b9, second pass).
///
/// The storage roots are checked at JUDGE time (`scope`: real directory, no
/// symlinked component). Another segment of the SAME command runs before the
/// `rm` and could create, move, link or replace the root or an ancestor
/// (`ln -s ~/Documents R && rm -rf R/p`, `mv R away && ln -s X R && rm -rf R/x`,
/// `mkdir`, `cp`, a redirection, a nested `bash -c`, a function or alias
/// definition, …). Enumerating the verbs that can do that is a denylist, so
/// this is an ALLOWLIST instead, and it is deliberately blunt:
///
///   * every character of the command is a plain word character — Unicode
///     alphanumerics, space, and `/ . _ - + , = : @ %` — or one of the segment
///     separators `;`, `&`, `|`. No quotes, `$`, backticks, parentheses,
///     braces, redirections (`<`, `>`), globs, `~`, `#`, `!`, backslash,
///     newline, tab or any other control character;
///   * splitting on `;`, `&` and `|` (exact, because no quoting is possible),
///     every segment is empty or its FIRST whitespace-separated word is
///     exactly `rm` or `cd`;
///   * THE cd RULE: if ANY segment is a `cd`, every word of every `rm` segment
///     after `rm` is an option word (leading run only, see
///     [`rm_words_are_flags_then_absolute`]) or an ABSOLUTE path (starts with
///     `/`). A relative `rm` operand is eligible only in a command with no
///     `cd` at all, where it is judged against the payload cwd — which is the
///     runtime cwd. ONE exception lives in [`Ctx::new`], not here: for a line
///     `cd D && REST` that [`literal_cd_and_rest`] accepts (literal, existing,
///     absolute `D` with no `.`/`..`, tied to `REST` by `&&` only), this
///     function is ALSO evaluated on `REST` alone, so `REST`'s relative `rm`
///     operands — already rewritten to `D/<operand>` by the cwd walk — are
///     eligible exactly as their absolute spelling is;
///   * THE `..` RULE: no word of any `rm` segment after `rm` (option or
///     operand) has a component that is exactly `..` ([`has_dotdot_component`]:
///     `..`, `../x`, `a/../b`, `a/..`, `--opt=../x`). `cd` words are not
///     checked: with a `cd` present every `rm` operand is absolute, so the cwd
///     does not reach them.
///
/// Why the `..` rule exists (verifier finding on 8de9b04b): `scope` collapses
/// `..` LEXICALLY and only then asks the resolver for the real path, while the
/// kernel resolves `..` against the REAL parent — i.e. after following any
/// symlink before it. With `lnk` a symlink inside a worktree pointing outside,
/// `rm -rf <root>/w/lnk/../v2` was judged as `<root>/w/v2` (Allow) and deletes
/// `<outside>/v2`; likewise relative `lnk/../x`, nested `sub/lnk2/../x`, and a
/// trailing `<root>/w/x/..` with `x` a symlink out. Rather than model `..`
/// physically, any `..` in the raw spelling withholds the Allow — including
/// spellings that would in fact stay inside. The operand is then judged by the
/// pre-existing logic, which still collapses `..` lexically (see `scope`'s
/// module doc); it can at most reach an interactive Ask there, never an Allow.
///
/// Why the cd rule exists (verifier finding on 4af6b960): the cwd walk in
/// [`detect_bash`] assumes every `cd` SUCCEEDS and APPLIES to the next
/// segment, and rewrites the next `rm`'s relative operands against it (so by
/// the time [`worktree_confined`] runs, the operand is already absolute and the
/// relative spelling is gone — which is why this rule lives here, on the raw
/// command, and not there). At runtime neither assumption is guaranteed:
/// `cd <root>/s & rm -rf src` runs the `cd` in a background subshell,
/// `cd <root>/missing; rm -rf src` and `cd <root>/missing || rm -rf src`
/// run `rm` after a FAILED `cd`, and `cd X | rm -rf src` runs each side in its
/// own subshell. In every one of them `rm` deletes `<original cwd>/src`, which
/// was never judged. An absolute operand names the same path whatever the cwd
/// is, so it is immune to all four separators and to a failing `cd`. This
/// function does not try to tell `&&` apart from the others: blunt and closed
/// wins. The `&&`-only literal-directory shape is recognised separately and
/// strictly by [`literal_cd_and_rest`] (see the exception under THE cd RULE).
///
/// What this function actually guarantees, and nothing more: neither `rm` nor
/// `cd` can create, link or move a path, so no segment of an eligible command
/// can turn the judged root into something else before the `rm` runs, no
/// `cd` in it can move a relative `rm` operand away from where it was judged,
/// and no `rm` operand in it is spelled with a `..` whose lexical collapse
/// could disagree with the kernel's. It does NOT make `rm` safe by itself: every `rm` segment is still judged
/// per operand (the root itself stays a Deny, a symlink out is placed Outside
/// by operand canonicalisation). Anything else — `/bin/rm`, `command rm`,
/// `FOO=1 rm`, `rm -rf x 2>/dev/null`, `sudo rm` — is simply not eligible and
/// keeps the verdict it had before the worktree rule existed. There is no
/// branch that answers `true` for a command it did not fully read.
///
/// NOT covered: a DIFFERENT tool call (an earlier background job, a
/// concurrent session) that creates, moves or links something between this
/// judgement and the `rm` (cross-call TOCTOU); and anything an EARLIER tool
/// call already moved into a root (`mv ~/Documents <root>/x` in a previous
/// command, then `rm -rf <root>/x` here — `x` is now a real directory inside
/// the root, so it is Allow; a SYMLINK `<root>/x -> ~/Documents` is not,
/// because operand canonicalisation places it Outside).
pub(super) fn worktree_rm_eligible(command: &str) -> bool {
    let plain = |c: char| {
        c.is_alphanumeric()
            || matches!(
                c,
                ' ' | '/' | '.' | '_' | '-' | '+' | ',' | '=' | ':' | '@' | '%'
            )
    };
    if !command
        .chars()
        .all(|c| plain(c) || matches!(c, ';' | '&' | '|'))
    {
        return false;
    }
    let segments: Vec<Vec<&str>> = command
        .split([';', '&', '|'])
        .map(|seg| seg.split(' ').filter(|w| !w.is_empty()).collect())
        .collect();
    if !segments
        .iter()
        .all(|words| matches!(words.first().copied(), None | Some("rm" | "cd")))
    {
        return false;
    }
    // THE `..` RULE: no word of any `rm` segment may contain a `..` path
    // component. Checked here, on the raw spelling, because the cwd walk
    // rewrites operands and `scope` collapses `..` lexically BEFORE resolving
    // symlinks, so `<root>/w/lnk/../x` (lnk a symlink out) would be judged as
    // `<root>/w/x` while the kernel deletes `<lnk target's parent>/x`.
    if segments
        .iter()
        .filter(|w| w.first().copied() == Some("rm"))
        .any(|w| w[1..].iter().any(|word| has_dotdot_component(word)))
    {
        return false;
    }
    let has_cd = segments.iter().any(|w| w.first().copied() == Some("cd"));
    if !has_cd {
        return true;
    }
    segments
        .iter()
        .filter(|w| w.first().copied() == Some("rm"))
        .all(|w| rm_words_are_flags_then_absolute(&w[1..]))
}

/// Whole-command precondition for every deletion-principle verdict of
/// [`crate::deletion`] (backlog 3aa215e1): [`worktree_rm_eligible`] — bare
/// `rm`/`cd` segments of plain word characters, no `..` in any `rm` word, and
/// only absolute `rm` operands when a `cd` is present — AND, for every `rm`
/// segment:
///
///   * no word after `rm` has a component that is exactly `.` or `..`
///     (split on `/` and `=`): the spec withholds the Allow for any such
///     spelling, harmless or not;
///   * the words after `rm` are a run of option words followed only by
///     operands: no lone `-` (an operand named `-` that the arm's `-`-prefix
///     filter would drop), no `--`, and no option-looking word after the
///     first operand (BSD `rm` treats it as a FILE, which the arm's operand
///     list would never see).
///
/// Anything else is `false`, and the rm arm keeps its pre-existing verdict.
///
/// Like [`worktree_rm_eligible`], [`Ctx::new`] ALSO evaluates this on `REST`
/// alone for a line `cd D && REST` accepted by [`literal_cd_and_rest`], so a
/// relative `rm` operand there (already rewritten to `D/<operand>`) is eligible
/// exactly as its absolute spelling is.
pub(super) fn deletion_rm_eligible(command: &str) -> bool {
    if !worktree_rm_eligible(command) {
        return false;
    }
    command
        .split([';', '&', '|'])
        .map(|seg| {
            seg.split(' ')
                .filter(|w| !w.is_empty())
                .collect::<Vec<&str>>()
        })
        .filter(|w| w.first().copied() == Some("rm"))
        .all(|w| rm_words_are_plain_options_then_operands(&w[1..]))
}

/// See [`deletion_rm_eligible`]: options first, then operands; no `-`, no
/// `--`, no `.`/`..` component anywhere.
fn rm_words_are_plain_options_then_operands(words: &[&str]) -> bool {
    let mut in_options = true;
    for w in words {
        if w.split(['/', '=']).any(|c| c == "." || c == "..") {
            return false;
        }
        if *w == "-" || *w == "--" {
            return false;
        }
        if w.starts_with('-') {
            if !in_options {
                return false;
            }
            continue;
        }
        in_options = false;
    }
    true
}

/// True when `word` has a component that is exactly `..` (`..`, `../x`,
/// `a/../b`, `a/..`). Components are split on `/` and also on `=`, so an
/// attached option value (`--opt=../x`) counts too — over-matching here only
/// withholds the worktree `Allow`, which is the restrictive direction.
fn has_dotdot_component(word: &str) -> bool {
    word.split(['/', '=']).any(|c| c == "..")
}

/// The cd rule of [`worktree_rm_eligible`]: the words after `rm` are a run of
/// option words (start with `-`, length >= 2 -- a lone `-` is an operand)
/// followed ONLY by words that start with `/`. Once the first non-option word
/// is seen, every later word must be absolute too: BSD `rm` stops option
/// parsing at the first operand, so a later `-x` is a relative FILE there, and
/// after `--` every word is an operand on every `rm`.
fn rm_words_are_flags_then_absolute(words: &[&str]) -> bool {
    let mut in_options = true;
    for w in words {
        if in_options && w.len() >= 2 && w.starts_with('-') {
            if *w == "--" {
                in_options = false;
            }
            continue;
        }
        in_options = false;
        if !w.starts_with('/') {
            return false;
        }
    }
    true
}

/// True only when `operands` is non-empty and EVERY operand is a literal path
/// (no glob meta, touches no protected path) that [`SafeRoots::classify_worktree`]
/// places as [`Placement::Inside`] a worktree storage root.
///
/// It is also `false` whenever the top-level command is not eligible (see
/// [`worktree_rm_eligible`]).
///
/// Every other answer — an empty list, a glob, a protected path, `IsRoot`,
/// `Outside`, and `Undetermined` (unresolvable, no resolver, no roots, an
/// unknown `cd`) — is `false`, i.e. the caller keeps the verdict it had before
/// this rule existed. There is no default that answers `true`.
fn worktree_confined(ctx: &Ctx<'_>, operands: &[&str]) -> bool {
    if !ctx.worktree_rm_eligible || operands.is_empty() {
        return false;
    }
    // A `..` / `.` in the payload cwd is NOT checked here: by now `base` is
    // `SafeRoots::session_cwd`, already collapsed lexically and resolved, so
    // the raw spelling is gone. `SafeRoots::new` refuses it on the raw value
    // and `classify_worktree` then answers `Undetermined` for every operand.
    let base = ctx.base_for("rm");
    for operand in operands {
        if has_glob_meta(operand) {
            return false;
        }
        // A protected-NAMED operand is still refused here unless it is the
        // checkout's own file (`Ctx::in_worktree_checkout`, user ruling
        // 2026-10-03) — the same lift the protected-path precedence in
        // `analyze_rm` applies, so the two never disagree.
        if exclude::touches_protected(operand) && !ctx.in_worktree_checkout(operand) {
            return false;
        }
        match ctx.scope.classify_worktree(operand, base) {
            Determination::Known(Placement::Inside { .. }) => {}
            Determination::Known(Placement::IsRoot { .. } | Placement::Outside { .. })
            | Determination::Undetermined(_) => return false,
        }
    }
    true
}

/// The operands to hand a placement check, or `None` when at least one of them
/// cannot be placed at all.
///
/// A glob is never resolved by this crate, so a wildcard operand is placed by
/// the literal directory prefix it can only ever expand INSIDE (`target/*` ->
/// `target`). A wildcard with no literal prefix (`*`, `./*`) has no such
/// bound — `glob_literal_prefix` returns `None` — and the whole operand list is
/// then unplaceable, which leaves the caller's Deny standing.
fn placeable_targets<'a>(operands: &[&'a str]) -> Option<Vec<std::borrow::Cow<'a, str>>> {
    if operands.is_empty() {
        return None;
    }
    let mut out: Vec<std::borrow::Cow<'a, str>> = Vec::new();
    for o in operands {
        if has_glob_meta(o) {
            out.push(std::borrow::Cow::Owned(glob_literal_prefix(o)?));
        } else {
            out.push(std::borrow::Cow::Borrowed(*o));
        }
    }
    Some(out)
}

/// The whole-command eligibility of the two deletion-principle axes of
/// [`analyze_rm`], computed once by `Ctx::new` (on the whole line, and on the
/// `REST` of a [`literal_cd_and_rest`] line) and carried unchanged.
#[derive(Clone, Copy)]
pub(super) struct DeletionEligibility {
    /// [`deletion_rm_eligible`]: the literal-operand verdicts of
    /// [`crate::deletion`].
    pub(super) literal: bool,
    /// [`tmp_glob_rm_eligible`]: the `/tmp` wildcard `Allow` of
    /// [`tmp_glob_confined`].
    pub(super) tmp_glob: bool,
}

impl DeletionEligibility {
    /// `literal` is [`deletion_rm_eligible`] already combined by `eligible`;
    /// `eligible` combines [`tmp_glob_rm_eligible`] the same way.
    pub(super) fn of(literal: bool, eligible: impl Fn(fn(&str) -> bool) -> bool) -> Self {
        DeletionEligibility {
            literal,
            tmp_glob: eligible(tmp_glob_rm_eligible),
        }
    }
}

/// Whole-command precondition for the `/tmp` (and linked-worktree-checkout)
/// wildcard `Allow` ([`tmp_glob_confined`]): EXACTLY [`deletion_rm_eligible`] with `*` allowed
/// in one place only — the LAST path component of an `rm` OPERAND word
/// ([`last_component_glob`]: literal parent, no leading `.` in the pattern,
/// no other glob metacharacter). A `*` anywhere else (the verb, an option
/// word, a `cd` word, a non-last component) answers `false`. With every
/// such `*` stood in for by a plain character, the line must then pass
/// [`deletion_rm_eligible`] unchanged: bare `rm`/`cd` segments of plain word
/// characters (so no `?`, `[`, `{`, `$`, `~`, quote, backslash, …), no `.`/`..`
/// component, only absolute operands when a `cd` is present, options before
/// operands. There is no branch that answers `true` for a word it did not read.
pub(super) fn tmp_glob_rm_eligible(command: &str) -> bool {
    let stars_ok = command.split([';', '&', '|']).all(|seg| {
        let words: Vec<&str> = seg.split(' ').filter(|w| !w.is_empty()).collect();
        let is_rm = words.first().copied() == Some("rm");
        words.iter().enumerate().all(|(i, w)| {
            !w.contains('*')
                || (is_rm && i > 0 && !w.starts_with('-') && last_component_glob(w).is_some())
        })
    });
    stars_ok && deletion_rm_eligible(&command.replace('*', "x"))
}

/// `(parent, pattern)` for an operand whose wildcard is confined to its last
/// path component, or `None`. `None` for: no `/` (a bare pattern would be
/// placed only through the cwd), an empty parent (`/x*`) or pattern, any
/// glob metacharacter or backslash in the parent, a pattern with no `*`,
/// with a `?`/`[`/`]`/`{`/`}`/backslash, or starting with `.` (`.*` names
/// hidden entries, and on older bash `.` and `..` themselves).
fn last_component_glob(operand: &str) -> Option<(&str, &str)> {
    let (parent, pattern) = operand.rsplit_once('/')?;
    if parent.is_empty()
        || parent.contains(['*', '?', '[', ']', '{', '}', '\\'])
        || !pattern.contains('*')
        || pattern.starts_with('.')
        || pattern.contains(['?', '[', ']', '{', '}', '\\'])
    {
        return None;
    }
    Some((parent, pattern))
}

/// The `/tmp` wildcard `Allow` (user ruling 2026-11-02, backlog 486fb7de),
/// which since 0.2.287 also covers a wildcard whose parent is strictly inside
/// a linked worktree checkout (user ruling 2026-11-07; the name is kept):
/// true only when the line is [`tmp_glob_rm_eligible`], the session has no
/// deletion refusal, `operands` is non-empty, and EVERY operand is either
///
///   * a literal that [`crate::deletion::judge_operand`] answers `Allow` for
///     (exactly the literal rule), or
///   * a wildcard [`tmp_glob_operand_allowed`] accepts.
///
/// A wildcard expands only to entries of its literal parent directory (`*`
/// never crosses `/` and never matches a leading `.`), and a recursive `rm`
/// does not follow a symlink it deletes, so what such an operand destroys is
/// bounded by that real directory. Every other answer — including every
/// `Undetermined` placement — is `false`; the caller then keeps the verdict
/// it had before this rule existed. There is no default that answers `true`.
fn tmp_glob_confined(ctx: &Ctx<'_>, operands: &[&str]) -> bool {
    if !ctx.deletion_rm_eligible.tmp_glob
        || operands.is_empty()
        || ctx.scope.deletion_refusal().is_some()
    {
        return false;
    }
    let base = ctx.base_for("rm");
    operands.iter().all(|o| {
        if o.contains('*') || has_glob_meta(o) {
            tmp_glob_operand_allowed(ctx, o, base)
        } else {
            matches!(
                crate::deletion::judge_operand(ctx.scope, o, base),
                crate::deletion::OperandJudgement::Allow(_)
            )
        }
    })
}

/// Minimum length, in characters, of the literal head before the first `*`
/// of a wildcard placed directly in the `/tmp` root (user ruling 2026-11-05,
/// #1531). Deeper below `/tmp` there is no minimum.
const MIN_TMP_ROOT_GLOB_HEAD: usize = 4;

/// One wildcard operand of [`tmp_glob_confined`]. True only when
/// [`last_component_glob`] splits it into a literal parent and a `*` pattern,
/// the parent itself touches no protected path ([`exclude::touches_protected`]:
/// `<wt>/.githooks/pre-*` keeps its verdict even inside a checkout), and the
/// parent is placed in a managed disposable place:
///
///   * STRICTLY inside a linked worktree checkout under a worktree storage
///     root ([`SafeRoots::classify_worktree_checkout`] answers `Inside`: no
///     `..` in the spelling, no `.git` component below the checkout, no
///     symlink below the storage root, the parent itself included; user ruling
///     2026-11-07). The checkout itself (`IsRoot`) does not count. When an
///     ENTRY the pattern could match may touch a protected path
///     ([`exclude::glob_entry_may_touch_protected`] — a bare `*` always may,
///     since it is matched against every protected name, e.g.
///     `settings.local.json`), it is accepted only when
///     [`Ctx::in_worktree_checkout`] lifts the protected-path rule for the
///     parent (absolute spelling, one plain command line) — the lift a literal
///     operand inside the checkout gets (user ruling 2026-10-03); every entry
///     is strictly below the parent and so inside the same checkout. Otherwise:
///   * no entry the pattern could match touches a protected path, and
///     [`SafeRoots::classify_temp`] (the FIXED `/tmp`
///     roots only — never `$TMPDIR`, never a cache root; symlinks above the
///     anchor accepted, none below it, the parent itself included) places the
///     parent:
///     * `Inside` the real `/tmp` root — any such pattern; or
///     * `IsRoot` — only with a literal head of at least
///       [`MIN_TMP_ROOT_GLOB_HEAD`] characters before the first `*` (user
///       ruling 2026-11-05, #1531: `/tmp/name*` yes; `/tmp/*`, `/tmp/c*`,
///       `/tmp/cla*` never), and only when the parent is spelled as the
///       canonical root itself or a single top-level component (`/tmp`), so a
///       symlink below `/tmp` that resolves back to the root is refused.
///
/// `Outside` and every `Undetermined` (unresolvable, no resolver, a symlink
/// below the root, `..` left after normalisation) answer `false`.
fn tmp_glob_operand_allowed(ctx: &Ctx<'_>, operand: &str, base: Option<&str>) -> bool {
    let scope = ctx.scope;
    let Some((parent, pattern)) = last_component_glob(operand) else {
        return false;
    };
    if exclude::touches_protected(parent) {
        return false;
    }
    let entry_may_be_protected = exclude::glob_entry_may_touch_protected(parent, pattern);
    if matches!(
        scope.classify_worktree_checkout(parent, base),
        Determination::Known(Placement::Inside { .. })
    ) {
        return !entry_may_be_protected || ctx.in_worktree_checkout(parent);
    }
    if entry_may_be_protected {
        return false;
    }
    match scope.classify_temp(parent, base) {
        Determination::Known(Placement::Inside { .. }) => true,
        Determination::Known(Placement::IsRoot { root }) => {
            let has_head = pattern
                .find('*')
                .is_some_and(|i| pattern[..i].chars().count() >= MIN_TMP_ROOT_GLOB_HEAD);
            let spelled_as_root = match scope.lexical_absolute(parent, base) {
                Determination::Known(l) => {
                    l == root || l.strip_prefix('/').is_some_and(|t| !t.contains('/'))
                }
                Determination::Undetermined(_) => false,
            };
            has_head && spelled_as_root
        }
        Determination::Known(Placement::Outside { .. }) | Determination::Undetermined(_) => false,
    }
}
