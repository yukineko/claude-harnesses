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
//! The rule judges against [`SafeRoots::managed_place_model`] and resolves a
//! relative landing against [`PlaceBases`] (`Ctx::places`). For the hook
//! binary both are the session's own model and `raw_base`. The
//! library-consumer entry [`crate::detect::detect`] (condukt, specguard,
//! daily) keeps [`SafeRoots::none`] for every other rule and attaches
//! [`process_place_model`] here (GitHub #1637), so no other rule gains a
//! location model. Before #1637 that entry skipped this rule and answered
//! `Allow`, so those unattended callers (which harden `Ask` into `Deny`) got a
//! MORE permissive verdict than the hook for the same command. Building that
//! model reads the process environment and the filesystem.
//!
//! With no location model at all (a bare [`SafeRoots::none`] handed to
//! [`crate::detect::detect_scoped`]) the rule STILL applies: no landing can be
//! shown to be managed, so every tree-creating shape is `Ask`. The shapes that
//! create no tree (`git clone` with no repository, `cp -r` with one operand)
//! keep their verdict either way.

use super::{Ctx, Decision, Determination};
use crate::scope::{Placement, SafeRoots};

impl SafeRoots {
    /// Attach `places` as the model the scratch-place rule consults instead
    /// of `self`. Every other rule keeps judging against `self`.
    #[must_use]
    pub(crate) fn with_managed_places(mut self, places: SafeRoots) -> SafeRoots {
        self.managed_places = Some(std::sync::Arc::new(places));
        self
    }

    /// The model the scratch-place rule consults: the attached
    /// [`SafeRoots::with_managed_places`] model, else `self`.
    pub(crate) fn managed_place_model(&self) -> &SafeRoots {
        self.managed_places.as_deref().unwrap_or(self)
    }
}

/// The bases the scratch-place rule resolves a relative landing against:
/// `raw` / `cd` mirror `Ctx::raw_base` / `Ctx::rewritten_base` exactly
/// (same `cd` tracking), starting from [`SafeRoots::managed_place_model`]'s
/// session cwd instead of the scope's — so they are identical to `Ctx`'s own
/// whenever no separate place model is attached (the hook). `None` = unknown.
#[derive(Clone)]
pub(super) struct PlaceBases<'a> {
    raw: Option<String>,
    cd: Option<&'a str>,
}

impl<'a> PlaceBases<'a> {
    pub(super) fn new(scope: &'a SafeRoots) -> PlaceBases<'a> {
        let cwd = scope.managed_place_model().session_cwd();
        PlaceBases {
            raw: cwd.map(str::to_string),
            cd: cwd,
        }
    }

    /// After a `cd` the analysis resolved to `dir` (`Ctx::after_cd`).
    pub(super) fn after_cd(&self, dir: &str) -> PlaceBases<'a> {
        let raw = if dir.starts_with('/') {
            Some(dir.to_string())
        } else {
            self.cd
                .map(|base| format!("{}/{}", base.trim_end_matches('/'), dir))
        };
        PlaceBases { raw, cd: self.cd }
    }

    /// After a `cd` the analysis could not resolve (`Ctx::after_unknown_cd`).
    pub(super) fn unknown() -> PlaceBases<'a> {
        PlaceBases {
            raw: None,
            cd: None,
        }
    }

    fn base(&self) -> Option<&str> {
        self.raw.as_deref()
    }
}

/// The managed places of the PROCESS the library entry runs in, for the
/// scratch-place rule only: the process cwd (where the library callers'
/// `sh -c` runs, since none of them sets a different `current_dir`),
/// `CLAUDE_PROJECT_DIR`, `HOME` (for `$HOME/.condukt/worktrees`; never a
/// root itself) and `TMPDIR`, built by [`SafeRoots::new`] with the same
/// derivation rules the hook binary uses. Every read that fails is `None`,
/// which only removes managed places (more `Ask`), never adds one.
pub(super) fn process_place_model() -> SafeRoots {
    let cwd = std::env::current_dir()
        .ok()
        .and_then(|p| p.to_str().map(str::to_string));
    let project = std::env::var("CLAUDE_PROJECT_DIR").ok();
    let home = std::env::var("HOME").ok();
    let tmpdir = std::env::var("TMPDIR").ok();
    SafeRoots::new(
        cwd.as_deref(),
        project.as_deref(),
        home.as_deref(),
        tmpdir.as_deref(),
        Some(real_path),
    )
}

/// `path`'s real path, symlinked components resolved, without requiring that
/// `path` itself exists: the deepest existing ancestor is canonicalised and
/// the missing components re-attached (the same resolution the hook binary's
/// resolver performs). `None` when nothing resolves, which
/// [`SafeRoots::classify_managed_place`] reads as `Undetermined` (so `Ask`).
fn real_path(path: &str) -> Option<String> {
    let mut cur = std::path::PathBuf::from(path);
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    loop {
        if let Ok(real) = cur.canonicalize() {
            let mut out = real;
            for name in tail.iter().rev() {
                out.push(name);
            }
            return out.to_str().map(str::to_string);
        }
        let name = cur.file_name()?.to_os_string();
        if !cur.pop() {
            return None;
        }
        tail.push(name);
    }
}

/// The `Ask` (or `None` = managed) for a new tree `verb` creates at
/// `landing`, relative paths resolved against `base`.
fn landing_verdict(
    ctx: &Ctx<'_>,
    verb: &str,
    landing: &str,
    base: Option<&str>,
) -> Option<Decision> {
    let places = ctx.scope.managed_place_model();
    if !places.has_location_model() {
        return Some(Decision::ask(format!(
            "blastguard has no location model on this entry (no session cwd / project dir / temp \
roots), so it cannot tell whether the tree {verb} creates at `{landing}` lands in a managed place \
(session scratchpad / temp roots, worktrees, the project) and refuses to guess"
        )));
    }
    match places.classify_managed_place(landing, base) {
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
    if !matches!(verdict, Decision::Allow) {
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
    landing_verdict(ctx, "cp -r", &landing, ctx.places.base()).unwrap_or(verdict)
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
    let base: Option<String> = match super::git::git_moved_dir(
        ctx.scope.managed_place_model(),
        globals,
        ctx.places.base(),
    ) {
        Determination::Known(Some(d)) => Some(d),
        Determination::Known(None) => ctx.places.base().map(str::to_string),
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
