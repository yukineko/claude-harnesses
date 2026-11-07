//! `git branch` force-delete and `git worktree remove --force`: the two git
//! commands that throw away work git holds nowhere else (memory
//! `worktree-always-merge-then-delete`, backlog 43034f75).
//!
//! * A force-deleted branch (`-D`, or a delete flag plus a force flag in any
//!   spelling: `-d -f`, `-df`, `-fd`, `--delete --force`, `--force --delete`)
//!   loses its commits unless they are reachable from the repository's default
//!   branch. Every operand is checked. Deleting the default branch itself —
//!   in any letter case (`-D Main` deletes `main` on a case-insensitive
//!   filesystem) — is Ask.
//! * A force-removed worktree loses everything `git status --porcelain
//!   --ignored` reports in it. Ignored files COUNT as content: the removal
//!   deletes the whole directory, ignored build output (e.g. `target/`)
//!   included, so a worktree holding only ignored files is Ask too.
//!
//! The probes run in the repository git will act on: the cumulative `-C`
//! directory (relative to the segment's base), with `--git-dir` /
//! `--work-tree` forwarded. Every cannot-determine state is Ask, never Allow
//! (CLAUDE.md §3): no known working directory (`base_for("git")` is `None`
//! and no absolute `-C`), an unplaceable or unrecognised global option, no
//! operand, an unresolvable default branch, a git probe that exits with a code
//! other than the ones listed for it, fails to spawn, or times out.

use super::{positional_operands, Ctx, Decision, Determination};

/// Per-probe timeout. Several probes run per command; each is a local
/// ref/status read.
const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(1500);

/// Where the git command acts: the directory it runs in and the repository
/// options that relocate it. All paths are absolute.
struct Target {
    dir: String,
    git_dir: Option<String>,
    work_tree: Option<String>,
}

/// Global options that neither move git nor change which refs it sees.
const INERT_GLOBALS: &[&str] = &[
    "--no-pager",
    "-p",
    "--paginate",
    "-P",
    "--no-replace-objects",
    "--no-optional-locks",
    "--literal-pathspecs",
    "--glob-pathspecs",
    "--noglob-pathspecs",
    "--icase-pathspecs",
    "--no-advice",
];

/// Resolve the repository `globals` (the words before the subcommand) point
/// git at. `-C` applies cumulatively, each relative to the previous (the first
/// relative to `base`); a relative `--git-dir` / `--work-tree` is relative to
/// the final `-C` directory, as git sets them up after changing directory.
/// `-c` / `--config-env` values are skipped (they are not forwarded to the
/// probes). Any other global option is Undetermined.
fn target(ctx: &Ctx<'_>, globals: &[&str]) -> Determination<Target> {
    let mut cur: Option<String> = ctx.base_for("git").map(str::to_string);
    let mut git_dir: Option<&str> = None;
    let mut work_tree: Option<&str> = None;
    let mut i = 0;
    while i < globals.len() {
        let t = globals[i];
        let (key, value) = if matches!(t, "-C" | "--git-dir" | "--work-tree") {
            let Some(v) = globals.get(i + 1) else {
                return Determination::undetermined(format!("git {t} has no value"));
            };
            i += 2;
            (t, *v)
        } else if let Some(v) = t.strip_prefix("--git-dir=") {
            i += 1;
            ("--git-dir", v)
        } else if let Some(v) = t.strip_prefix("--work-tree=") {
            i += 1;
            ("--work-tree", v)
        } else if t == "-c" || t == "--config-env" {
            i += 2;
            continue;
        } else if t.starts_with("--config-env=")
            || (t.starts_with("-c") && t.len() > 2)
            || INERT_GLOBALS.contains(&t)
        {
            i += 1;
            continue;
        } else {
            return Determination::undetermined(format!(
                "git global option `{t}` is not one blastguard can place"
            ));
        };
        match key {
            "-C" => {
                cur = match ctx.scope.lexical_absolute(value, cur.as_deref()) {
                    Determination::Known(d) => Some(d),
                    Determination::Undetermined(u) => return Determination::Undetermined(u),
                };
            }
            "--git-dir" => {
                if git_dir.replace(value).is_some() {
                    return Determination::undetermined("git --git-dir given twice");
                }
            }
            _ => {
                if work_tree.replace(value).is_some() {
                    return Determination::undetermined("git --work-tree given twice");
                }
            }
        }
    }
    let Some(dir) = cur else {
        return Determination::undetermined(
            "the directory git runs in is unknown (no known working directory)",
        );
    };
    let place = |p: Option<&str>| -> Determination<Option<String>> {
        match p {
            None => Determination::known(None),
            Some(p) => match ctx.scope.lexical_absolute(p, Some(&dir)) {
                Determination::Known(a) => Determination::known(Some(a)),
                Determination::Undetermined(u) => Determination::Undetermined(u),
            },
        }
    };
    let git_dir = match place(git_dir) {
        Determination::Known(g) => g,
        Determination::Undetermined(u) => return Determination::Undetermined(u),
    };
    let work_tree = match place(work_tree) {
        Determination::Known(w) => w,
        Determination::Undetermined(u) => return Determination::Undetermined(u),
    };
    Determination::known(Target {
        dir,
        git_dir,
        work_tree,
    })
}

