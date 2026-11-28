//! Write targets of commands that are not `>` redirects, fed to the SAME
//! protected-path judgement a `cp` destination / `tee` target / `sort -o`
//! output goes through (`protected_path_block`, so symlinks are followed).
//!
//! Backlog 96faed52 / 4e7fc8e2. Shapes that reached `Allow`:
//!
//! * `git <any subcommand> --output=FILE` (also `--output FILE`; `diff`, `log`,
//!   `show`, `stash show -p`, `reflog`, an alias such as `-c alias.dd=diff dd`),
//!   `git archive -o|--output FILE`, `git format-patch -o|--output-directory
//!   DIR`, `git bundle create FILE`, `git config --file|-f FILE` (a write form),
//!   `git worktree add|move … PATH`, `git checkout-index --prefix=DIR/`, `git
//!   init [DIR]` / `--separate-git-dir` / `--git-dir=DIR init`, `git clone SRC
//!   DIR`: git itself writes (or creates a checkout / repository at) the named
//!   path.
//! * `touch NAME` / `mkdir NAME` where `NAME` is a DANGLING symlink whose
//!   target is a protected path (`touch` creates the target through the link),
//!   or where `NAME` lands in a system directory like a `cp` destination.
//!
//! Every git target is relative to the process cwd, which only `-C` moves.
//! `--work-tree` does NOT: `git --work-tree=W diff --output=REL` writes `REL`
//! under the cwd (observed with real git), so `--work-tree` is dropped from the
//! globals before the base is placed.
//!
//! A DIRECTORY target (`format-patch -o`, `worktree add`, `--prefix`, `init`,
//! `clone`) is judged as a landing: the directory itself being protected, or
//! holding protected paths (`~/.claude`), or being a system directory counts,
//! not only a file named under it ([`dir_block`]).
//!
//! Undetermined is never `Allow` (CLAUDE.md §3): a git write value that holds
//! an unexpanded `$`/backtick, or an option that is given without a value, is
//! an Ask, and so is a `-C` directory that cannot be placed while a relative
//! write target is being judged.

use harness_core::verdict::Determination;

use super::super::git::git_moved_dir;
use super::super::protected::{
    physical_landing, protected_glob_deny, protected_landing_block, protected_path_block,
    protected_path_lexical_block, system_landing_block, system_path_block,
};
use super::super::words::{first_shell_word, normalized_command};
use super::super::{has_unresolvable_expansion, Ctx, Decision};
use super::positional_operands;

/// A write target found in git's arguments: the value as spelled, or `None`
/// when the option was given without one (cannot be determined).
type Found = Option<String>;

/// What git creates at the named path.
#[derive(Clone, Copy, PartialEq)]
enum Kind {
    /// One file is written (`--output=F`, `bundle create F`, `config --file F`).
    File,
    /// A directory is populated or created (`format-patch -o D`, `worktree
    /// add D`, `--prefix=D/`, `init D`, `clone S D`).
    Dir,
}

struct Target {
    action: String,
    value: Found,
    kind: Kind,
}

/// `--NAME[=V]` / `--NAME V` where `NAME` is `full` or an abbreviation of it
/// at least `min` characters long (git's parse-options accepts unambiguous
/// prefixes). Returns the value and how many words were consumed.
fn long_value(args: &[&str], i: usize, full: &str, min: usize) -> Option<(Found, usize)> {
    let body = args[i].strip_prefix("--")?;
    let (name, inline) = match body.split_once('=') {
        Some((n, v)) => (n, Some(v)),
        None => (body, None),
    };
    if name.len() < min || !full.starts_with(name) {
        return None;
    }
    match inline {
        Some(v) => Some((Some(v.to_string()), 1)),
        None => match args.get(i + 1) {
            Some(v) => Some((Some((*v).to_string()), 2)),
            None => Some((None, 1)),
        },
    }
}

/// `-X V` / `-XV` for the short letter `letter`.
fn short_value(args: &[&str], i: usize, letter: char) -> Option<(Found, usize)> {
    let t = args[i];
    let rest = t.strip_prefix('-')?;
    if t.starts_with("--") || !rest.starts_with(letter) {
        return None;
    }
    let tail = &rest[letter.len_utf8()..];
    if !tail.is_empty() {
        return Some((Some(tail.to_string()), 1));
    }
    match args.get(i + 1) {
        Some(v) => Some((Some((*v).to_string()), 2)),
        None => Some((None, 1)),
    }
}

