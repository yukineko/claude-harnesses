//! The `git rm` verdict, split out of `detect/git.rs` (file-growth cap).
//!
//! Round 2: `git rm` had no arm, so `git rm .githooks/pre-commit`,
//! `git rm -rf .githooks`, `git rm -r -f .claude` and
//! `git rm --cached .claude/settings.json` all reached `_ => Allow`.
//! `.githooks/**` is tracked in this repo, so `git rm` really does delete it
//! from the working tree — the same one-command disarm as the already-denied
//! `rm -rf .githooks`, spelled through git.
//!
//! `--cached` is denied too for protected targets: it leaves the bytes on disk
//! but removes the file from the repo, so the next clone/checkout has no gate.
//! Ordinary targets are untouched (`git rm src/old.rs` stays Allow) — only the
//! TARGET is classified, never the blast radius.
//!
//! The recursive main-tree refusal (backlog f63b97b3) is about deleting tracked
//! files from the shared main working tree. `--cached` and `-n`/`--dry-run`
//! leave every working-tree file in place, so they are exempt from that
//! refusal — but only when the shared git argv parser read the whole line;
//! anything it could not read keeps the refusal.

use super::argv::{parse, Opt, Spec};
use super::{
    git_moved_dir, positional_operands, protected_disarm_deny, protected_glob_deny,
    protected_tree_deny, Ctx, Decision, Determination,
};

/// The options of `git rm`.
const RM_OPTS: &[Opt] = &[
    Opt::flag("recursive", Some('r')),
    Opt::flag("quiet", Some('q')),
    Opt::flag("force", Some('f')),
    Opt::flag("dry-run", Some('n')),
    Opt::flag("cached", None),
    Opt::flag("ignore-unmatch", None),
    Opt::flag("sparse", None),
    Opt::flag("pathspec-file-nul", None),
    Opt::val("pathspec-from-file", None),
];

/// Verdict for `git rm`. `global` are the words before the subcommand (`-C`,
/// `-c`, …); `args` the words after it.
pub(super) fn git_rm_verdict(ctx: &Ctx<'_>, global: &[&str], args: &[&str]) -> Decision {
    for op in positional_operands(args, &[]) {
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
    let recursive = args.iter().take_while(|t| **t != "--").any(|t| {
        *t == "--recursive" || (t.starts_with('-') && !t.starts_with("--") && t.contains('r'))
    });
    let parsed = parse(
        args,
        &Spec {
            tables: &[RM_OPTS],
            lenient: false,
            digits: false,
        },
    );
    let keeps_worktree_files =
        parsed.undetermined.is_empty() && (parsed.has("cached") || parsed.has("dry-run"));
    if recursive && !keeps_worktree_files {
        let base = match git_moved_dir(ctx.scope, global, ctx.base_for("git")) {
            Determination::Known(Some(d)) => Some(d),
            Determination::Known(None) => ctx.base_for("git").map(str::to_string),
            Determination::Undetermined(u) => {
                return Decision::ask(format!(
                    "git rm -r runs in a directory that could not be placed ({}) — \
blastguard cannot tell which tracked files it deletes and refuses to guess",
                    u.reason().as_str()
                ));
            }
        };
        for op in positional_operands(args, &[]) {
            if let Some(why) =
                crate::deletion::git_rm_main_tree_refusal(ctx.scope, op, base.as_deref())
            {
                return Decision::ask(format!(
                    "git rm -r {op}: {why} — a recursive delete of tracked files is not \
an Allow here, and blastguard refuses to guess"
                ));
            }
        }
    }
    Decision::Allow
}
