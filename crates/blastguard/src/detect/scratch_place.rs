//! Scratch trees outside the managed places (backlog ef7c9fbe).
//!
//! Memory `scratch-artifacts-in-deletable-places` used to live only in LLM
//! memory: a scratch TREE (a `git clone`, a `cp -r`) is created only in a
//! place that is cleaned up — the session scratchpad / temp roots, the
//! worktree storage roots, or the project. User ruling 2026-11-07:
//! "実装で補えるものは実装にして。LLMの推論での防御はすべてあてにならない" — so it is
//! enforced here. Reading (a) pinned: any destination outside those places is
//! `Ask`; inside them the verdict is unchanged. Later ruling the same day:
//! `mkdir -p` is NOT in the Ask set — only `git clone` and `cp -r`.
//!
//! The managed places are [`crate::scope::SafeRoots::classify_managed_place`]'s
//! (no new list). Every cannot-determine state — an option this parser does
//! not know, more operands than the verb takes, a destination that is not a
//! literal path, no known base for a relative destination, a `..` — is `Ask`
//! (CLAUDE.md §3). The rule only RAISES an `Allow` to an `Ask`; a `Deny`/`Ask`
//! another rule produced always stands (callers apply it to `Allow` only).
//!
//! With no location model ([`crate::scope::SafeRoots::none`], the
//! library-consumer entry [`crate::detect::detect`]) the rule does not apply:
//! that entry's contract is the verdict it had before the location model
//! existed, and the hook binary always builds a model.

use super::{Ctx, Decision, Determination};
use crate::scope::Placement;

/// The `Ask` (or `None` = managed) for a new tree `verb` creates at
/// `landing`, relative paths resolved against `base`.
fn landing_verdict(
    ctx: &Ctx<'_>,
    verb: &str,
    landing: &str,
    base: Option<&str>,
) -> Option<Decision> {
    match ctx.scope.classify_managed_place(landing, base) {
        Determination::Known(Placement::Inside { .. } | Placement::IsRoot { .. }) => None,
        Determination::Known(Placement::Outside { path }) => Some(Decision::ask(format!(
            "{verb} creates a tree at `{path}`, outside the managed places (session scratchpad / \
temp roots, .harness-worktrees / ~/.condukt/worktrees, the project) — a scratch tree there is \
never cleaned up; create it in the scratchpad or a worktree instead"
        ))),
        Determination::Undetermined(why) => Some(Decision::ask(format!(
            "blastguard cannot determine where {verb} creates its tree (`{landing}`: {}), so it \
cannot tell whether it lands in a managed place (session scratchpad / temp roots, worktrees, the \
project) and refuses to guess",
            why.reason().as_str()
        ))),
    }
}

/// An `Ask` for a parse this module cannot complete.
fn unparsed(verb: &str, why: &str) -> Decision {
    Decision::ask(format!(
        "blastguard cannot determine where {verb} creates its tree ({why}), so it cannot tell \
whether it lands in a managed place and refuses to guess"
    ))
}

/// `cp -r` / `-R` / `-a` (the caller has established the recursion): raise an
/// `Allow` to `Ask` when the landing is not a managed place. `verdict` is what
/// the copy rule already decided; anything but `Allow` is returned unchanged.
pub(super) fn cp_recursive_verdict(
    ctx: &Ctx<'_>,
    verdict: Decision,
    target_dir: Option<String>,
    operands: &[&str],
) -> Decision {
    if !matches!(verdict, Decision::Allow) || !ctx.scope.has_location_model() {
        return verdict;
    }
    let landing = match target_dir {
        Some(d) => d,
        None => match operands.split_last() {
            Some((last, init)) if !init.is_empty() => (*last).to_string(),
            // One operand or none: cp errors out and creates nothing.
            _ => return verdict,
        },
    };
    landing_verdict(ctx, "cp -r", &landing, ctx.base_for("cp")).unwrap_or(verdict)
}

/// `git clone` options that take a value, accepted in separate form
/// (`--branch main`) as well as `=`-joined.
const CLONE_VALUE_LONG: &[&str] = &[
    "--template",
    "--reference",
    "--reference-if-able",
    "--origin",
    "--branch",
    "--revision",
    "--upload-pack",
    "--depth",
    "--shallow-since",
    "--shallow-exclude",
    "--separate-git-dir",
    "--jobs",
    "--config",
    "--server-option",
    "--filter",
    "--bundle-uri",
    "--ref-format",
];

