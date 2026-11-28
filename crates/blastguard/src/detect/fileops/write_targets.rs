//! Write targets of commands that are not `>` redirects, fed to the SAME
//! protected-path judgement a `cp` destination / `tee` target / `sort -o`
//! output goes through (`protected_path_block`, so symlinks are followed).
//!
//! Backlog 96faed52. Three shapes reached `Allow`:
//!
//! * `git diff|log|show --output=FILE` (also `--output FILE`), `git archive
//!   -o|--output FILE`, `git format-patch -o|--output-directory DIR`, `git
//!   bundle create FILE`, `git worktree add|move … PATH`: git itself writes
//!   (or creates a checkout at) the named path.
//! * `touch NAME` / `mkdir NAME` where `NAME` is a DANGLING symlink whose
//!   target is a protected path. `touch` creates the target through the link.
//!   `cp`/`tee` already resolved the link (`physical_landing`); the creation
//!   verbs had no arm and fell to the unknown-verb rule, which only reads the
//!   operand's text.
//!
//! Undetermined is never `Allow` (CLAUDE.md §3): a git write value that holds
//! an unexpanded `$`/backtick, or an option that is given without a value, is
//! an Ask, and so is a `-C`/`--work-tree` directory that cannot be placed
//! while a relative write target is being judged.

use harness_core::verdict::Determination;

use super::super::git::git_moved_dir;
use super::super::protected::{
    protected_glob_deny, protected_path_block, protected_path_lexical_block, system_landing_block,
};
use super::super::words::{first_shell_word, normalized_command};
use super::super::{has_unresolvable_expansion, Ctx, Decision};
use super::positional_operands;

/// A write target found in git's arguments: the value as spelled, or `None`
/// when the option was given without one (cannot be determined).
type Found = Option<String>;

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

/// `-o V` / `-oV`.
fn short_o_value(args: &[&str], i: usize) -> Option<(Found, usize)> {
    let t = args[i];
    if t.starts_with("--") || !t.starts_with("-o") {
        return None;
    }
    if t.len() > 2 {
        return Some((Some(t[2..].to_string()), 1));
    }
    match args.get(i + 1) {
        Some(v) => Some((Some((*v).to_string()), 2)),
        None => Some((None, 1)),
    }
}

/// The write targets of `git <sub> args…` as `(action, value)` pairs.
fn git_write_targets(sub: &str, args: &[&str]) -> Vec<(String, Found)> {
    let mut out = Vec::new();
    // `--` ends option parsing: nothing after it is an option.
    let opts_end = args.iter().position(|t| *t == "--").unwrap_or(args.len());
    let opts = &args[..opts_end];
    match sub {
        "diff" | "log" | "show" | "whatchanged" | "diff-tree" | "diff-index" | "diff-files"
        | "range-diff" | "format-patch" | "archive" => {
            let mut i = 0;
            while i < opts.len() {
                let found = long_value(opts, i, "output", 2)
                    .map(|r| (format!("git {sub} --output"), r))
                    .or_else(|| {
                        if sub == "format-patch" {
                            long_value(opts, i, "output-directory", 8)
                                .or_else(|| short_o_value(opts, i))
                                .map(|r| (format!("git {sub} -o"), r))
                        } else if sub == "archive" {
                            short_o_value(opts, i).map(|r| (format!("git {sub} -o"), r))
                        } else {
                            None
                        }
                    });
                match found {
                    Some((action, (v, n))) => {
                        out.push((action, v));
                        i += n;
                    }
                    None => i += 1,
                }
            }
        }
        "bundle" => {
            // `git bundle create FILE …`: FILE is the first operand after `create`.
            if let Some(p) = args.iter().position(|t| *t == "create") {
                let first = positional_operands(&args[p + 1..], &[])
                    .first()
                    .map(|f| (*f).to_string());
                out.push(("git bundle create".to_string(), first));
            }
        }
        "worktree" => {
            let Some(p) = args.iter().position(|t| !t.starts_with('-')) else {
                return out;
            };
            let ops = positional_operands(&args[p + 1..], &["-b", "-B", "--reason"]);
            match args[p] {
                // `add [opts] PATH [COMMIT-ISH]`: the first operand is the new checkout.
                "add" => out.push((
                    "git worktree add".to_string(),
                    ops.first().map(|s| (*s).to_string()),
                )),
                // `move WORKTREE NEW-PATH`: the second operand is the destination.
                "move" => out.push((
                    "git worktree move".to_string(),
                    ops.get(1).map(|s| (*s).to_string()),
                )),
                _ => {}
            }
        }
        _ => {}
    }
    out
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
    let targets = git_write_targets(sub, args);
    if targets.is_empty() {
        return Decision::Allow;
    }
    let moved = match git_moved_dir(ctx.scope, globals, ctx.base_for("git")) {
        Determination::Known(Some(dir)) => Ok(ctx.after_cd(&dir)),
        Determination::Known(None) => Ok(ctx.clone()),
        Determination::Undetermined(why) => Err(why.reason().as_str().to_string()),
    };
    for (action, value) in targets {
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
        if let Some(d) = protected_path_block(wctx, &action, &path)
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
/// is the protected path. `touch -h` / `--no-dereference` changes the link
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
        } else {
            protected_path_block(ctx, &action, &op)
        };
        if let Some(d) = hit.or_else(|| protected_glob_deny(&action, &op)) {
            return d;
        }
    }
    Decision::Allow
}