/// Value options of `git clone` that consume the next word.
const CLONE_VALUE_FLAGS: &[&str] = &[
    "-b",
    "--branch",
    "--depth",
    "-o",
    "--origin",
    "-c",
    "--config",
    "--reference",
    "--reference-if-able",
    "--template",
    "-j",
    "--jobs",
    "--filter",
    "-u",
    "--upload-pack",
    "--server-option",
    "--shallow-since",
    "--shallow-exclude",
    "--separate-git-dir",
    "--bundle-uri",
    "--ref-format",
    "--revision",
];

/// Value options of `git init` that consume the next word.
const INIT_VALUE_FLAGS: &[&str] = &[
    "-b",
    "--initial-branch",
    "--template",
    "--separate-git-dir",
    "--object-format",
    "--ref-format",
];

/// Options that make `git config` only READ the file it is given.
fn config_is_read_only(args: &[&str]) -> bool {
    const READ: &[&str] = &[
        "--get",
        "--get-all",
        "--get-regexp",
        "--get-urlmatch",
        "--get-color",
        "--get-colorbool",
        "--list",
        "-l",
    ];
    if args.iter().any(|t| READ.contains(t)) {
        return true;
    }
    matches!(
        positional_operands(args, &["-f", "--file", "--blob", "--type", "--default"])
            .first()
            .copied(),
        Some("get" | "list")
    )
}

/// `--git-dir=V` / `--git-dir V` among git's global options.
fn global_git_dir(globals: &[&str]) -> Vec<Found> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < globals.len() {
        match long_value(globals, i, "git-dir", 7) {
            Some((v, n)) => {
                out.push(v);
                i += n;
            }
            None => i += 1,
        }
    }
    out
}

