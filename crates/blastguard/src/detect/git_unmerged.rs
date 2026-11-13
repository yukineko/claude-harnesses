//! `git branch` force-delete and `git worktree remove --force`: the two git
//! commands that throw away work git holds nowhere else (memory
//! `worktree-always-merge-then-delete`, backlog 43034f75).
//!
//! * A force-deleted branch (`-D`, or a delete flag plus a force flag in any
//!   spelling: `-d -f`, `-df`, `-fd`, `--delete --force`, `--force --delete`)
//!   loses its commits unless they are reachable from the repository's default
//!   branch. Every operand is checked. Deleting the default branch itself is
//!   Ask, including through a spelling the filesystem folds onto it
//!   (`-D Main` / `-D maſter` delete `main` / `master` on APFS): the ref
//!   actually deleted is identified by loose-ref file identity
//!   ([`exact_ref`]), not by string comparison.
//! * A force-removed worktree loses everything `git status --porcelain=v1 -z
//!   --ignored=matching --untracked-files=all --ignore-submodules=none`
//!   reports in it. It is Allow only when every entry is an ignored directory
//!   positively recognised as regenerable build output (a real directory,
//!   not a symlink, whose last component is one of
//!   [`crate::deletion::BUILD_OUTPUT_NAMES`], e.g. `target/`; see
//!   [`only_ignored_build_output`], backlog 2089dfc5). Any modified, staged,
//!   untracked or other ignored entry (`.env`, keys, config, unknown names)
//!   is Ask: the removal deletes the whole directory. Only the worktree named
//!   as the operand is ever inspected (user ruling 2026-11-14, backlog
//!   b997a3d5): before any git probe, the operand must be, by local metadata,
//!   a linked-worktree root ([`linked_worktree_root`]: a real directory whose
//!   `.git` is a regular file), and the probe's `--show-toplevel` must be the
//!   operand itself. Otherwise (an ignored dir inside another worktree, a main
//!   worktree, a symlink, a missing path) it is Ask, so no enclosing or other
//!   worktree's state ever decides the verdict.
//!
//! The `branch` probes run in the repository git will act on: the cumulative
//! `-C` directory (relative to the segment's base), with `--git-dir` /
//! `--work-tree` forwarded. The `worktree remove` probes run only with `-C
//! <operand>` (no `--git-dir` / `--work-tree`); the cumulative `-C`
//! directory only places a relative operand. Every cannot-determine state is Ask, never Allow
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

/// Ask for force-deleting the default branch itself.
fn default_ask(op: &str, name: &str) -> Decision {
    Decision::ask(format!(
        "git branch force-delete of `{op}`, which is the repository's default branch \
(`{name}`): deleting it removes the ref every other branch is judged against"
    ))
}

/// The exact ref git acts on for `full`, a ref name `git rev-parse --verify`
/// has just reported as existing.
///
/// * `full` byte-equal to a ref `git for-each-ref <namespace>` lists: `full`.
/// * Otherwise git reached it only through filesystem folding of the loose
///   ref file (APFS folds case AND e.g. `ſ` -> `s`, which no string folding
///   here reproduces). The deleted ref is then identified by FILE IDENTITY:
///   the listed ref whose loose file under `--git-common-dir` has the same
///   (device, inode) as the file `full` opens. Exactly one match: that ref.
/// * No match, more than one, a stat error other than not-found, a probe
///   failure, or a non-unix host: Undetermined.
fn exact_ref(t: &Target, full: &str, namespace: &str) -> Determination<String> {
    macro_rules! known {
        ($e:expr) => {
            match $e {
                Determination::Known(v) => v,
                Determination::Undetermined(u) => return Determination::Undetermined(u),
            }
        };
    }
    let (_, listed) = known!(probe(
        t,
        &["for-each-ref", "--format=%(refname)", namespace],
        &[0]
    ));
    let listed: Vec<&str> = listed.lines().filter(|l| !l.is_empty()).collect();
    if listed.contains(&full) {
        return Determination::known(full.to_string());
    }
    let (_, common) = known!(probe(
        t,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        &[0]
    ));
    let common = std::path::PathBuf::from(common.trim());
    let folded = |why: String| {
        Determination::undetermined(format!(
            "`{full}` matches no existing ref byte-for-byte, so git reaches a ref through \
filesystem case/Unicode folding, and which one could not be established ({why})"
        ))
    };
    let Some(want) = known!(file_id(&common.join(full))) else {
        return folded("its loose ref file is absent".to_string());
    };
    let mut hits = Vec::new();
    for r in listed {
        if known!(file_id(&common.join(r))) == Some(want) {
            hits.push(r);
        }
    }
    match hits.as_slice() {
        [one] => Determination::known((*one).to_string()),
        [] => folded("no listed ref shares its loose ref file".to_string()),
        many => folded(format!("several listed refs share it: {}", many.join(", "))),
    }
}

