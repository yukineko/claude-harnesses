//! Git subcommands the write-target table has no row for (backlog 5c848034).
//!
//! The table in `write_targets.rs` names the verbs whose write target is not
//! `--output`. Every git subcommand outside it used to be read for `--output`
//! alone, so `git merge-file <protected>`, `git mailsplit -o<dir>`, `git
//! fast-export --export-marks=F` and `git credential-store --file=F` (each
//! observed writing under a fake `~/.claude`) reached `Allow`: a list of "the
//! verbs that write" is a mirror that is always one verb behind.
//!
//! The direction is inverted. A subcommand is judged only by what the table
//! KNOWS about it:
//!
//! * [`is_ordinary`] — the everyday porcelain whose operands are revisions,
//!   refs and pathspecs of the repository git already manages (`status`,
//!   `add`, `commit`, `merge`, …). Their operands are not output paths, and
//!   judging them would Ask on ordinary repository work.
//! * everything else (plumbing, helpers, an alias name, a verb this table has
//!   never heard of) is UNMODELLED: every word that could be a path is judged
//!   and a hit on a protected / system path is an Ask, never a guess that
//!   nothing is written there.
//!
//! [`resolve_alias`] reads `-c alias.X=VALUE` the way git does, so the verb an
//! alias stands for is judged under its own name.

use super::super::super::git::git_global_config_assignments;
use super::super::super::words::first_shell_word;
use crate::detect::quote_aware_words;

/// Subcommands whose operands are repository state (revisions, refs,
/// pathspecs), not a path git writes to. Anything not listed is unmodelled.
/// Subcommands with their own arm in `git_write_targets` or in `git.rs`
/// (`init`, `clone`, `worktree`, `config`, `bundle`, `checkout-index`,
/// `format-patch`, `archive`, `apply`) never reach the unmodelled judgement.
const ORDINARY: &[&str] = &[
    "status",
    "log",
    "diff",
    "show",
    "add",
    "stage",
    "rm",
    "mv",
    "commit",
    "merge",
    "rebase",
    "branch",
    "checkout",
    "switch",
    "restore",
    "reset",
    "stash",
    "fetch",
    "pull",
    "push",
    "tag",
    "remote",
    "blame",
    "grep",
    "ls-files",
    "ls-tree",
    "rev-parse",
    "rev-list",
    "describe",
    "cherry-pick",
    "revert",
    "cat-file",
    "diff-tree",
    "diff-index",
    "diff-files",
    "show-ref",
    "symbolic-ref",
    "update-ref",
    "shortlog",
    "reflog",
    "gc",
    "prune",
    "fsck",
    "count-objects",
    "notes",
    "am",
    "clean",
    "sparse-checkout",
    "bisect",
    "range-diff",
    "cherry",
    "name-rev",
    "merge-base",
    "for-each-ref",
    "ls-remote",
    "hash-object",
    "write-tree",
    "read-tree",
    "update-index",
    "check-ignore",
    "check-attr",
    "whatchanged",
    "show-branch",
    "version",
    "help",
    "rerere",
];

/// Subcommands that have a table row elsewhere (their targets are read by a
/// dedicated arm), so the unmodelled judgement must not repeat them.
const MODELLED: &[&str] = &[
    "init",
    "clone",
    "worktree",
    "config",
    "bundle",
    "checkout-index",
    "format-patch",
    "archive",
    "apply",
];

/// Subcommands that only READ: they write neither the work tree nor a named
/// path (a `--output` option is read by the generic scan, not here). Every
/// read-only subcommand belongs here, so reading a protected file in the
/// repository's own work tree is never an Ask (`git annotate .claude/…`).
const READ_ONLY: &[&str] = &[
    "status",
    "log",
    "diff",
    "show",
    "blame",
    "annotate",
    "grep",
    "ls-files",
    "ls-tree",
    "rev-parse",
    "rev-list",
    "describe",
    "cat-file",
    "diff-tree",
    "diff-index",
    "diff-files",
    "show-ref",
    "shortlog",
    "reflog",
    "fsck",
    "count-objects",
    "name-rev",
    "merge-base",
    "merge-tree",
    "for-each-ref",
    "ls-remote",
    "check-ignore",
    "check-attr",
    "check-mailmap",
    "check-ref-format",
    "whatchanged",
    "show-branch",
    "cherry",
    "range-diff",
    "patch-id",
    "show-index",
    "stripspace",
    "fmt-merge-msg",
    "column",
    "var",
    "version",
    "help",
    "verify-commit",
    "verify-tag",
    "verify-pack",
    "get-tar-commit-id",
    "request-pull",
];

/// True when `sub` only reads (see [`READ_ONLY`]).
pub(super) fn is_read_only(sub: &str) -> bool {
    READ_ONLY.contains(&sub)
}

/// True when `sub` is everyday repository porcelain, read-only, or has its own
/// arm; false when its path operands must be judged as possible write targets.
pub(super) fn is_ordinary(sub: &str) -> bool {
    ORDINARY.contains(&sub) || READ_ONLY.contains(&sub) || MODELLED.contains(&sub)
}

/// Longest alias chain followed (git itself stops at a loop).
const MAX_ALIAS_DEPTH: usize = 8;

/// The subcommand and arguments an alias set by `-c alias.NAME=VALUE` stands
/// for, or `None` when `sub` is not such an alias. A `!` alias runs a shell
/// command and is refused by the exec ban before this is reached.
pub(super) fn resolve_alias(
    globals: &[&str],
    sub: &str,
    args: &[&str],
) -> Option<(String, Vec<String>)> {
    let assignments: Vec<(String, String)> = git_global_config_assignments(globals)
        .into_iter()
        .filter_map(|a| {
            let a = first_shell_word(a).unwrap_or_else(|| a.to_string());
            let (key, value) = a.split_once('=')?;
            key.get(..6).filter(|p| p.eq_ignore_ascii_case("alias."))?;
            Some((key[6..].to_ascii_lowercase(), value.to_string()))
        })
        .collect();
    if assignments.is_empty() {
        return None;
    }
    let mut cur_sub = sub.to_string();
    let mut cur_args: Vec<String> = args.iter().map(|a| (*a).to_string()).collect();
    let mut resolved = false;
    for _ in 0..MAX_ALIAS_DEPTH {
        let Some((_, value)) = assignments
            .iter()
            .rev()
            .find(|(n, _)| *n == cur_sub.to_ascii_lowercase())
        else {
            break;
        };
        if value.trim_start().starts_with('!') {
            break;
        }
        let words: Vec<String> = quote_aware_words(value)
            .iter()
            .map(|w| first_shell_word(w).unwrap_or_else(|| w.clone()))
            .collect();
        let Some((head, tail)) = words.split_first() else {
            break;
        };
        let mut next_args = tail.to_vec();
        next_args.extend(cur_args);
        cur_args = next_args;
        cur_sub = head.clone();
        resolved = true;
    }
    resolved.then_some((cur_sub, cur_args))
}