/// `git clone` options that take no value (or only a glued `=` one, like
/// `--recurse-submodules[=<pathspec>]`).
const CLONE_BOOL_LONG: &[&str] = &[
    "--verbose",
    "--quiet",
    "--progress",
    "--no-progress",
    "--reject-shallow",
    "--no-reject-shallow",
    "--no-checkout",
    "--checkout",
    "--bare",
    "--mirror",
    "--local",
    "--no-local",
    "--no-hardlinks",
    "--hardlinks",
    "--shared",
    "--dissociate",
    "--single-branch",
    "--no-single-branch",
    "--tags",
    "--no-tags",
    "--shallow-submodules",
    "--no-shallow-submodules",
    "--remote-submodules",
    "--no-remote-submodules",
    "--recurse-submodules",
    "--recursive",
    "--no-recurse-submodules",
    "--sparse",
    "--also-filter-submodules",
    "--ipv4",
    "--ipv6",
    "--no-template",
];

/// Short `git clone` flags: value-taking (the rest of the cluster, else the
/// next word) and boolean.
const CLONE_VALUE_SHORT: &str = "obucj";
const CLONE_BOOL_SHORT: &str = "lsqvn46";

/// `git clone`: `globals` are git's options before the subcommand, `args` the
/// words after `clone`. Ask when the new tree (and a `--separate-git-dir`)
/// does not land in a managed place; Allow otherwise.
pub(super) fn clone_verdict(ctx: &Ctx<'_>, globals: &[&str], args: &[&str]) -> Decision {
    if !ctx.scope.has_location_model() {
        return Decision::Allow;
    }
    const VERB: &str = "git clone";
    // Where git runs: `-C` moves it (cumulatively); anything that names a
    // different work tree / git dir is not modelled for clone.
    if globals
        .iter()
        .any(|g| g.starts_with("--work-tree") || g.starts_with("--git-dir"))
    {
        return unparsed(
            VERB,
            "a global --work-tree / --git-dir is not modelled for clone",
        );
    }
    let base: Option<String> =
        match super::git::git_moved_dir(ctx.scope, globals, ctx.base_for("git")) {
            Determination::Known(Some(d)) => Some(d),
            Determination::Known(None) => ctx.base_for("git").map(str::to_string),
            Determination::Undetermined(why) => {
                return unparsed(VERB, &format!("git -C: {}", why.reason().as_str()))
            }
        };
    let mut positionals: Vec<&str> = Vec::new();
    let mut extra_landings: Vec<String> = Vec::new();
    let mut i = 0;
    let mut opts_done = false;
    while i < args.len() {
        let t = args[i];
        i += 1;
        if opts_done || !t.starts_with('-') || t == "-" {
            positionals.push(t);
            continue;
        }
        if t == "--" {
            opts_done = true;
            continue;
        }
        if let Some(long) = t.strip_prefix("--") {
            let (name, glued) = match long.split_once('=') {
                Some((n, v)) => (format!("--{n}"), Some(v)),
                None => (t.to_string(), None),
            };
            if CLONE_VALUE_LONG.contains(&name.as_str()) {
                let value = match glued {
                    Some(v) => v.to_string(),
                    None => match args.get(i) {
                        Some(v) => {
                            i += 1;
                            (*v).to_string()
                        }
                        None => return unparsed(VERB, &format!("{name} has no value")),
                    },
                };
                if name == "--separate-git-dir" {
                    extra_landings.push(value);
                }
            } else if !CLONE_BOOL_LONG.contains(&name.as_str()) {
                return unparsed(VERB, &format!("unrecognised option `{name}`"));
            }
            continue;
        }
        // Short cluster.
        let cluster = &t[1..];
        for (pos, c) in cluster.char_indices() {
            if CLONE_VALUE_SHORT.contains(c) {
                if cluster[pos + c.len_utf8()..].is_empty() {
                    if args.get(i).is_none() {
                        return unparsed(VERB, &format!("-{c} has no value"));
                    }
                    i += 1;
                }
                break;
            }
            if !CLONE_BOOL_SHORT.contains(c) {
                return unparsed(VERB, &format!("unrecognised option `-{c}`"));
            }
        }
    }
    let landing = match positionals.as_slice() {
        // No repository: git errors out and creates nothing.
        [] => return Decision::Allow,
        // No directory: git creates `<humanish>` in the directory it runs in.
        [_repo] => ".".to_string(),
        [_repo, dir] => (*dir).to_string(),
        _ => return unparsed(VERB, "more than two operands"),
    };
    for l in std::iter::once(landing).chain(extra_landings) {
        if let Some(ask) = landing_verdict(ctx, VERB, &l, base.as_deref()) {
            return ask;
        }
    }
    Decision::Allow
}