/// The write targets of `git <sub> args…`. One lib for every git shape: the
/// per-subcommand arms name the verbs whose targets are not `--output`, and
/// the fall-through scans `--output` for ANY other subcommand (a subcommand
/// git ships, an alias, one this table has never heard of) — an unknown
/// subcommand's `--output` is treated as a write.
fn git_write_targets(globals: &[&str], sub: &str, args: &[&str]) -> Vec<Target> {
    let mut out = Vec::new();
    let t = |action: String, value: Found, kind: Kind| Target {
        action,
        value,
        kind,
    };
    // `--` ends option parsing: nothing after it is an option.
    let opts_end = args.iter().position(|a| *a == "--").unwrap_or(args.len());
    let opts = &args[..opts_end];
    match sub {
        "bundle" => {
            // `git bundle create FILE …`: FILE is the first operand after `create`.
            if let Some(p) = args.iter().position(|a| *a == "create") {
                let first = positional_operands(&args[p + 1..], &[])
                    .first()
                    .map(|f| (*f).to_string());
                out.push(t("git bundle create".to_string(), first, Kind::File));
            }
        }
        "worktree" => {
            let Some(p) = args.iter().position(|a| !a.starts_with('-')) else {
                return out;
            };
            let ops = positional_operands(&args[p + 1..], &["-b", "-B", "--reason"]);
            match args[p] {
                // `add [opts] PATH [COMMIT-ISH]`: the first operand is the new checkout.
                "add" => out.push(t(
                    "git worktree add".to_string(),
                    ops.first().map(|s| (*s).to_string()),
                    Kind::Dir,
                )),
                // `move WORKTREE NEW-PATH`: the second operand is the destination.
                "move" => out.push(t(
                    "git worktree move".to_string(),
                    ops.get(1).map(|s| (*s).to_string()),
                    Kind::Dir,
                )),
                _ => {}
            }
        }
        "config" => {
            if config_is_read_only(opts) {
                return out;
            }
            let mut i = 0;
            while i < opts.len() {
                match long_value(opts, i, "file", 3).or_else(|| short_value(opts, i, 'f')) {
                    Some((v, n)) => {
                        out.push(t("git config --file".to_string(), v, Kind::File));
                        i += n;
                    }
                    None => i += 1,
                }
            }
        }
        "checkout-index" => {
            let mut i = 0;
            while i < opts.len() {
                match long_value(opts, i, "prefix", 3) {
                    Some((v, n)) => {
                        out.push(t("git checkout-index --prefix".to_string(), v, Kind::Dir));
                        i += n;
                    }
                    None => i += 1,
                }
            }
        }
        "init" => {
            for v in global_git_dir(globals) {
                out.push(t("git --git-dir init".to_string(), v, Kind::Dir));
            }
            let mut i = 0;
            while i < opts.len() {
                match long_value(opts, i, "separate-git-dir", 3) {
                    Some((v, n)) => {
                        out.push(t("git init --separate-git-dir".to_string(), v, Kind::Dir));
                        i += n;
                    }
                    None => i += 1,
                }
            }
            if let Some(d) = positional_operands(opts, INIT_VALUE_FLAGS).first() {
                out.push(t("git init".to_string(), Some((*d).to_string()), Kind::Dir));
            }
        }
        "clone" => {
            let mut i = 0;
            while i < opts.len() {
                match long_value(opts, i, "separate-git-dir", 3) {
                    Some((v, n)) => {
                        out.push(t("git clone --separate-git-dir".to_string(), v, Kind::Dir));
                        i += n;
                    }
                    None => i += 1,
                }
            }
            // `git clone SRC DIR`: the second operand is the destination (a
            // lone SRC names a directory derived from it, never one the
            // command line spells).
            if let Some(d) = positional_operands(opts, CLONE_VALUE_FLAGS).get(1) {
                out.push(t(
                    "git clone".to_string(),
                    Some((*d).to_string()),
                    Kind::Dir,
                ));
            }
        }
        _ => {
            let mut i = 0;
            while i < opts.len() {
                let found = long_value(opts, i, "output", 2)
                    .map(|r| (format!("git {sub} --output"), r, Kind::File))
                    .or_else(|| {
                        if sub == "format-patch" {
                            long_value(opts, i, "output-directory", 8)
                                .or_else(|| short_value(opts, i, 'o'))
                                .map(|r| (format!("git {sub} -o"), r, Kind::Dir))
                        } else if sub == "archive" {
                            short_value(opts, i, 'o')
                                .map(|r| (format!("git {sub} -o"), r, Kind::File))
                        } else {
                            None
                        }
                    });
                match found {
                    Some((action, (v, n), kind)) => {
                        out.push(t(action, v, kind));
                        i += n;
                    }
                    None => i += 1,
                }
            }
        }
    }
    out
}

/// `globals` without `--work-tree` (it does not move the process cwd, which is
/// what git resolves a relative write target against).
fn without_work_tree<'a>(globals: &[&'a str]) -> Vec<&'a str> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < globals.len() {
        let g = globals[i];
        if g == "--work-tree" {
            i += 2;
        } else if g.starts_with("--work-tree=") {
            i += 1;
        } else {
            out.push(g);
            i += 1;
        }
    }
    out
}

/// A directory landing: the directory as a path (`protected_path_block`), the
/// directory ITSELF being / holding protected paths, at the spelling and at
/// the place its symlinks lead (`protected_landing_block`), and a system
/// directory (`system_landing_block`).
fn dir_block(ctx: &Ctx<'_>, action: &str, dir: &str) -> Option<Decision> {
    if let Some(d) = protected_path_block(ctx, action, dir) {
        return Some(d);
    }
    if let Some(d) = protected_landing_block(ctx, action, dir) {
        return Some(d);
    }
    if let Some(Determination::Known(real)) = physical_landing(dir, ctx.raw_base.as_deref()) {
        if let Some(d) = protected_landing_block(ctx, action, &real) {
            return Some(d);
        }
    }
    None
}