/// Run `git <args>` against `t` and return its exit code when it is one of
/// `ok_codes` (with stdout); anything else — another code, a spawn failure, a
/// timeout, an unreadable pipe — is Undetermined.
fn probe(t: &Target, args: &[&str], ok_codes: &[i32]) -> Determination<(i32, String)> {
    let mut cmd = std::process::Command::new("git");
    harness_core::git_env::scrub(&mut cmd).arg("-C").arg(&t.dir);
    if let Some(g) = &t.git_dir {
        cmd.arg(format!("--git-dir={g}"));
    }
    if let Some(w) = &t.work_tree {
        cmd.arg(format!("--work-tree={w}"));
    }
    // fsmonitor would run a configured command; optional locks would write.
    cmd.args(["--no-optional-locks", "-c", "core.fsmonitor=false"])
        .args(args);
    match harness_core::boundary::run_with_timeout(&mut cmd, PROBE_TIMEOUT) {
        Determination::Known(out) => {
            let code = out.code();
            match out.stdout_allowing(ok_codes) {
                Determination::Known(s) => Determination::known((code, s)),
                Determination::Undetermined(u) => Determination::Undetermined(u),
            }
        }
        Determination::Undetermined(u) => Determination::Undetermined(u),
    }
}

/// `Known(true)` when `full_ref` exists in `t` (exit 0), `Known(false)` when
/// git reports it absent (exit 1); otherwise Undetermined.
fn ref_exists(t: &Target, full_ref: &str) -> Determination<bool> {
    match probe(t, &["rev-parse", "--verify", "--quiet", full_ref], &[0, 1]) {
        Determination::Known((code, _)) => Determination::known(code == 0),
        Determination::Undetermined(u) => Determination::Undetermined(u),
    }
}

/// The repository's default branch: its short name and the existing refs that
/// hold it (the local branch and, when resolved through `origin/HEAD`, the
/// remote-tracking ref).
struct DefaultBranch {
    name: String,
    refs: Vec<String>,
}

