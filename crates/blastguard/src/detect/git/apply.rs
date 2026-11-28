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
//! The words are read by the shared git argv parser (`git/argv.rs`: `--` ends
//! the options, any unique long prefix is the option — `--dir=` is
//! `--directory=` — a value option takes its value, an ambiguous prefix is
//! Ask). The directory git runs in (cwd or `-C`) is itself judged as a landing
//! directory (backlog f63b97b3).

use super::argv::{parse, Opt, Spec};
use super::{git_moved_dir, Ctx, Decision, Determination};

/// The options of `git apply`. Unknown ones are skipped (a patch file name
/// is the only operand and is never a write target), but a value option must
/// be listed so its value is not read as an option.
const APPLY_OPTS: &[Opt] = &[
    Opt::flag("stat", None),
    Opt::flag("numstat", None),
    Opt::flag("summary", None),
    Opt::flag("check", None),
    Opt::flag("index", None),
    Opt::flag("cached", None),
    Opt::flag("intent-to-add", Some('N')),
    Opt::flag("3way", Some('3')),
    Opt::flag("apply", None),
    Opt::flag("no-add", None),
    Opt::val("build-fake-ancestor", None),
    Opt::flag("reverse", Some('R')),
    Opt::flag("allow-binary-replacement", None),
    Opt::flag("binary", None),
    Opt::flag("reject", None),
    Opt::flag("unidiff-zero", None),
    Opt::flag("ignore-space-change", None),
    Opt::flag("ignore-whitespace", None),
    Opt::val("whitespace", None),
    Opt::val("exclude", None),
    Opt::val("include", None),
    Opt::flag("inaccurate-eof", None),
    Opt::flag("recount", None),
    Opt::val("directory", None),
    Opt::flag("unsafe-paths", None),
    Opt::flag("verbose", Some('v')),
    Opt::flag("quiet", Some('q')),
    Opt::flag("allow-overlap", None),
    Opt::sval("p", 'p'),
    Opt::sval("C", 'C'),
    Opt::sflag("z", 'z'),
];

pub(super) fn git_apply_verdict(ctx: &Ctx<'_>, globals: &[&str], args: &[&str]) -> Decision {
    let parsed = parse(
        args,
        &Spec {
            tables: &[APPLY_OPTS],
            lenient: true,
            digits: false,
        },
    );
    let read_only = ["check", "stat", "numstat", "summary"]
        .iter()
        .any(|n| parsed.has(n));
    // `--apply` re-enables the write a read-only flag switched off, and
    // `--reject` applies what it can.
    let writes = parsed.has("apply") || parsed.has("reject");
    if read_only && !writes && parsed.undetermined.is_empty() {
        return Decision::Allow;
    }
    if let Some(why) = parsed.undetermined.first() {
        return Decision::ask(format!(
            "git apply option {why} — blastguard cannot tell which option it names (it may be \
--directory or --apply) and refuses to guess"
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
            .or_else(|| super::super::protected::protected_ancestor_landing_ask(ctx, action, b))
        {
            return d;
        }
    }
    for hit in parsed.hits_of("directory") {
        let Some(value) = hit.value else {
            return Decision::ask(
                "git apply --directory has no value — blastguard refuses to guess where \
the patch is written",
            );
        };
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
    if parsed.has("unsafe-paths") {
        return Decision::ask(
            "git apply --unsafe-paths lets the patch write outside the work tree, and the patch \
text is not on the command line — blastguard cannot tell whether it lands on a protected \
gate/config path and refuses to guess",
        );
    }
    Decision::Allow
}
