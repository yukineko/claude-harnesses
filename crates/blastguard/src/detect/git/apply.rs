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
//! `--numstat`, `--summary`) write nothing and stay Allow — unless `--apply` /
//! `--reject` re-enables the write, or an option is an ambiguous abbreviation.
//! Options are resolved as git does (any unique prefix: `--dir=` is
//! `--directory=`, see `long_opt`). The directory git runs in (cwd or `-C`) is
//! itself judged as a landing directory (backlog f63b97b3).

use super::long_opt::{resolve_long_abbrev, Abbrev};
use super::{git_moved_dir, Ctx, Decision, Determination};

/// The long options of `git apply` (git parse-options accepts any unique
/// prefix of these, so `--dir=x` is `--directory=x`).
const APPLY_LONGS: &[&str] = &[
    "--stat",
    "--numstat",
    "--summary",
    "--check",
    "--index",
    "--cached",
    "--intent-to-add",
    "--3way",
    "--apply",
    "--no-add",
    "--build-fake-ancestor",
    "--reverse",
    "--allow-binary-replacement",
    "--binary",
    "--reject",
    "--unidiff-zero",
    "--ignore-space-change",
    "--ignore-whitespace",
    "--whitespace",
    "--exclude",
    "--include",
    "--inaccurate-eof",
    "--recount",
    "--directory",
    "--unsafe-paths",
    "--verbose",
    "--quiet",
    "--allow-overlap",
];

/// `Some(name)` for the option `t` resolves to; an ambiguous prefix is
/// `Err(())` and callers treat it restrictively.
fn apply_option(t: &str) -> Result<Option<&'static str>, ()> {
    match resolve_long_abbrev(t, APPLY_LONGS) {
        Abbrev::Resolved { name, .. } => Ok(APPLY_LONGS.iter().copied().find(|k| *k == name)),
        Abbrev::NotAnOption => Ok(None),
        Abbrev::Ambiguous => Err(()),
    }
}

pub(super) fn git_apply_verdict(ctx: &Ctx<'_>, globals: &[&str], args: &[&str]) -> Decision {
    // Options end at `--`; a later word is a patch file name.
    let opts: Vec<&str> = args.iter().copied().take_while(|t| *t != "--").collect();
    let mut read_only = false;
    let mut writes = false;
    let mut ambiguous = None;
    for t in &opts {
        match apply_option(t) {
            Ok(Some("--check" | "--stat" | "--numstat" | "--summary")) => read_only = true,
            // `--apply` re-enables the write a read-only flag switched off, and
            // `--reject` applies what it can.
            Ok(Some("--apply" | "--reject")) => writes = true,
            Ok(_) => {}
            Err(()) => ambiguous = Some(*t),
        }
    }
    if read_only && !writes && ambiguous.is_none() {
        return Decision::Allow;
    }
    if let Some(t) = ambiguous {
        return Decision::ask(format!(
            "git apply option `{t}` is an ambiguous abbreviation — blastguard cannot tell which \
option it names (it may be --directory or --apply) and refuses to guess"
        ));
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
    // The patch lands below the directory git runs in (cwd, moved by `-C`)
    // even without `--directory` (backlog f63b97b3).
    if let Some(b) = base.as_deref() {
        let action = "git apply";
        if let Some(d) = super::super::protected::protected_landing_block(ctx, action, b)
            .or_else(|| super::super::protected::protected_path_block(ctx, action, b))
        {
            return d;
        }
    }
    let mut i = 0;
    while i < opts.len() {
        let t = opts[i];
        let value = match resolve_long_abbrev(t, APPLY_LONGS) {
            Abbrev::Resolved {
                name: "--directory",
                value: Some(v),
            } => Some(v),
            Abbrev::Resolved {
                name: "--directory",
                value: None,
            } => {
                i += 1;
                let Some(v) = opts.get(i) else {
                    return Decision::ask(
                        "git apply --directory has no value — blastguard refuses to guess where \
the patch is written",
                    );
                };
                Some(*v)
            }
            _ => None,
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
    if opts
        .iter()
        .any(|t| apply_option(t) == Ok(Some("--unsafe-paths")))
    {
        return Decision::ask(
            "git apply --unsafe-paths lets the patch write outside the work tree, and the patch \
text is not on the command line — blastguard cannot tell whether it lands on a protected \
gate/config path and refuses to guess",
        );
    }
    Decision::Allow
}