/// Pick the default branch of `t`, in this order:
///
/// 1. `refs/remotes/origin/HEAD` as a symbolic ref (`git symbolic-ref`, exit 1
///    = not set, go on) — a recorded fact about the repository;
/// 2. `init.defaultBranch` (`git config --get`, exit 1 = unset), used only when
///    `refs/heads/<it>` exists. This setting only names the initial branch
///    of NEW repositories, so for an existing repo it is a hint, not a fact;
/// 3. a NAME-BASED HEURISTIC, reached only when steps 1 and 2 give nothing:
///    if exactly one of the conventional names `main` / `master` / `trunk`
///    exists as a local branch, it is taken as the default. Nothing in the
///    repository records that it is; a repo whose real default is another
///    branch but which also has e.g. `main` is judged against `main`.
///
/// Undetermined when none of these names an existing ref, when step 3 finds
/// more than one candidate, or when any probe fails. HEAD is never used as the
/// default.
fn default_branch(t: &Target) -> Determination<DefaultBranch> {
    macro_rules! known {
        ($e:expr) => {
            match $e {
                Determination::Known(v) => v,
                Determination::Undetermined(u) => return Determination::Undetermined(u),
            }
        };
    }
    let local = |name: &str| format!("refs/heads/{name}");
    let (code, out) = known!(probe(
        t,
        &["symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"],
        &[0, 1]
    ));
    if code == 0 {
        let remote = out.trim().to_string();
        let Some(name) = remote.strip_prefix("refs/remotes/origin/") else {
            return Determination::undetermined(format!(
                "refs/remotes/origin/HEAD points outside refs/remotes/origin/: `{remote}`"
            ));
        };
        let name = name.to_string();
        let mut refs = Vec::new();
        for r in [local(&name), remote.clone()] {
            if known!(ref_exists(t, &r)) {
                refs.push(r);
            }
        }
        if refs.is_empty() {
            return Determination::undetermined(format!(
                "refs/remotes/origin/HEAD names `{name}`, but neither {} nor {remote} exists",
                local(&name)
            ));
        }
        return Determination::known(DefaultBranch { name, refs });
    }
    let (code, out) = known!(probe(
        t,
        &["config", "--get", "init.defaultBranch"],
        &[0, 1]
    ));
    if code == 0 {
        let name = out.trim().to_string();
        if !name.is_empty() && known!(ref_exists(t, &local(&name))) {
            let refs = vec![local(&name)];
            return Determination::known(DefaultBranch { name, refs });
        }
    }
    let mut found = Vec::new();
    for name in ["main", "master", "trunk"] {
        if known!(ref_exists(t, &local(name))) {
            found.push(name);
        }
    }
    match found.as_slice() {
        [one] => Determination::known(DefaultBranch {
            name: (*one).to_string(),
            refs: vec![local(one)],
        }),
        [] => Determination::undetermined(
            "no origin/HEAD, no existing init.defaultBranch branch, and none of main/master/trunk \
exists",
        ),
        many => Determination::undetermined(format!(
            "no origin/HEAD or existing init.defaultBranch branch, and more than one candidate \
exists ({})",
            many.join(", ")
        )),
    }
}

/// True for a long option that git's parser accepts as `--<full>`: the full
/// spelling or an unambiguous prefix of at least `min` characters after `--`.
fn long_abbrev(tok: &str, full: &str, min: usize) -> bool {
    tok.strip_prefix("--")
        .is_some_and(|t| t.len() >= min && full.starts_with(t))
}

/// True for a short-option bundle (`-x`, `-xy`) containing `ch`.
fn short_has(tok: &str, ch: char) -> bool {
    tok.len() > 1 && tok.starts_with('-') && !tok.starts_with("--") && tok.contains(ch)
}

/// Options before the `--` that ends them.
fn option_words<'a, 'b>(args: &'b [&'a str]) -> &'b [&'a str] {
    let end = args.iter().position(|t| *t == "--").unwrap_or(args.len());
    &args[..end]
}

/// Operands of `args`: [`positional_operands`] before a `--`, every word after
/// it.
fn operands<'a>(args: &[&'a str]) -> Vec<&'a str> {
    match args.iter().position(|t| *t == "--") {
        Some(p) => {
            let mut v = positional_operands(&args[..p], &[]);
            v.extend(args[p + 1..].iter().copied());
            v
        }
        None => positional_operands(args, &[]),
    }
}

