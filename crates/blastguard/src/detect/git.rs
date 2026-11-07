use super::{
    analyze_shell_payload, confined_ask, has_short, irreversible_git_subcommand, is_short_flag,
    normalized_command, positional_operands, protected_disarm_deny, protected_glob_deny,
    protected_tree_deny, Ctx, Decision, Determination, VerdictAcc, MAX_SHELL_DEPTH,
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

/// Whole-command precondition for the git working-tree-discard `Allow` inside
/// a linked worktree checkout (user ruling 2026-11-07; see
/// [`managed_worktree_git`]). `Some(<the words after git, joined by one
/// space>)` only when the line is ONE plain `git` command:
///
///   * every character is a Unicode alphanumeric, space, or one of
///     `/ . _ - + , = : @ %` — no segment separator (`;`, `&`, `|`), quote,
///     `$`, backtick, parenthesis, brace, redirection, glob, `~`, `#`, `!`,
///     backslash, newline, tab or other control character. So no other
///     command on the line can create, move or link the checkout (or its
///     `.git` file) between this judgement and git running, and no shell
///     expansion decides an argument;
///   * the first word is exactly `git` (no `/usr/bin/git`, `command git`,
///     `env`, `sudo`, `VAR=x git`);
///   * no word has a component that is exactly `..` (split on `/` and `=`):
///     `..` is collapsed lexically by [`crate::scope`] while the kernel
///     resolves it physically.
///
/// [`Ctx::new`] also evaluates it on the `REST` of a `cd D && REST` line that
/// [`super::literal_cd_and_rest`] accepts (the `cd` neither creates nor links
/// anything, and `&&` guarantees `REST` runs in `D`), exactly like the other
/// whole-command eligibility predicates. Anything else is `None` and the git
/// arm keeps the verdict it had before the ruling. Over-matching here only
/// withholds the `Allow`.
pub(super) fn git_worktree_eligible(command: &str) -> Option<String> {
    let plain = |c: char| {
        c.is_alphanumeric()
            || matches!(
                c,
                ' ' | '/' | '.' | '_' | '-' | '+' | ',' | '=' | ':' | '@' | '%'
            )
    };
    if !command.chars().all(plain) {
        return None;
    }
    let words: Vec<&str> = command.split(' ').filter(|w| !w.is_empty()).collect();
    if words.first().copied() != Some("git") {
        return None;
    }
    if words.iter().any(|w| w.split(['/', '=']).any(|c| c == "..")) {
        return None;
    }
    Some(words[1..].join(" "))
}

/// Where git runs, as git's global options move it: `Known(Some(dir))` — the
/// lexically normalised absolute directory git's work tree is named by (the
/// `--work-tree` value when given, else the cumulative `-C` directory);
/// `Known(None)` — no global option moves it (git runs in `base`);
/// `Undetermined` — it cannot be named: a second `--work-tree`, a glued
/// `-C<dir>` (git rejects it, but no directory is named), a missing value, or
/// a value that is not a placeable literal path (a shell expansion, a
/// relative path with no known base, a residual `..`).
///
/// `-C` values apply cumulatively, as git applies them (a relative one is
/// relative to the previous); a relative `--work-tree` is relative to the
/// directory `-C` left git in.
fn git_moved_dir(
    scope: &crate::scope::SafeRoots,
    globals: &[&str],
    base: Option<&str>,
) -> Determination<Option<String>> {
    let mut cur: Option<String> = base.map(str::to_string);
    let mut moved = false;
    let mut work_tree: Option<&str> = None;
    let mut i = 0;
    while i < globals.len() {
        let t = globals[i];
        let value = if t == "-C" || t == "--work-tree" {
            let Some(v) = globals.get(i + 1) else {
                return Determination::undetermined(format!("git {t} has no value"));
            };
            i += 2;
            *v
        } else if let Some(v) = t.strip_prefix("--work-tree=") {
            i += 1;
            v
        } else {
            if t.starts_with("-C") && t.len() > 2 {
                return Determination::undetermined("glued git -C<dir> names no directory");
            }
            i += 1;
            continue;
        };
        if t == "-C" {
            cur = match scope.lexical_absolute(value, cur.as_deref()) {
                Determination::Known(d) => Some(d),
                Determination::Undetermined(u) => return Determination::Undetermined(u),
            };
            moved = true;
        } else if work_tree.replace(value).is_some() {
            return Determination::undetermined("git --work-tree given twice");
        }
    }
    if let Some(wt) = work_tree {
        return match scope.lexical_absolute(wt, cur.as_deref()) {
            Determination::Known(d) => Determination::known(Some(d)),
            Determination::Undetermined(u) => Determination::Undetermined(u),
        };
    }
    if moved {
        Determination::known(cur)
    } else {
        Determination::known(None)
    }
}

/// The git working-tree-discard `Allow` (user ruling 2026-11-07: work
/// products in a managed disposable place may be deleted): true ONLY when
///
///   * the top-level line is one plain git command
///     ([`git_worktree_eligible`]) and `rest` is exactly ITS words — a nested
///     git invocation (an alias body, a `bisect run git …`) is a different
///     word list and never qualifies;
///   * every global option before the subcommand is `-C <dir>` (at most
///     once), `--work-tree <dir>` / `--work-tree=<dir>` (at most once), or
///     absent. Any other global — `--git-dir` (repoints the repository away
///     from the checkout), `-c`/`--config-env` (`core.worktree`, aliases),
///     `--namespace`, `-p`, … — withholds the `Allow`;
///   * the directory git discovers its repository from (the `-C` directory,
///     else this segment's resolved working directory) is ITSELF a linked
///     worktree checkout directly under a worktree storage root, observed on
///     disk by [`crate::scope::SafeRoots::is_linked_worktree_checkout`] — a
///     subdirectory of a checkout, a primary working tree (`.git`
///     directory), the storage root and every probe failure do not qualify;
///   * a `--work-tree`, when given, resolves to that SAME checkout (another
///     work tree under the checkout's index would discard changes that are
///     not this checkout's).
///
/// Every other answer is `false`: the caller keeps the exact verdict it had.
fn managed_worktree_git(ctx: &Ctx<'_>, rest: &[&str], idx: usize) -> bool {
    if ctx.git_eligible_rest.as_deref() != Some(rest.join(" ").as_str()) {
        return false;
    }
    let globals = &rest[..idx];
    let mut c_count = 0;
    let mut i = 0;
    while i < globals.len() {
        match globals[i] {
            "-C" => {
                c_count += 1;
                i += 2;
            }
            "--work-tree" => i += 2,
            t if t.starts_with("--work-tree=") => i += 1,
            _ => return false,
        }
    }
    if c_count > 1 {
        return false;
    }
    let base = ctx.base_for("git");
    let discovery = match globals.iter().position(|t| *t == "-C") {
        Some(p) => match globals.get(p + 1) {
            Some(d) => match ctx.scope.lexical_absolute(d, base) {
                Determination::Known(d) => d,
                Determination::Undetermined(_) => return false,
            },
            None => return false,
        },
        None => match base {
            Some(b) => b.to_string(),
            None => return false,
        },
    };
    if !matches!(
        ctx.scope.is_linked_worktree_checkout(&discovery, None),
        Determination::Known(true)
    ) {
        return false;
    }
    match git_moved_dir(ctx.scope, globals, base) {
        Determination::Known(Some(work)) if work != discovery => {
            matches!(
                ctx.scope.is_linked_worktree_checkout(&work, None),
                Determination::Known(true)
            ) && ctx.scope.resolve(&work).is_some()
                && ctx.scope.resolve(&work) == ctx.scope.resolve(&discovery)
        }
        Determination::Known(_) => true,
        Determination::Undetermined(_) => false,
    }
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

/// Reason for the head-brace Ask: an unexpanded brace list among the words up
/// to and including the git subcommand word.
const HEAD_BRACE_REASON: &str = "blastguard cannot determine which git subcommand this runs: \
bash brace expansion (a comma list or a `..` range) in the words up to the subcommand can build \
a different subcommand and arguments (e.g. `git {stash,push} -u` runs `git stash push -u`, \
which removes untracked files from the working tree)";

/// Backlog debc6df3: the words from `git`'s first argument up to and including
/// the subcommand word (global options and their values included; every word
/// when there is no subcommand) are tested with [`has_brace_expansion`]. A
/// brace list there can BUILD the subcommand (`git {stash,push} -u`,
/// `git {,stash} push -u`), so the per-subcommand arms would classify a word
/// git never sees. Such a line is never Allowed: an Allow from the analysis is
/// raised to Ask; a Deny/Ask from it stands (the stricter verdict wins).
/// Braces in LATER words (`git stash pop stash@{0}`) are not part of this
/// test.
///
/// `seg` is the raw segment `rest` was whitespace-split from, when known; it is
/// handed to the whole-tree-staging check only (see [`super::git_stage`]).
pub(super) fn analyze_git(rest: &[&str], seg: Option<&str>, ctx: &Ctx<'_>) -> Decision {
    let head_end = git_subcommand_index(rest).map_or(rest.len(), |i| i + 1);
    let head_brace = rest[..head_end].iter().any(|w| has_brace_expansion(w));
    let verdict = analyze_git_words(rest, seg, ctx);
    if head_brace && matches!(verdict, Decision::Allow) {
        return Decision::ask(HEAD_BRACE_REASON);
    }
    verdict
}

/// An exec-capable git shape found on a command line: why it is refused, and
/// the command line(s) git would run when they are spelled out on the line
/// (empty when the command is only named by reference, e.g. an env var).
pub(super) struct GitExecBan {
    pub(super) reason: String,
    pub(super) embedded: Vec<String>,
}

/// fb338793: when an exec-capable git shape spells out the command it would
/// run and that command is itself refused (`git bisect run rm -rf /usr`), that
/// Deny — with its own reason and system-harm marker — is returned instead of
/// the generic exec ban. `None` = no such embedded Deny (the ban, if any, is
/// still applied by `analyze_git_words`).
pub(super) fn git_exec_ban_embedded_deny(
    rest: &[&str],
    depth: usize,
    ctx: &Ctx<'_>,
) -> Option<Decision> {
    let ban = git_exec_ban(rest)?;
    if depth >= MAX_SHELL_DEPTH {
        return None;
    }
    ban.embedded
        .iter()
        .find_map(|line| match analyze_shell_payload(line, depth, ctx) {
            deny @ Decision::Deny(_) => Some(deny),
            _ => None,
        })
}

/// The shared tail of every exec-ban reason. It claims the shapes blastguard
/// RECOGNISES, not every shape: git also runs commands from settings this ban
/// does not model (e.g. `PAGER`/`EDITOR`, `sendemail.smtpServer` naming a
/// program, `instaweb --httpd`, config already stored in a file), which keep
/// their ordinary analysis — see docs/specs/blastguard.md.
const EXEC_BAN_TAIL: &str = "Every git shape blastguard recognises as running a \
command of the caller's choosing is refused outright, whatever that command is (user ruling \
2026-11-07) — run the command directly, where blastguard judges it, instead of through git";

/// The ban reason for an ENVIRONMENT variable that makes git run a command
/// (`GIT_PAGER=…`, `GIT_SSH=…`, `GIT_CONFIG_KEY_<n>`/`GIT_CONFIG_VALUE_<n>`,
/// `GIT_CONFIG_PARAMETERS`; backlog 3c750df2). `what` names the variable(s).
pub(super) fn env_exec_ban_reason(what: &str) -> String {
    format!("`{what}` makes git run a command. {EXEC_BAN_TAIL}")
}

fn exec_ban(shape: &str, embedded: Option<String>) -> Option<GitExecBan> {
    Some(GitExecBan {
        reason: format!("`git {shape}` makes git run a command. {EXEC_BAN_TAIL}"),
        embedded: embedded.into_iter().filter(|e| !e.is_empty()).collect(),
    })
}

/// git's `ext::<command>` remote transport (backlog 3c750df2): git runs
/// `<command>` itself to talk to the "remote". Banned whenever a URL-shaped
/// word — a remote argument, an `--opt=ext::…` value, a config value or a
/// `url.ext::….insteadOf` key — starts with `ext::`, whatever
/// `protocol.ext.allow` says (the line can set that too).
fn ext_transport_ban(shape: &str, words: &[&str]) -> Option<GitExecBan> {
    words.iter().find_map(|w| {
        let lowered = w.to_ascii_lowercase();
        let value = lowered.split_once('=').map_or(lowered.as_str(), |(_, v)| v);
        let at = [lowered.as_str(), value]
            .into_iter()
            .find_map(|s| s.strip_prefix("url.").unwrap_or(s).strip_prefix("ext::"))?;
        let original = &w[w.len() - at.len()..];
        exec_ban(shape, Some(original.replace("% ", " ")))
    })
}

/// `words` (already shell-unquoted argv) re-joined as one shell line that
/// splits back into the same words. A word holding whitespace or a shell
/// metacharacter is single-quoted; any other word is left bare, so an
/// originally unquoted `~`, `*` or `$X` keeps its (restrictive) expansion
/// reading. Only used for the embedded line, which can add a Deny reason to
/// a ban that is already a Deny.
fn shell_join(words: &[String]) -> String {
    words
        .iter()
        .map(|w| {
            let plain = !w.is_empty()
                && !w.chars().any(|c| {
                    c.is_whitespace()
                        || matches!(
                            c,
                            ';' | '&' | '|' | '<' | '>' | '(' | ')' | '\'' | '"' | '\\' | '`'
                        )
                });
            if plain {
                w.clone()
            } else {
                format!("'{}'", w.replace('\'', r"'\''"))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// True when `t` is a spelling git's option parser accepts for the long
/// option `full` (with or without `=value`): the exact name or an
/// abbreviation of at least `min` characters (git accepts any unambiguous
/// prefix; `min` is chosen so the prefix is unambiguous for the subcommands
/// it is checked on).
fn long_opt(t: &str, full: &str, min: usize) -> bool {
    let name = t.split_once('=').map_or(t, |(n, _)| n);
    name.len() >= min && full.starts_with(name)
}

/// The value of the option at `args[i]`: the `=`-joined value, else the next
/// word.
fn long_value(args: &[String], i: usize) -> Option<String> {
    match args[i].split_once('=') {
        Some((_, v)) => Some(v.to_string()),
        None => args.get(i + 1).cloned(),
    }
}

/// In a short-option bundle `args[i]` (`-ix`, `-x`, `-xCMD`), whether the
/// letter `letter` is present before any letter in `stops` (a value-taking
/// option, whose glued value is not more letters). `Some(value)` when found:
/// the glued remainder, else the next word.
fn short_opt(args: &[String], i: usize, letter: char, stops: &str) -> Option<Option<String>> {
    let t = args[i].as_str();
    if !t.starts_with('-') || t.starts_with("--") || t.len() < 2 {
        return None;
    }
    let body = &t[1..];
    for (pos, c) in body.char_indices() {
        if c == letter {
            let glued = &body[pos + c.len_utf8()..];
            return Some(if glued.is_empty() {
                args.get(i + 1).cloned()
            } else {
                Some(glued.to_string())
            });
        }
        if stops.contains(c) {
            return None;
        }
    }
    None
}

/// Exec-capable git shapes, BANNED outright (backlog fb338793, user rulings
/// 2026-11-07: 「git bisectでいろいろ構文を処理できてしまうくらいなら使用を
/// 禁止した方がいいのでは?」 — every git subcommand or option that executes an
/// arbitrary command is Deny regardless of the embedded command, so `git bisect
/// run true` is Deny). `None` for every other git line, which the
/// per-subcommand arms judge as before.
///
/// Before this, `analyze_git_words` ended in `_ => Decision::Allow`, so an
/// unmodelled subcommand never had its embedded command analysed: `git bisect
/// run rm -rf /usr` was Allow while `rm -rf /usr` is Deny.
///
/// Banned (the words are shell-unquoted first, so `-c 'alias.x=!…'` and
/// `-c alias.x='!…'` are the same word):
///   * global `-c` setting a command-valued key
///     ([`super::executors::git_config_command`]: `core.pager`,
///     `core.sshCommand`, `core.editor`, `sequence.editor`, `diff.external`,
///     `pager.<cmd>`, `alias.<x>=!…`, `credential.helper`, …), and the same
///     keys written by `git config [scope] <key> <value>` /
///     `git config set <key> <value>` or set by `git clone -c`/`--config`;
///   * `bisect run`; `rebase`/`pull` `-x`/`--exec`; `submodule foreach`;
///   * `difftool`/`mergetool` `-x`/`--extcmd`; `grep -O<cmd>` /
///     `--open-files-in-pager=<cmd>` (a bare `-O` / `--open-files-in-pager`
///     runs the configured pager and is not banned);
///   * `--upload-pack`/`--receive-pack`/`--exec` on
///     `fetch`/`pull`/`clone`/`ls-remote`/`push`/`archive`/`fetch-pack`/`send-pack`,
///     and `-u` on `clone`/`ls-remote`;
///   * `send-email` `--to-cmd`/`--cc-cmd`/`--header-cmd`/`--sendmail-cmd`;
///   * the `ext::<command>` transport ([`ext_transport_ban`]) as a remote
///     argument of `fetch`/`pull`/`push`/`clone`/`ls-remote`/`remote`/`config`/
///     `archive`/`fetch-pack`/`send-pack`/`submodule`, or in a `-c` key/value.
///
/// The words are decoded with bash's quoting rules, ANSI-C `$'…'` included;
/// a `$'…'` in git's global options that cannot be decoded is banned. The same
/// ban is applied to git's ENVIRONMENT (`GIT_PAGER`, `GIT_SSH`, …,
/// `GIT_CONFIG_KEY_<n>`/`GIT_CONFIG_VALUE_<n>`, `GIT_CONFIG_PARAMETERS`) by
/// `executors::env_exec_var_verdict`. Not modelled (their commands keep the
/// ordinary analysis only): `PAGER`/`EDITOR`/`VISUAL` (honoured by git, but
/// not git-specific), `sendemail.smtpServer` naming a program, `instaweb
/// --httpd`, and settings already stored in a config file.
///
/// `filter-branch` is not here: [`irreversible_git_subcommand`] already
/// denies every use of it. Plain `bisect start/good/bad`, `rebase <upstream>`,
/// `-c pager.log=false`, an empty value (`-c credential.helper=`) and a
/// non-`!` alias run no caller-chosen command and are not banned.
///
/// Known-safe values are not banned (user ruling 2026-11-07, "Allow
/// known-safe values"): a command-valued key set to a value on the fixed list
/// of `executors::is_safe_command_value` — pager `less`/`more`/`cat`/`delta`,
/// editor `vim`/`vi`/`nvim`/`emacs`/`nano`/`code`/`cursor`/`subl`/`true`/`:`,
/// credential helper `osxkeychain`/`store`/`cache`/`manager`/`manager-core`,
/// `gpg.program` `gpg`/`gpg2`, `core.sshCommand` `ssh` with plain flags (never
/// `-o`/`-F`/`-I`/`-E`/`-S`) — each with flag-only arguments.
/// Any other value stays banned. `git --config-env <key>=VAR` keeps the Ask
/// `judge_git_config` gives it (the value is in the environment; pinned by a
/// pre-existing control).
pub(super) fn git_exec_ban(rest: &[&str]) -> Option<GitExecBan> {
    use super::executors::git_config_command;
    // Backlog 3c750df2 (h3): bash's ANSI-C quoting is decoded
    // (`git -c $'core.pager=rm -rf ~' log` is `-c core.pager=rm -rf ~`).
    // A line the full decoder cannot read (a `$(`, a backquote, …) falls back
    // to one-level unquoting, and a `$'…'` it could not decode in git's
    // global options is refused below (fail closed).
    let joined = rest.join(" ");
    let (words, decoded): (Vec<String>, bool) = match stash_argv_words(&joined) {
        Determination::Known(w) => (w, true),
        Determination::Undetermined(_) => (
            super::quote_aware_words(&joined)
                .iter()
                .map(|w| super::first_shell_word(w).unwrap_or_else(|| w.clone()))
                .collect(),
            false,
        ),
    };
    let refs: Vec<&str> = words.iter().map(String::as_str).collect();
    let idx = git_subcommand_index(&refs);
    let globals = &refs[..idx.unwrap_or(refs.len())];
    if !decoded && globals.iter().any(|g| g.contains("$'")) {
        return Some(GitExecBan {
            reason: format!(
                "`git` global options use `$'…'` quoting blastguard cannot decode, so it cannot \
tell whether they set a command git runs. {EXEC_BAN_TAIL}"
            ),
            embedded: Vec::new(),
        });
    }
    // Global config: `-c k=v`, `-ck=v`. (`--config-env k=VAR` takes the value
    // from the environment; it keeps the Ask `judge_git_config` gives it —
    // see the doc comment above.)
    let mut i = 0;
    while i < globals.len() {
        let t = globals[i];
        let (assignment, step) = if t == "-c" {
            (globals.get(i + 1).copied(), 2)
        } else if let Some(v) = t.strip_prefix("-c").filter(|v| !v.is_empty()) {
            (Some(v), 1)
        } else if matches!(
            t,
            "-C" | "--git-dir" | "--work-tree" | "--namespace" | "--super-prefix" | "--config-env"
        ) {
            (None, 2)
        } else {
            (None, 1)
        };
        if let Some((key, value)) = assignment.and_then(|a| a.split_once('=')) {
            let key = key.trim().to_ascii_lowercase();
            if let Some(ban) = ext_transport_ban(&format!("-c {key}=…"), &[&key, value]) {
                return Some(ban);
            }
            if let Some(cmd) = git_config_command(&key, value) {
                return exec_ban(&format!("-c {key}=…"), Some(cmd));
            }
        }
        i += step;
    }
    let idx = idx?;
    let sub = normalized_command(refs[idx]);
    let args: Vec<String> = words[idx + 1..].to_vec();
    if matches!(
        sub.as_str(),
        "fetch"
            | "pull"
            | "push"
            | "clone"
            | "ls-remote"
            | "remote"
            | "config"
            | "archive"
            | "fetch-pack"
            | "send-pack"
            | "submodule"
    ) {
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        if let Some(ban) = ext_transport_ban(&format!("{sub} ext::…"), &arg_refs) {
            return Some(ban);
        }
    }
    let rebase_exec = |args: &[String]| -> Option<GitExecBan> {
        for i in 0..args.len() {
            if long_opt(&args[i], "--exec", 4) {
                return exec_ban(&format!("{sub} --exec"), long_value(args, i));
            }
            if let Some(v) = short_opt(args, i, 'x', "sSXCjo") {
                return exec_ban(&format!("{sub} -x"), v);
            }
        }
        None
    };
    let pack_exec = |args: &[String], short_u: bool, stops: &str| -> Option<GitExecBan> {
        for i in 0..args.len() {
            let t = args[i].as_str();
            if t == "--" {
                break;
            }
            for (full, min) in [("--upload-pack", 5), ("--receive-pack", 6), ("--exec", 4)] {
                if long_opt(t, full, min) {
                    return exec_ban(&format!("{sub} {full}"), long_value(args, i));
                }
            }
            if short_u {
                if let Some(v) = short_opt(args, i, 'u', stops) {
                    return exec_ban(&format!("{sub} -u"), v);
                }
            }
        }
        None
    };
    match sub.as_str() {
        "bisect" if args.first().map(String::as_str) == Some("run") => {
            // `bisect run` execs its words as argv (no shell re-split), so the
            // embedded line re-quotes each word: `sh -c 'cd /etc && rm passwd'`
            // must reach the analyser as that, not as `sh -c cd /etc && …`.
            exec_ban("bisect run", Some(shell_join(&args[1..])))
        }
        "rebase" => rebase_exec(&args),
        "pull" => rebase_exec(&args).or_else(|| pack_exec(&args, false, "")),
        "submodule" => {
            let pos = args.iter().position(|a| !a.starts_with('-'))?;
            if args[pos] != "foreach" {
                return None;
            }
            let cmd: Vec<&str> = args[pos + 1..]
                .iter()
                .map(String::as_str)
                .skip_while(|a| a.starts_with('-'))
                .collect();
            exec_ban("submodule foreach", Some(cmd.join(" ")))
        }
        "difftool" | "mergetool" => (0..args.len()).find_map(|i| {
            if long_opt(&args[i], "--extcmd", 5) {
                return exec_ban(&format!("{sub} --extcmd"), long_value(&args, i));
            }
            short_opt(&args, i, 'x', "t").and_then(|v| exec_ban(&format!("{sub} -x"), v))
        }),
        "grep" => {
            let mut i = 0;
            while i < args.len() {
                let t = args[i].as_str();
                if t == "--" {
                    break;
                }
                // The pager value is OPTIONAL and only ever glued (`-O<cmd>`,
                // `--open-files-in-pager=<cmd>`): the next word is never it.
                // Bare `-O` / `--open-files-in-pager` runs the configured
                // pager, not a caller-chosen command, and is not banned (user
                // ruling 2026-11-07).
                if long_opt(t, "--open-files-in-pager", 6) {
                    match t.split_once('=') {
                        Some((_, v)) if !v.is_empty() => {
                            return exec_ban("grep --open-files-in-pager", Some(v.to_string()))
                        }
                        _ => {}
                    }
                }
                if let Some(glued) = t.strip_prefix('-').filter(|_| !t.starts_with("--")) {
                    for (pos, c) in glued.char_indices() {
                        if c == 'O' {
                            let v = &glued[pos + 1..];
                            if v.is_empty() {
                                break;
                            }
                            return exec_ban("grep -O", Some(v.to_string()));
                        }
                        if "efABCm".contains(c) {
                            break;
                        }
                    }
                }
                // A pattern/file given in separate form is not an option.
                i += if matches!(t, "-e" | "-f") { 2 } else { 1 };
            }
            None
        }
        "clone" => {
            if let Some(ban) = pack_exec(&args, true, "obcj") {
                return Some(ban);
            }
            for i in 0..args.len() {
                let t = args[i].as_str();
                let assignment = if t == "-c" || t == "--config" {
                    args.get(i + 1).cloned()
                } else if let Some(v) = t.strip_prefix("--config=") {
                    Some(v.to_string())
                } else {
                    t.strip_prefix("-c")
                        .filter(|v| !v.is_empty())
                        .map(str::to_string)
                };
                if let Some((key, value)) = assignment.as_deref().and_then(|a| a.split_once('=')) {
                    let key = key.trim().to_ascii_lowercase();
                    if let Some(cmd) = git_config_command(&key, value) {
                        return exec_ban(&format!("clone -c {key}=…"), Some(cmd));
                    }
                }
            }
            None
        }
        "ls-remote" => pack_exec(&args, true, "o"),
        "fetch" | "push" | "archive" | "fetch-pack" | "send-pack" => pack_exec(&args, false, ""),
        "send-email" => (0..args.len()).find_map(|i| {
            [
                ("--to-cmd", 7),
                ("--cc-cmd", 7),
                ("--header-cmd", 10),
                ("--sendmail-cmd", 12),
            ]
            .into_iter()
            .find(|(full, min)| long_opt(&args[i], full, *min))
            .and_then(|(full, _)| exec_ban(&format!("send-email {full}"), long_value(&args, i)))
        }),
        "config" => git_config_write_exec(&args),
        _ => None,
    }
}

/// `git config` writing a command-valued key (`git config [--global]
/// alias.x '!…'`, `git config set core.pager …`): the stored command runs on a
/// later, innocent-looking git call, so the write is banned like the `-c`
/// form. Reads, `--unset*`, section renames/removals and any key that runs
/// nothing are `None`.
fn git_config_write_exec(args: &[String]) -> Option<GitExecBan> {
    const VALUE_FLAGS: &[&str] = &[
        "-f",
        "--file",
        "--blob",
        "--type",
        "--default",
        "--comment",
        "--value",
    ];
    const NOT_A_WRITE: &[&str] = &[
        "--get",
        "--get-all",
        "--get-regexp",
        "--get-urlmatch",
        "--get-color",
        "--get-colorbool",
        "--list",
        "-l",
        "--unset",
        "--unset-all",
        "--rename-section",
        "--remove-section",
        "--edit",
        "-e",
    ];
    let mut positional: Vec<&str> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let t = args[i].as_str();
        if NOT_A_WRITE.contains(&t) {
            return None;
        }
        if VALUE_FLAGS.contains(&t) {
            i += 2;
            continue;
        }
        if !t.starts_with('-') {
            positional.push(t);
        }
        i += 1;
    }
    let (key, value) = match positional.as_slice() {
        ["set", key, value, ..] => (*key, *value),
        [verb, ..]
            if matches!(
                *verb,
                "get" | "list" | "unset" | "rename-section" | "remove-section" | "edit"
            ) =>
        {
            return None
        }
        [key, value, ..] => (*key, *value),
        _ => return None,
    };
    let key = key.to_ascii_lowercase();
    let cmd = super::executors::git_config_command(&key, value)?;
    exec_ban(&format!("config {key} …"), Some(cmd))
}

fn analyze_git_words(rest: &[&str], seg: Option<&str>, ctx: &Ctx<'_>) -> Decision {
    // Backlog fb338793: exec-capable shapes never reach the per-subcommand
    // arms (and so never the `_ => Allow` at the end). See `git_exec_ban`.
    if let Some(ban) = git_exec_ban(rest) {
        return Decision::deny(ban.reason);
    }
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
    // User ruling 2026-11-07: discarding working-tree changes of a linked
    // worktree checkout under a worktree storage root (a managed disposable
    // place) is Allow. Consulted only by the working-tree-discard arms below;
    // the object-store rule right after this, `stash clear/drop` and every
    // protected-path check are unaffected. See `managed_worktree_git`.
    let managed = managed_worktree_git(ctx, rest, idx);
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
            // The pathspec is relative to where git RUNS, which `-C` /
            // `--work-tree` move (they used to be ignored here, so
            // `git -C /elsewhere clean -fd` was judged against the session
            // cwd). A directory they name that cannot be placed is
            // UNDETERMINED: the confinement is then judged on this segment's
            // own base exactly as before (the old confined Ask inside a safe
            // root, the old Deny elsewhere; never an Allow, `managed` is false
            // for it), and an Ask from it states that the directory git runs
            // in was NOT placed, so its "resolved strictly inside" wording is
            // not taken as covering it.
            let (clean_ctx, unplaced) =
                match git_moved_dir(ctx.scope, &rest[..idx], ctx.base_for("git")) {
                    Determination::Known(Some(dir)) => (ctx.after_cd(&dir), None),
                    Determination::Known(None) => (ctx.clone(), None),
                    Determination::Undetermined(why) => {
                        (ctx.clone(), Some(why.reason().as_str().to_string()))
                    }
                };
            let clean_confined = clean_ctx.confined_root_or_root_itself("git", &clean_targets);
            let confined_ask = |action: &str, root: &str| match (
                confined_ask(action, root, &clean_targets),
                &unplaced,
            ) {
                (Decision::Ask(reason), Some(why)) => Decision::Ask(format!(
                    "{reason} NOTE: the directory git -C / --work-tree names could not be placed \
({why}); the targets were judged against the current directory instead, so where git will \
actually delete is undetermined."
                )),
                (d, _) => d,
            };
            if has_f && managed {
                Decision::Allow
            } else if has_f && (has_d || has_x) {
                match &clean_confined {
                    Some(root) => confined_ask("git clean -f with -d/-x", root),
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
                    Some(root) => confined_ask("git clean -f", root),
                    None => Decision::deny("git clean -f deletes untracked files irreversibly"),
                }
            } else {
                Decision::Allow
            }
        }
        "reset" => {
            if rest.contains(&"--hard") && !managed {
                Decision::deny("git reset --hard discards working-tree changes")
            } else {
                Decision::Allow
            }
        }
        "checkout" => {
            if (rest.contains(&"--force") || has_short(rest, 'f')) && !managed {
                return Decision::deny("git checkout --force discards working-tree changes");
            }
            // Round 3, MIRROR GAP. The `restore` arm below denies
            // `git restore <path>` UNCONDITIONALLY (outside a managed linked
            // worktree checkout, see `managed_worktree_git`), because it
            // overwrites the working tree from the index and throws away
            // uncommitted work.
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
            // Inside a linked worktree checkout every remaining form (`--force`,
            // `-- .`, `.`, `-- <path>`) only discards that checkout's own
            // working-tree changes.
            if managed {
                return Decision::Allow;
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
            if (rest.contains(&"--force")
                || rest.contains(&"--discard-changes")
                || has_short(rest, 'f'))
                && !managed
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
            // and leaves the working tree intact, so it stays allowed. Inside a
            // managed linked worktree checkout every form is allowed (user
            // ruling 2026-11-07, `managed_worktree_git`).
            let staged = rest.contains(&"--staged") || has_short(rest, 'S');
            let worktree = rest.contains(&"--worktree") || has_short(rest, 'W');
            if (staged && !worktree) || managed {
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
                    // The brace test covers EVERY word, message values
                    // included: bash splits a brace list in any position
                    // (`-m {a,-u}` becomes `-m a -u`), so a message value is
                    // no exemption. Only the sweep-flag test skips the
                    // separate-word -m/--message value (a literal `-u`
                    // there is the message git stores, not a flag).
                    let any_brace = words.iter().any(|w| has_brace_expansion(w));
                    let outside = stash_words_outside_message(words);
                    // The sweep only parks the checkout's untracked files in
                    // a stash entry; inside a managed checkout that is Allow.
                    if outside.iter().any(|w| is_stash_sweep_flag(w)) && !managed {
                        Decision::deny(SWEEP_REASON)
                    } else if any_brace {
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
        // Backlog 43034f75: force-deleting a branch / force-removing a
        // worktree. Judged in `git_unmerged`; every cannot-determine state
        // there is Ask.
        "branch" => super::git_unmerged::branch_verdict(ctx, &rest[..idx], &rest[idx + 1..]),
        "worktree" => super::git_unmerged::worktree_verdict(ctx, &rest[..idx], &rest[idx + 1..]),
        // Backlog e9325654: whole-tree staging sweeps in other sessions' WIP.
        // Judged in `git_stage` from the argv alone (any repository); every
        // cannot-determine state there is Ask. `stage` is git's synonym for
        // `add`.
        "add" | "stage" => super::git_stage::add_verdict(&rest[idx + 1..], seg),
        "commit" => super::git_stage::commit_verdict(&rest[idx + 1..], seg),
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

/// True when the word MAY hold a bash brace expansion (comma list or `..`
/// sequence), which can turn into a sweep flag at runtime.
///
/// `w` is a word after quote removal (`stash_argv_words`), so a quoted or
/// escaped `}` (raw `{-mx\},-u}` or `{-mx'}',-u}`) is indistinguishable here
/// from a real one; pairing the first `{` with the first `}` would let
/// bash's real expansion (`-mx}` and `-u`) through. The test is therefore
/// deliberately over-approximate, resolving to the restrictive side: true
/// when a `{` is followed ANYWHERE later in the word by a `,` or `..` (no
/// brace pairing at all), or when the last `{` has no `}` after it
/// (unbalanced: not provably literal). Otherwise false, so `stash@` followed
/// by `{0}` stays literal. This may also flag words bash would not expand
/// (a quoted `{`, or `{x},y`); that over-flagging is the intended side.
///
/// The caller applies it to EVERY stash argv word, including the value of
/// `-m` / `--message` and the glued `--message=` / `-mVALUE` forms: bash
/// splits a brace list wherever it stands (`-m {a,-u}` runs as `-m a -u`),
/// and quote removal has already erased whether the brace was quoted. So a
/// quoted message such as `-m "fix {a,b}"` is deliberately put to a human
/// (Ask) too.
fn has_brace_expansion(w: &str) -> bool {
    let Some(first_open) = w.find('{') else {
        return false;
    };
    let after_first = &w[first_open + 1..];
    if after_first.contains(',') || after_first.contains("..") {
        return true;
    }
    // Unbalanced: the last `{` has no `}` after it.
    let last_open = w.rfind('{').unwrap_or(first_open);
    !w[last_open + 1..].contains('}')
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
