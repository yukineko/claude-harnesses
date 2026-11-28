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

/// Every command `git help -a` lists (porcelain, ancillary, plumbing) — the
/// names git runs as builtins / external `git-*` programs BEFORE it looks at an
/// alias. A static table: the set of git commands is not read from the machine.
const BUILTINS: &[&str] = &[
    "add",
    "am",
    "annotate",
    "apply",
    "archimport",
    "archive",
    "bisect",
    "blame",
    "branch",
    "bugreport",
    "bundle",
    "cat-file",
    "check-attr",
    "check-ignore",
    "check-mailmap",
    "check-ref-format",
    "checkout",
    "checkout-index",
    "cherry",
    "cherry-pick",
    "citool",
    "clean",
    "clone",
    "column",
    "commit",
    "commit-graph",
    "commit-tree",
    "config",
    "count-objects",
    "credential",
    "credential-cache",
    "credential-store",
    "cvsexportcommit",
    "cvsimport",
    "cvsserver",
    "daemon",
    "describe",
    "diagnose",
    "diff",
    "diff-files",
    "diff-index",
    "diff-tree",
    "difftool",
    "fast-export",
    "fast-import",
    "fetch",
    "fetch-pack",
    "filter-branch",
    "fmt-merge-msg",
    "for-each-ref",
    "for-each-repo",
    "format-patch",
    "fsck",
    "fsck-objects",
    "gc",
    "get-tar-commit-id",
    "gitk",
    "gitweb",
    "grep",
    "gui",
    "hash-object",
    "help",
    "hook",
    "http-backend",
    "imap-send",
    "index-pack",
    "init",
    "init-db",
    "instaweb",
    "interpret-trailers",
    "log",
    "ls-files",
    "ls-remote",
    "ls-tree",
    "mailinfo",
    "mailsplit",
    "maintenance",
    "merge",
    "merge-base",
    "merge-file",
    "merge-index",
    "merge-octopus",
    "merge-one-file",
    "merge-ours",
    "merge-recursive",
    "merge-resolve",
    "merge-subtree",
    "merge-tree",
    "mergetool",
    "mktag",
    "mktree",
    "multi-pack-index",
    "mv",
    "name-rev",
    "notes",
    "p4",
    "pack-objects",
    "pack-redundant",
    "pack-refs",
    "patch-id",
    "prune",
    "prune-packed",
    "pull",
    "push",
    "quiltimport",
    "range-diff",
    "read-tree",
    "rebase",
    "receive-pack",
    "reflog",
    "refs",
    "remote",
    "remote-ext",
    "remote-fd",
    "repack",
    "replace",
    "replay",
    "request-pull",
    "rerere",
    "reset",
    "restore",
    "rev-list",
    "rev-parse",
    "revert",
    "rm",
    "scalar",
    "send-email",
    "send-pack",
    "sh-i18n",
    "sh-setup",
    "shell",
    "shortlog",
    "show",
    "show-branch",
    "show-index",
    "show-ref",
    "sparse-checkout",
    "stage",
    "stash",
    "status",
    "stripspace",
    "submodule",
    "svn",
    "switch",
    "symbolic-ref",
    "tag",
    "unpack-file",
    "unpack-objects",
    "update-index",
    "update-ref",
    "update-server-info",
    "upload-archive",
    "upload-pack",
    "var",
    "verify-commit",
    "verify-pack",
    "verify-tag",
    "version",
    "web--browse",
    "whatchanged",
    "worktree",
    "write-tree",
];

/// True when git would run `sub` as a command of its own (an alias of the same
/// name is ignored).
pub(super) fn is_builtin(sub: &str) -> bool {
    BUILTINS.contains(&sub) || is_ordinary(sub)
}

/// Subcommands that write files into the work tree (or remove / move them):
/// the work tree is a landing for these. Reading verbs and verbs that only touch
/// the index / refs / objects (`add`, `commit`, `fetch`, `tag`, …) are not.
const WORKTREE_WRITERS: &[&str] = &[
    "checkout",
    "checkout-index",
    "switch",
    "restore",
    "reset",
    "read-tree",
    "stash",
    "am",
    "apply",
    "merge",
    "rebase",
    "pull",
    "cherry-pick",
    "revert",
    "clean",
    "rm",
    "mv",
    "sparse-checkout",
];

/// True when `sub` writes into the work tree (see [`WORKTREE_WRITERS`]).
pub(super) fn writes_work_tree(sub: &str) -> bool {
    WORKTREE_WRITERS.contains(&sub)
}

/// Helpers that drop files in the current directory without naming them:
/// `unpack-file` writes `.merge_file_*`, `bugreport` / `diagnose` a report.
/// Only these make the cwd a landing; a verb that reads or prints (`fast-export`,
/// `submodule status`) does not.
const CWD_WRITERS: &[&str] = &["unpack-file", "bugreport", "diagnose"];

/// True when `sub` writes a file into the cwd (see [`CWD_WRITERS`]).
pub(super) fn writes_cwd(sub: &str) -> bool {
    CWD_WRITERS.contains(&sub)
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
    stored: &[(String, String)],
) -> Option<(String, Vec<String>)> {
    // Stored (config file) aliases first, command-line `-c` after: the later
    // assignment wins.
    let mut assignments: Vec<(String, String)> = stored.to_vec();
    assignments.extend(
        git_global_config_assignments(globals)
            .into_iter()
            .filter_map(|a| {
                let a = first_shell_word(a).unwrap_or_else(|| a.to_string());
                let (key, value) = a.split_once('=')?;
                key.get(..6).filter(|p| p.eq_ignore_ascii_case("alias."))?;
                Some((key[6..].to_ascii_lowercase(), value.to_string()))
            }),
    );
    if assignments.is_empty() {
        return None;
    }
    let mut cur_sub = sub.to_string();
    let mut cur_args: Vec<String> = args.iter().map(|a| (*a).to_string()).collect();
    let mut resolved = false;
    for _ in 0..MAX_ALIAS_DEPTH {
        // git runs a builtin of that name and ignores an alias spelled the
        // same, so a builtin name is never expanded (`-c alias.merge-file=log
        // merge-file …` is `merge-file`).
        if is_builtin(&cur_sub) {
            break;
        }
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