/// (device, inode) of `p` (not following symlinks); `Known(None)` when it
/// does not exist; Undetermined on any other error or a non-unix host.
fn file_id(p: &std::path::Path) -> Determination<Option<(u64, u64)>> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        match std::fs::symlink_metadata(p) {
            Ok(m) => Determination::known(Some((m.dev(), m.ino()))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Determination::known(None),
            Err(e) => Determination::undetermined(format!("stat {}: {e}", p.display())),
        }
    }
    #[cfg(not(unix))]
    {
        Determination::undetermined(format!(
            "no file identity on this platform for {}",
            p.display()
        ))
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
        if (!remotes && op == default.name) || default.refs.contains(&full) {
            return default_ask(op, &default.name);
        }
        match ref_exists(&t, &full) {
            Determination::Known(false) => continue, // git deletes nothing for it
            Determination::Known(true) => {}
            Determination::Undetermined(u) => return ask(u.reason().as_str().to_string()),
        }
        // The ref git found may be ANOTHER ref reached through filesystem
        // folding (`-D Main` / `-D maſter` open the loose `main` / `master`
        // file on APFS), so the ref actually deleted is identified by
        // `exact_ref`, not by the spelling.
        let namespace = if remotes {
            "refs/remotes"
        } else {
            "refs/heads"
        };
        let full = match exact_ref(&t, &full, namespace) {
            Determination::Known(r) => r,
            Determination::Undetermined(u) => return ask(u.reason().as_str().to_string()),
        };
        if default.refs.contains(&full) {
            return default_ask(op, &default.name);
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
/// when, for every operand: [`linked_worktree_root`] accepts it (checked
/// locally before any git subprocess); `git -C <operand> rev-parse
/// --show-toplevel` and [`STATUS_ARGS`] both exit 0; the reported toplevel,
/// canonicalised, equals the canonicalised operand; the status output parses
/// as porcelain v1; and [`only_ignored_build_output`] accepts every entry
/// (nothing at all, or only ignored build-output directories). Anything else
/// is Ask. A relative operand is placed against the directory git runs in
/// (the cumulative `-C` directory, else the segment's base); the globals'
/// repository itself is never probed here.
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
        // Established locally BEFORE any git subprocess: run with `-C` on a
        // path that is not itself a linked-worktree root, git would walk up
        // and report the ENCLOSING worktree's state (user ruling
        // 2026-11-14: never consult a worktree other than the one named).
        if let Err(why) = linked_worktree_root(std::path::Path::new(&path)) {
            return ask(why);
        }
        let wt = Target {
            dir: path.clone(),
            git_dir: None,
            work_tree: None,
        };
        let toplevel = match probe(&wt, &["rev-parse", "--show-toplevel"], &[0]) {
            Determination::Known((_, out)) => out.trim_end_matches('\n').to_string(),
            Determination::Undetermined(u) => return ask(u.reason().as_str().to_string()),
        };
        if !toplevel.starts_with('/') || toplevel.contains('\n') {
            return ask("git printed a work-tree root that is not one absolute path".to_string());
        }
        match (
            std::fs::canonicalize(&path),
            std::fs::canonicalize(&toplevel),
        ) {
            (Ok(a), Ok(b)) if a == b => {}
            (Ok(_), Ok(_)) => {
                return ask(format!(
                    "git reports the work-tree root `{toplevel}` for `{op}`, which is not the \
operand itself, so its state is not the operand's"
                ))
            }
            (Err(e), _) | (_, Err(e)) => {
                return ask(format!(
                    "the operand and the work-tree root git reports could not be compared ({e})"
                ))
            }
        }
        let status = match probe(&wt, STATUS_ARGS, &[0]) {
            Determination::Known((_, out)) => out,
            Determination::Undetermined(u) => return ask(u.reason().as_str().to_string()),
        };
        let Some(entries) = crate::reversible::parse_status_z(&status) else {
            return ask("git status printed output not in the porcelain v1 shape".to_string());
        };
        if let Err(why) = only_ignored_build_output(std::path::Path::new(&toplevel), &entries) {
            return Decision::ask(format!(
                "git worktree remove --force `{op}`: {why}, which the forced removal (it deletes \
the whole directory) would destroy"
            ));
        }
    }
    Decision::Allow
}

/// `Ok(())` only when `path` (absolute, lexically normalised) is, by local
/// metadata alone, the root of a LINKED worktree: `path` itself is a real
/// directory (`symlink_metadata`, not a symlink) and `path/.git` is a regular
/// file (not a symlink, not a directory). A `.git` directory is a main
/// worktree (or a plain repository), which `git worktree remove` never
/// removes; anything else (absent, symlink, unreadable) is `Err`. No git
/// subprocess runs and no parent directory is consulted, and the `.git`
/// file's `gitdir:` pointer is not read or followed here.
fn linked_worktree_root(path: &std::path::Path) -> Result<(), String> {
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_dir() => {}
        Ok(_) => {
            return Err(format!(
                "`{}` is not a real directory (a symlink or a file)",
                path.display()
            ))
        }
        Err(e) => return Err(format!("stat {}: {e}", path.display())),
    }
    let dot_git = path.join(".git");
    match std::fs::symlink_metadata(&dot_git) {
        Ok(m) if m.file_type().is_file() => Ok(()),
        Ok(m) if m.file_type().is_dir() => Err(format!(
            "`{}` is a directory, so `{}` is a main worktree or repository, not a linked \
worktree root",
            dot_git.display(),
            path.display()
        )),
        Ok(_) => Err(format!(
            "`{}` is not a regular file, so `{}` is not recognised as a linked worktree root",
            dot_git.display(),
            path.display()
        )),
        Err(e) => Err(format!(
            "`{}` is not recognised as a linked worktree root (stat {}: {e})",
            path.display(),
            dot_git.display()
        )),
    }
}