/// `git <globals> branch <args>`. Allow unless the command force-deletes;
/// then Allow only when every operand is observed to lose nothing: it names no
/// existing branch, or it is an ancestor of the default branch (and is not the
/// default branch itself). Everything else is Ask.
pub(super) fn branch_verdict(ctx: &Ctx<'_>, globals: &[&str], args: &[&str]) -> Decision {
    let opts = option_words(args);
    // `--d`… is unambiguous for `--delete`; `--force` needs `--for` (`--fo`
    // also matches `--format`).
    let delete = opts
        .iter()
        .any(|t| short_has(t, 'd') || short_has(t, 'D') || long_abbrev(t, "delete", 1));
    let force = opts
        .iter()
        .any(|t| short_has(t, 'f') || short_has(t, 'D') || long_abbrev(t, "force", 3));
    if !(delete && force) {
        return Decision::Allow;
    }
    let remotes = opts
        .iter()
        .any(|t| short_has(t, 'r') || long_abbrev(t, "remotes", 3));
    let ask = |why: String| {
        Decision::ask(format!(
            "git branch force-delete: blastguard cannot determine that the deleted branch's \
commits are kept elsewhere ({why}), and a force-delete destroys unmerged work"
        ))
    };
    let ops = operands(args);
    if ops.is_empty() {
        return ask("no branch operand to check".to_string());
    }
    let t = match target(ctx, globals) {
        Determination::Known(t) => t,
        Determination::Undetermined(u) => return ask(u.reason().as_str().to_string()),
    };
    let default = match default_branch(&t) {
        Determination::Known(d) => d,
        Determination::Undetermined(u) => {
            return ask(format!(
                "the default branch could not be resolved: {}",
                u.reason().as_str()
            ))
        }
    };
    for op in ops {
        let full = if remotes {
            format!("refs/remotes/{op}")
        } else {
            format!("refs/heads/{op}")
        };
        // Compared case-insensitively: on a case-insensitive filesystem
        // (macOS default) a loose `refs/heads/Main` IS the `main` ref file, so
        // `git branch -D Main` deletes `main`, while the merge-base probe
        // below would see `main` as an ancestor of itself and Allow. A
        // case-variant that is a different ref on a case-sensitive
        // filesystem is Ask too (the stricter side).
        let same = |a: &str, b: &str| a.to_lowercase() == b.to_lowercase();
        if (!remotes && same(op, &default.name)) || default.refs.iter().any(|r| same(r, &full)) {
            return Decision::ask(format!(
                "git branch force-delete of `{op}`, the repository's default branch \
(`{}`): deleting it removes the ref every other branch is judged against",
                default.name
            ));
        }
        match ref_exists(&t, &full) {
            Determination::Known(false) => continue, // git deletes nothing for it
            Determination::Known(true) => {}
            Determination::Undetermined(u) => return ask(u.reason().as_str().to_string()),
        }
        let mut merged = false;
        for d in &default.refs {
            match probe(&t, &["merge-base", "--is-ancestor", &full, d], &[0, 1]) {
                // 0 = `full` is an ancestor of `d`; 1 = it is not.
                Determination::Known((0, _)) => {
                    merged = true;
                    break;
                }
                Determination::Known(_) => {}
                Determination::Undetermined(u) => return ask(u.reason().as_str().to_string()),
            }
        }
        if !merged {
            return Decision::ask(format!(
                "git branch force-delete of `{op}`: it has commits not reachable from the \
default branch `{}` ({}), so deleting it destroys unmerged work",
                default.name,
                default.refs.join(", ")
            ));
        }
    }
    Decision::Allow
}

/// `git <globals> worktree <args>`. Allow unless the verb is `remove` with a
/// force flag anywhere in `args` (before or after the verb); then Allow only
/// when `git status --porcelain --ignored` in every operand worktree exits 0
/// and prints nothing. A relative operand is placed against the directory git
/// runs in (the cumulative `-C` directory, else the segment's base).
pub(super) fn worktree_verdict(ctx: &Ctx<'_>, globals: &[&str], args: &[&str]) -> Decision {
    let opts = option_words(args);
    let Some(verb_pos) = args.iter().position(|t| !t.starts_with('-')) else {
        return Decision::Allow;
    };
    if args[verb_pos] != "remove" {
        return Decision::Allow;
    }
    let force = opts
        .iter()
        .any(|t| short_has(t, 'f') || long_abbrev(t, "force", 1));
    if !force {
        return Decision::Allow;
    }
    let ask = |why: String| {
        Decision::ask(format!(
            "git worktree remove --force: blastguard cannot determine that the worktree holds \
nothing that would be lost ({why}); a forced removal deletes the whole directory, \
uncommitted, untracked and ignored files included"
        ))
    };
    let ops = operands(&args[verb_pos + 1..]);
    if ops.is_empty() {
        return ask("no worktree operand to check".to_string());
    }
    let t = match target(ctx, globals) {
        Determination::Known(t) => t,
        Determination::Undetermined(u) => return ask(u.reason().as_str().to_string()),
    };
    for op in ops {
        let path = match ctx.scope.lexical_absolute(op, Some(&t.dir)) {
            Determination::Known(p) => p,
            Determination::Undetermined(u) => return ask(u.reason().as_str().to_string()),
        };
        let wt = Target {
            dir: path,
            git_dir: None,
            work_tree: None,
        };
        match probe(&wt, &["status", "--porcelain", "--ignored"], &[0]) {
            Determination::Known((_, out)) if out.trim().is_empty() => {}
            Determination::Known(_) => {
                return Decision::ask(format!(
                    "git worktree remove --force `{op}`: git status reports uncommitted changes, \
untracked files or ignored files there (ignored files such as build output in target/ count: \
the forced removal deletes the whole directory), which would be destroyed"
                ))
            }
            Determination::Undetermined(u) => return ask(u.reason().as_str().to_string()),
        }
    }
    Decision::Allow
}
