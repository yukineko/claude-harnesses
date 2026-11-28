//! The `git apply` verdict (backlog ea96e65d), split out of `detect/git.rs`.
//!
//! `git apply` writes the files its patch names, below the directory git runs in
//! (moved by `-C`) and below `--directory=<root>` when given. The patch text is
//! not on the command line, so the one place that IS is the prefix: a
//! `--directory` value that is, or holds, a protected gate/config path is judged
//! exactly like any other write there (`protected_path_block` /
//! `protected_landing_block`, lifted inside the session's linked worktree
//! checkout as everywhere); a value (or `-C` directory) that cannot be placed is
//! Ask. `--unsafe-paths` lifts git's own refusal of paths outside the work tree,
//! so the patch can name a protected path through `..` and the command line
//! cannot rule that out: Ask. Read-only modes (`--check`, `--stat`,
//! `--numstat`, `--summary`) write nothing and stay Allow.

use super::{git_moved_dir, Ctx, Decision, Determination};

pub(super) fn git_apply_verdict(ctx: &Ctx<'_>, globals: &[&str], args: &[&str]) -> Decision {
    if args
        .iter()
        .any(|t| matches!(*t, "--check" | "--stat" | "--numstat" | "--summary"))
    {
        return Decision::Allow;
    }
    let base = match git_moved_dir(ctx.scope, globals, ctx.base_for("git")) {
        Determination::Known(Some(dir)) => Some(dir),
        Determination::Known(None) => ctx.base_for("git").map(str::to_string),
        Determination::Undetermined(u) => {
            return Decision::ask(format!(
                "git apply runs in a directory that could not be placed ({}) — blastguard \
cannot tell which files it writes and refuses to guess",
                u.reason().as_str()
            ));
        }
    };
    let mut i = 0;
    while i < args.len() {
        let t = args[i];
        let value = if t == "--directory" {
            i += 1;
            let Some(v) = args.get(i) else {
                return Decision::ask(
                    "git apply --directory has no value — blastguard refuses to guess where the \
patch is written",
                );
            };
            Some(*v)
        } else {
            t.strip_prefix("--directory=")
        };
        i += 1;
        let Some(value) = value else { continue };
        let abs = match ctx.scope.lexical_absolute(value, base.as_deref()) {
            Determination::Known(a) => a,
            Determination::Undetermined(u) => {
                return Decision::ask(format!(
                    "git apply --directory={value} could not be placed ({}) — blastguard cannot \
tell whether the patch lands on a protected path and refuses to guess",
                    u.reason().as_str()
                ));
            }
        };
        let action = "git apply --directory";
        if let Some(d) = super::super::protected::protected_landing_block(ctx, action, &abs)
            .or_else(|| super::super::protected::protected_path_block(ctx, action, &abs))
        {
            return d;
        }
    }
    if args.contains(&"--unsafe-paths") {
        return Decision::ask(
            "git apply --unsafe-paths lets the patch write outside the work tree, and the patch \
text is not on the command line — blastguard cannot tell whether it lands on a protected \
gate/config path and refuses to guess",
        );
    }
    Decision::Allow
}