/// The verdict for a git subcommand's write targets, `Allow` when it has none.
/// `globals` are the words before the subcommand (`-C DIR` moves where a
/// relative target is placed), `args` the words after it.
pub(in crate::detect) fn git_write_target_verdict(
    ctx: &Ctx<'_>,
    globals: &[&str],
    sub: &str,
    args: &[&str],
) -> Decision {
    let targets = git_write_targets(globals, sub, args);
    if targets.is_empty() {
        return Decision::Allow;
    }
    let cwd_globals = without_work_tree(globals);
    let moved = match git_moved_dir(ctx.scope, &cwd_globals, ctx.base_for("git")) {
        Determination::Known(Some(dir)) => Ok(ctx.after_cd(&dir)),
        Determination::Known(None) => Ok(ctx.clone()),
        Determination::Undetermined(why) => Err(why.reason().as_str().to_string()),
    };
    for Target {
        action,
        value,
        kind,
    } in targets
    {
        let Some(raw) = value else {
            return Decision::ask(format!(
                "{action} has no value, so blastguard cannot tell which path git writes — it \
refuses to guess"
            ));
        };
        let path = first_shell_word(&raw).unwrap_or_else(|| raw.clone());
        if path.is_empty() || has_unresolvable_expansion(&path) {
            return Decision::ask(format!(
                "{action} names `{raw}`, a path whose value is only known at run time — \
blastguard cannot tell whether git writes a protected gate/config path, so it refuses to guess"
            ));
        }
        let wctx = match &moved {
            Ok(c) => c,
            Err(why) => {
                // Only an absolute target is independent of the moved directory.
                if !path.starts_with('/') {
                    return Decision::ask(format!(
                        "{action} names the relative path `{path}`, but the directory git runs \
in could not be placed ({why}) — blastguard refuses to guess where it is written"
                    ));
                }
                ctx
            }
        };
        let protected = match kind {
            Kind::File => protected_path_block(wctx, &action, &path),
            Kind::Dir => dir_block(wctx, &action, &path),
        };
        if let Some(d) = protected
            .or_else(|| protected_glob_deny(&action, &path))
            .or_else(|| system_landing_block(wctx, &action, &path))
        {
            return d;
        }
    }
    Decision::Allow
}

/// Creation verbs (`touch`, `mkdir`): every operand is judged through its
/// symlinks like a `cp` destination, so a dangling link onto a protected path
/// is the protected path, and an operand in a system directory is the same
/// Deny a `cp` there is. `touch -h` / `--no-dereference` changes the link
/// itself, never its target, and is judged as spelled.
pub(super) fn analyze_create(cmd: &str, rest: &[&str], ctx: &Ctx<'_>) -> Decision {
    // Short letters / long options that consume a value (reference file,
    // timestamp, mode, …) — never a created path.
    let (short_value, long_with_value): (&[char], &[&str]) = match normalized_command(cmd).as_str()
    {
        "touch" => (&['d', 'r', 't', 'A'], &["--date", "--reference", "--time"]),
        _ => (&['m'], &["--mode"]),
    };
    let mut no_deref = false;
    let mut operands: Vec<&str> = Vec::new();
    let mut i = 0;
    let mut after_dd = false;
    while i < rest.len() {
        let t = rest[i];
        if after_dd || t == "-" || !t.starts_with('-') {
            if !t.is_empty() {
                operands.push(t);
            }
            i += 1;
        } else if t == "--" {
            after_dd = true;
            i += 1;
        } else if let Some(body) = t.strip_prefix("--") {
            if body == "no-dereference" {
                no_deref = true;
            }
            // `--opt V` consumes the next word; `--opt=V` is self-contained.
            i += if long_with_value.contains(&t) { 2 } else { 1 };
        } else {
            // A short cluster: the first value letter ends it, taking the rest
            // of the word as its value or, if last, the next word.
            let cluster = &t[1..];
            let mut skip = 1;
            for (pos, c) in cluster.char_indices() {
                if c == 'h' {
                    no_deref = true;
                }
                if short_value.contains(&c) {
                    if pos + c.len_utf8() == cluster.len() {
                        skip = 2;
                    }
                    break;
                }
            }
            i += skip;
        }
    }
    let action = format!("{cmd} target");
    for op in operands {
        let op = first_shell_word(op).unwrap_or_else(|| op.to_string());
        let hit = if no_deref {
            protected_path_lexical_block(ctx, &action, &op)
                .or_else(|| system_path_block(&action, &op, ctx))
        } else {
            protected_path_block(ctx, &action, &op)
                .or_else(|| system_landing_block(ctx, &action, &op))
        };
        if let Some(d) = hit.or_else(|| protected_glob_deny(&action, &op)) {
            return d;
        }
    }
    Decision::Allow
}