/// The status probe for a force-removed worktree: every untracked file
/// individually, ignored paths as git matched them (an ignored directory is
/// one `dir/` entry, its contents not listed), submodule changes never hidden
/// by configuration.
const STATUS_ARGS: &[&str] = &[
    "status",
    "--porcelain=v1",
    "-z",
    "--ignored=matching",
    "--untracked-files=all",
    "--ignore-submodules=none",
];

/// `Ok(())` only when every `git status` entry of the worktree rooted at
/// `toplevel` is ignored (`!!`) regenerable build output; otherwise `Err`
/// naming the first entry that is not.
///
/// An ignored entry counts as build output only when ALL of these hold:
/// it is a directory entry (git prints it with a trailing `/`); its LAST
/// path component, relative to the worktree root, is exactly one of
/// [`crate::deletion::BUILD_OUTPUT_NAMES`] (the same list the rm class-4
/// `Allow` uses, so `rm -rf <wt>/target` and removing a worktree holding only
/// that directory agree); no component is empty, `.` or `..`; and every
/// prefix of it, the entry itself included, is a real directory right now
/// (`symlink_metadata`: a symlink, a file, or an unreadable path is not
/// build output). Matching is per component, never by substring:
/// `target-secrets/` and `x/target.env` are not build output. A file
/// ignored inside a build-named directory that is not itself ignored
/// (`target/key.env` under `*.env`) is listed by its own path and is not
/// build output either.
fn only_ignored_build_output(
    toplevel: &std::path::Path,
    entries: &[(String, String)],
) -> Result<(), String> {
    for (xy, path) in entries {
        if xy != "!!" {
            return Err(format!(
                "git status reports `{path}` there as uncommitted or untracked (`{xy}`)"
            ));
        }
        let not_build = || {
            Err(format!(
                "git status reports the ignored `{path}` there, which is not recognised \
regenerable build output"
            ))
        };
        let Some(dir) = path.strip_suffix('/') else {
            return not_build();
        };
        let comps: Vec<&str> = dir.split('/').collect();
        if comps
            .iter()
            .any(|c| c.is_empty() || *c == "." || *c == "..")
        {
            return not_build();
        }
        match comps.last() {
            Some(last) if crate::deletion::BUILD_OUTPUT_NAMES.contains(last) => {}
            _ => return not_build(),
        }
        let mut cur = toplevel.to_path_buf();
        for c in &comps {
            cur.push(c);
            match std::fs::symlink_metadata(&cur) {
                Ok(m) if m.file_type().is_dir() => {}
                _ => return not_build(),
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::only_ignored_build_output;

    fn e(xy: &str, p: &str) -> (String, String) {
        (xy.to_string(), p.to_string())
    }

    #[test]
    fn only_real_build_named_ignored_directories_pass() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path();
        std::fs::create_dir_all(root.join("target/debug")).unwrap();
        std::fs::create_dir_all(root.join("crates/x/target")).unwrap();
        std::fs::create_dir_all(root.join("target-secrets")).unwrap();
        std::fs::create_dir_all(root.join("x")).unwrap();
        std::fs::write(root.join("x/target.env"), "s").unwrap();
        std::fs::write(root.join(".env"), "s").unwrap();
        std::os::unix::fs::symlink("/tmp", root.join("build")).unwrap();
        std::os::unix::fs::symlink("/tmp", root.join("node_modules")).unwrap();

        assert!(only_ignored_build_output(root, &[]).is_ok());
        assert!(only_ignored_build_output(root, &[e("!!", "target/")]).is_ok());
        assert!(only_ignored_build_output(
            root,
            &[e("!!", "target/"), e("!!", "crates/x/target/")]
        )
        .is_ok());

        for bad in [
            vec![e("!!", ".env")],
            vec![e("!!", "target-secrets/")],
            vec![e("!!", "x/target.env")],
            vec![e("!!", "target/debug/")],
            vec![e("!!", "target")],
            vec![e("!!", "build/")],
            vec![e("!!", "build")],
            vec![e("!!", "node_modules/")],
            vec![e("!!", "absent/target/")],
            vec![e("!!", "../target/")],
            vec![e("!!", "./target/")],
            vec![e("!!", "target/"), e("??", "new.txt")],
            vec![e(" M", "tracked.txt"), e("!!", "target/")],
            vec![e("!!", "target/"), e("!!", ".env")],
        ] {
            assert!(
                only_ignored_build_output(root, &bad).is_err(),
                "{bad:?} must not be build output"
            );
        }
    }
}
