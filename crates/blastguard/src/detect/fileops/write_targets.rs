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
//!   [DIR]`: git itself writes (or creates a checkout / repository at) the named
//!   path.
//! * `touch NAME` / `mkdir NAME` where `NAME` is a DANGLING symlink whose
//!   target is a protected path (`touch` creates the target through the link),
//!   or where `NAME` lands in a system directory like a `cp` destination.
//!
//! Every git subcommand's words are read by ONE parser (`git/argv.rs`): `--`
//! ends the options, a long option is its unique prefix, a clustered short
//! flag (`-ko DIR`) is taken apart by the option table, a value option takes
//! its value. A word the table cannot place makes the reading undetermined,
//! and then EVERY word that could be a path is judged (as an Ask, never a
//! guess that nothing is written).
//!
//! A relative git target is relative to the process cwd, which only `-C`
//! moves. `--work-tree` does NOT move it: `git --work-tree=W diff
//! --output=REL` writes `REL` under the cwd (observed with real git), so
//! `--work-tree` is dropped from the globals before that base is placed. The
//! one exception is `checkout-index`, which git runs from the work tree top:
//! its `--prefix=REL` is relative to `--work-tree` (observed with real git),
//! and so is the tree it writes when `--work-tree` is given. An env prefix
//! `GIT_DIR=D` / `GIT_WORK_TREE=W` is the env twin of `--git-dir` /
//! `--work-tree` and is judged as a landing too.
//!
//! A DIRECTORY target (`format-patch -o`, `worktree add`, `--prefix`, `init`,
//! `clone`) is judged as a landing: the directory itself being protected, or
//! holding protected paths (`~/.claude`), or being a system directory counts,
//! not only a file named under it ([`dir_block`]). `git init` without an
//! operand creates the repository IN the (`-C`-moved) cwd, so the cwd itself is
//! the landing; `git clone SRC` without a destination clones into the
//! directory named after SRC.
//!
//! Undetermined is never `Allow` (CLAUDE.md §3): a git write value that holds
//! an unexpanded `$`/backtick, or an option that is given without a value, is
//! an Ask, and so is a `-C` directory that cannot be placed while a relative
//! write target is being judged.

mod unmodelled;

use harness_core::verdict::Determination;

use super::super::git::argv::{parse, Opt, Parsed, Spec};
use super::super::git::{git_global_config_assignments, git_moved_dir};
use super::super::protected::{
    physical_landing, protected_glob_deny, protected_landing_block, protected_path_block,
    protected_path_lexical_block, system_landing_block, system_path_block,
};
use super::super::words::{first_shell_word, normalized_command};
use super::super::{has_unresolvable_expansion, Ctx, Decision};

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

/// Which directory a relative target is relative to.
#[derive(Clone, Copy, PartialEq)]
enum Base {
    /// The process cwd, moved by `-C`.
    Cwd,
    /// The work tree: `--work-tree` / `GIT_WORK_TREE` when given (checkout-index).
    WorkTree,
}

/// How sure the reading of a target is.
#[derive(Clone, Copy, PartialEq)]
enum Mode {
    /// Read from the command line: an unresolved or missing value is an Ask.
    Exact,
    /// Judged only when it is a placeable literal (an env prefix on a
    /// subcommand that may not write there at all): never an Ask for being
    /// unresolved.
    Soft,
    /// A word of a command line that could not be read with certainty: a block
    /// is an Ask, an unresolved word is skipped.
    Guess,
}

struct Target {
    action: String,
    value: Found,
    kind: Kind,
    base: Base,
    mode: Mode,
}

impl Target {
    fn exact(action: &str, value: Found, kind: Kind) -> Target {
        Target {
            action: action.to_string(),
            value,
            kind,
            base: Base::Cwd,
            mode: Mode::Exact,
        }
    }
}

/// The git environment variables of the command's env prefix that move where
/// git writes.
#[derive(Default)]
struct GitEnv {
    git_dir: Option<String>,
    work_tree: Option<String>,
}

fn git_env(words: &[&str]) -> GitEnv {
    let mut out = GitEnv::default();
    for w in words {
        let Some((name, value)) = w.split_once('=') else {
            continue;
        };
        let slot = match name {
            "GIT_DIR" => &mut out.git_dir,
            "GIT_WORK_TREE" => &mut out.work_tree,
            _ => continue,
        };
        *slot = Some(first_shell_word(value).unwrap_or_else(|| value.to_string()));
    }
    out
}

// ---- option tables (the shared parser reads the words against these) ----

/// Subcommands without a table: only `--output` is looked for.
const GENERIC: &[Opt] = &[Opt::val("output", None)];
const ARCHIVE: &[Opt] = &[Opt::val("output", Some('o'))];
/// `git push`: only the options whose value is the NEXT word matter, so the
/// remote is read as the first operand.
const PUSH: &[Opt] = &[
    Opt::val("repo", None),
    Opt::val("receive-pack", None),
    Opt::val("exec", None),
    Opt::val("push-option", Some('o')),
    Opt::inline("recurse-submodules"),
    Opt::inline("force-with-lease"),
    Opt::inline("signed"),
];

const INIT: &[Opt] = &[
    Opt::flag("quiet", Some('q')),
    Opt::flag("bare", None),
    Opt::val("template", None),
    Opt::val("separate-git-dir", None),
    Opt::val("object-format", None),
    Opt::val("ref-format", None),
    Opt::val("initial-branch", Some('b')),
    Opt::inline("shared"),
];

const CLONE: &[Opt] = &[
    Opt::flag("local", Some('l')),
    Opt::flag("hardlinks", None),
    Opt::flag("shared", Some('s')),
    Opt::val("reference", None),
    Opt::val("reference-if-able", None),
    Opt::flag("dissociate", None),
    Opt::flag("quiet", Some('q')),
    Opt::flag("verbose", Some('v')),
    Opt::flag("progress", None),
    Opt::flag("checkout", Some('n')),
    Opt::flag("bare", None),
    Opt::flag("mirror", None),
    Opt::val("origin", Some('o')),
    Opt::val("branch", Some('b')),
    Opt::val("upload-pack", Some('u')),
    Opt::val("template", None),
    Opt::val("config", Some('c')),
    Opt::val("depth", None),
    Opt::val("shallow-since", None),
    Opt::val("shallow-exclude", None),
    Opt::flag("single-branch", None),
    Opt::flag("tags", None),
    Opt::inline("recurse-submodules"),
    Opt::inline("recursive"),
    Opt::flag("shallow-submodules", None),
    Opt::flag("remote-submodules", None),
    Opt::val("separate-git-dir", None),
    Opt::val("ref-format", None),
    Opt::val("jobs", Some('j')),
    Opt::flag("sparse", None),
    Opt::val("filter", None),
    Opt::flag("also-filter-submodules", None),
    Opt::val("server-option", None),
    Opt::val("bundle-uri", None),
    Opt::val("revision", None),
    Opt::flag("reject-shallow", None),
    Opt::sflag("ipv4", '4'),
    Opt::sflag("ipv6", '6'),
];

const WORKTREE: &[Opt] = &[
    Opt::flag("force", Some('f')),
    Opt::sval("b", 'b'),
    Opt::sval("B", 'B'),
    Opt::flag("detach", Some('d')),
    Opt::flag("checkout", None),
    Opt::flag("lock", None),
    Opt::val("reason", None),
    Opt::flag("quiet", Some('q')),
    Opt::flag("track", None),
    Opt::flag("guess-remote", None),
    Opt::flag("orphan", None),
    Opt::flag("porcelain", None),
    Opt::val("expire", None),
    Opt::flag("dry-run", Some('n')),
    Opt::flag("verbose", Some('v')),
    Opt::flag("relative-paths", None),
    Opt::sflag("z", 'z'),
];

const BUNDLE: &[Opt] = &[
    Opt::flag("quiet", Some('q')),
    Opt::flag("progress", None),
    Opt::val("version", None),
    Opt::flag("all", None),
    Opt::flag("stdin", None),
];

const CONFIG: &[Opt] = &[
    Opt::flag("global", None),
    Opt::flag("system", None),
    Opt::flag("local", None),
    Opt::flag("worktree", None),
    Opt::val("file", Some('f')),
    Opt::val("blob", None),
    Opt::val("type", None),
    Opt::flag("bool", None),
    Opt::flag("int", None),
    Opt::flag("bool-or-int", None),
    Opt::flag("path", None),
    Opt::flag("expiry-date", None),
    Opt::flag("null", Some('z')),
    Opt::flag("name-only", None),
    Opt::flag("show-origin", None),
    Opt::flag("show-scope", None),
    Opt::flag("includes", None),
    Opt::val("default", None),
    Opt::val("comment", None),
    Opt::flag("replace-all", None),
    Opt::flag("add", None),
    Opt::flag("unset", None),
    Opt::flag("unset-all", None),
    Opt::flag("get", None),
    Opt::flag("get-all", None),
    Opt::flag("get-regexp", None),
    Opt::flag("get-urlmatch", None),
    Opt::flag("get-color", None),
    Opt::flag("get-colorbool", None),
    Opt::flag("list", Some('l')),
    Opt::flag("edit", Some('e')),
    Opt::flag("rename-section", None),
    Opt::flag("remove-section", None),
    Opt::flag("fixed-value", None),
];

/// The `git config` options that only READ.
const CONFIG_READS: &[&str] = &[
    "get",
    "get-all",
    "get-regexp",
    "get-urlmatch",
    "get-color",
    "get-colorbool",
    "list",
];

const CHECKOUT_INDEX: &[Opt] = &[
    Opt::flag("index", Some('u')),
    Opt::flag("quiet", Some('q')),
    Opt::flag("force", Some('f')),
    Opt::flag("all", Some('a')),
    Opt::flag("no-create", Some('n')),
    Opt::val("prefix", None),
    Opt::val("stage", None),
    Opt::flag("temp", None),
    Opt::flag("ignore-skip-worktree-bits", None),
    Opt::sflag("z", 'z'),
    Opt::flag("stdin", None),
];

const FORMAT_PATCH: &[Opt] = &[
    Opt::val("output-directory", Some('o')),
    Opt::val("output", None),
    Opt::flag("stdout", None),
    Opt::flag("keep-subject", Some('k')),
    Opt::flag("numbered", Some('n')),
    Opt::flag("signoff", Some('s')),
    Opt::val("start-number", None),
    Opt::flag("numbered-files", None),
    Opt::val("subject-prefix", None),
    Opt::flag("rfc", None),
    Opt::val("reroll-count", Some('v')),
    Opt::val("filename-max-length", None),
    Opt::flag("cover-letter", None),
    Opt::val("cover-from-description", None),
    Opt::inline("thread"),
    Opt::val("in-reply-to", None),
    Opt::val("to", None),
    Opt::val("cc", None),
    Opt::inline("from"),
    Opt::val("add-header", None),
    Opt::inline("attach"),
    Opt::inline("inline"),
    Opt::val("signature", None),
    Opt::val("signature-file", None),
    Opt::flag("quiet", Some('q')),
    Opt::val("base", None),
    Opt::flag("progress", None),
    Opt::val("interdiff", None),
    Opt::val("range-diff", None),
    Opt::val("creation-factor", None),
    Opt::flag("root", None),
    Opt::flag("no-stat", Some('p')),
    Opt::inline("stat"),
    Opt::flag("numstat", None),
    Opt::flag("shortstat", None),
    Opt::flag("summary", None),
    Opt::flag("minimal", None),
    Opt::flag("patience", None),
    Opt::flag("histogram", None),
    Opt::val("diff-algorithm", None),
    Opt::val("unified", Some('U')),
    Opt::flag("full-index", None),
    Opt::flag("binary", None),
    Opt::flag("ignore-space-change", Some('b')),
    Opt::flag("ignore-all-space", Some('w')),
    Opt::flag("ignore-space-at-eol", None),
    Opt::flag("text", Some('a')),
    Opt::flag("function-context", Some('W')),
    Opt::flag("irreversible-delete", Some('D')),
    Opt::flag("renames", None),
    Opt::inline("find-renames"),
    Opt::inline("find-copies"),
    Opt::flag("find-copies-harder", None),
    Opt::inline("break-rewrites"),
    Opt::sinline("M", 'M'),
    Opt::sinline("C", 'C'),
    Opt::sinline("B", 'B'),
    Opt::flag("ignore-if-in-upstream", None),
    Opt::inline("notes"),
    Opt::flag("merges", None),
    Opt::flag("first-parent", None),
    Opt::flag("all", None),
    Opt::flag("reverse", None),
    Opt::val("max-count", None),
    Opt::val("since", None),
    Opt::val("until", None),
    Opt::val("author", None),
    Opt::val("committer", None),
    Opt::val("grep", None),
    Opt::inline("color"),
    Opt::sflag("N", 'N'),
    Opt::flag("ext-diff", None),
    Opt::flag("prefix", None),
    Opt::val("src-prefix", None),
    Opt::val("dst-prefix", None),
    Opt::val("line-prefix", None),
    Opt::val("inter-hunk-context", None),
    Opt::val("diff-filter", None),
];

/// Every word of `args` that could be a path: operands, the value of an
/// `--opt=V`, the glued value of a short option, and the words after `--`.
fn guess_words(args: &[&str]) -> Vec<String> {
    let mut out = Vec::new();
    for a in args {
        if let Some(rest) = a.strip_prefix("--") {
            if let Some((_, v)) = rest.split_once('=') {
                out.push(v.to_string());
            }
        } else if a.starts_with('-') {
            if a.len() > 2 {
                out.push(a[2..].to_string());
            }
        } else {
            out.push((*a).to_string());
        }
    }
    out
}

/// `src` named by `git clone`, as the directory git derives from it.
fn clone_derived_name(src: &str, bare: bool) -> Option<String> {
    let s = src.trim_end_matches('/');
    let s = s
        .strip_suffix(".git")
        .or_else(|| s.strip_suffix(".bundle"))
        .unwrap_or(s);
    let s = s.trim_end_matches('/');
    let name = s.rsplit(['/', ':']).next().unwrap_or("");
    if name.is_empty() {
        return None;
    }
    Some(if bare {
        format!("{name}.git")
    } else {
        name.to_string()
    })
}

/// `--git-dir=V` / `--git-dir V` among git's global options.
fn global_git_dir(globals: &[&str]) -> Vec<Found> {
    let parsed = parse(
        globals,
        &Spec {
            tables: &[&[Opt::val("git-dir", None)]],
            lenient: true,
            digits: false,
        },
    );
    parsed
        .hits_of("git-dir")
        .map(|h| h.value.map(str::to_string))
        .collect()
}

/// The words after the subcommand, read against `table`; `guess` collects
/// every possible path word when the reading is undetermined.
fn read<'a>(args: &[&'a str], table: &[Opt], digits: bool) -> Parsed<'a> {
    parse(
        args,
        &Spec {
            tables: &[table],
            lenient: false,
            digits,
        },
    )
}

fn add_guesses(out: &mut Vec<Target>, action: &str, p: &Parsed<'_>, args: &[&str], kind: Kind) {
    if p.undetermined.is_empty() {
        return;
    }
    for w in guess_words(args) {
        out.push(Target {
            action: format!("{action} (unreadable command line: {})", p.undetermined[0]),
            value: Some(w),
            kind,
            base: Base::Cwd,
            mode: Mode::Guess,
        });
    }
}

/// The write targets of `git <sub> args…`. One lib for every git shape: the
/// per-subcommand arms name the verbs whose targets are not `--output`, and
/// the fall-through scans `--output` for ANY other subcommand (a subcommand
/// git ships, an alias, one this table has never heard of) — an unknown
/// subcommand's `--output` is treated as a write.
fn git_write_targets(
    globals: &[&str],
    env: &GitEnv,
    explicit_work_tree: bool,
    sub: &str,
    args: &[&str],
    judge_unmodelled: bool,
) -> Vec<Target> {
    let mut out = Vec::new();
    // An env prefix repoints git's repository / work tree. `init` creates the
    // repository there; any other subcommand may write into it, so it is
    // judged too, but only as a placeable literal.
    let git_dirs: Vec<Found> = global_git_dir(globals)
        .into_iter()
        .chain(env.git_dir.clone().map(Some))
        .collect();
    if sub == "init" {
        for v in &git_dirs {
            out.push(Target::exact("git --git-dir init", v.clone(), Kind::Dir));
        }
    } else if let Some(d) = &env.git_dir {
        out.push(Target {
            action: "GIT_DIR".to_string(),
            value: Some(d.clone()),
            kind: Kind::Dir,
            base: Base::Cwd,
            mode: Mode::Soft,
        });
    }
    if let Some(wt) = &env.work_tree {
        out.push(Target {
            action: "GIT_WORK_TREE".to_string(),
            value: Some(wt.clone()),
            kind: Kind::Dir,
            base: Base::Cwd,
            mode: Mode::Soft,
        });
    }
    // The global twins of the env prefix (`--work-tree=W`, `-c core.worktree=W`)
    // move the tree every subcommand that is not read-only may write
    // (`mv`, `stash`, `am`, `read-tree -u`, `rm -f`, …): the work tree is judged
    // as a landing, exactly as `GIT_WORK_TREE` is. `checkout-index` has its own
    // arm below.
    if sub != "checkout-index" && !unmodelled::is_read_only(sub) {
        if globals
            .iter()
            .any(|g| *g == "--work-tree" || g.starts_with("--work-tree="))
        {
            out.push(Target {
                action: "git --work-tree".to_string(),
                value: Some(".".to_string()),
                kind: Kind::Dir,
                base: Base::WorkTree,
                mode: Mode::Soft,
            });
        }
        for a in git_global_config_assignments(globals) {
            let a = first_shell_word(a).unwrap_or_else(|| a.to_string());
            if let Some((key, value)) = a.split_once('=') {
                if key.eq_ignore_ascii_case("core.worktree") {
                    out.push(Target {
                        action: "git -c core.worktree".to_string(),
                        value: Some(value.to_string()),
                        kind: Kind::Dir,
                        base: Base::Cwd,
                        mode: Mode::Soft,
                    });
                }
            }
        }
    }
    match sub {
        "bundle" => {
            // `git bundle create FILE …`: FILE is the first operand after `create`.
            let p = read(args, BUNDLE, false);
            if let Some(i) = p.operands.iter().position(|a| *a == "create") {
                out.push(Target::exact(
                    "git bundle create",
                    p.operands.get(i + 1).map(|f| (*f).to_string()),
                    Kind::File,
                ));
            }
            add_guesses(&mut out, "git bundle", &p, args, Kind::File);
        }
        "worktree" => {
            let p = read(args, WORKTREE, false);
            match p.operands.first().copied() {
                // `add [opts] PATH [COMMIT-ISH]`: the first operand is the new checkout.
                Some("add") => out.push(Target::exact(
                    "git worktree add",
                    p.operands.get(1).map(|s| (*s).to_string()),
                    Kind::Dir,
                )),
                // `move WORKTREE NEW-PATH`: the second operand is the destination.
                Some("move") => out.push(Target::exact(
                    "git worktree move",
                    p.operands.get(2).map(|s| (*s).to_string()),
                    Kind::Dir,
                )),
                _ => {}
            }
            add_guesses(&mut out, "git worktree", &p, args, Kind::Dir);
        }
        "config" => {
            let p = read(args, CONFIG, false);
            let read_only = CONFIG_READS.iter().any(|n| p.has(n))
                || matches!(p.operands.first().copied(), Some("get" | "list"));
            if !read_only {
                for h in p.hits_of("file") {
                    out.push(Target::exact(
                        "git config --file",
                        h.value.map(str::to_string),
                        Kind::File,
                    ));
                }
                add_guesses(&mut out, "git config", &p, args, Kind::File);
            }
        }
        "checkout-index" => {
            let p = read(args, CHECKOUT_INDEX, false);
            let mut prefixed = false;
            for h in p.hits_of("prefix") {
                prefixed = true;
                out.push(Target {
                    action: "git checkout-index --prefix".to_string(),
                    value: h.value.map(str::to_string),
                    kind: Kind::Dir,
                    base: Base::WorkTree,
                    mode: Mode::Exact,
                });
            }
            // Without a prefix the files land in the work tree itself — judged
            // only when `--work-tree` names it (the plain cwd form is the
            // ordinary `git checkout-index -a` of a checkout). Named files
            // (`-- settings.json`, `--stdin`) land there just as `-a` does.
            if !prefixed
                && explicit_work_tree
                && (p.has("all") || p.has("stdin") || !p.operands.is_empty())
            {
                out.push(Target {
                    action: "git checkout-index".to_string(),
                    value: Some(".".to_string()),
                    kind: Kind::Dir,
                    base: Base::WorkTree,
                    mode: Mode::Exact,
                });
            }
            let mut g = Vec::new();
            add_guesses(&mut g, "git checkout-index", &p, args, Kind::Dir);
            for t in &mut g {
                t.base = Base::WorkTree;
            }
            out.extend(g);
        }
        "init" => {
            let p = read(args, INIT, false);
            for h in p.hits_of("separate-git-dir") {
                out.push(Target::exact(
                    "git init --separate-git-dir",
                    h.value.map(str::to_string),
                    Kind::Dir,
                ));
            }
            match p.operands.first() {
                Some(d) => out.push(Target::exact("git init", Some((*d).to_string()), Kind::Dir)),
                // No operand: the repository is created in the cwd, unless the
                // git dir is named elsewhere.
                None if git_dirs.is_empty() => {
                    out.push(Target::exact("git init", Some(".".to_string()), Kind::Dir))
                }
                None => {}
            }
            add_guesses(&mut out, "git init", &p, args, Kind::Dir);
            if !p.undetermined.is_empty() {
                out.push(Target {
                    action: "git init (unreadable command line)".to_string(),
                    value: Some(".".to_string()),
                    kind: Kind::Dir,
                    base: Base::Cwd,
                    mode: Mode::Guess,
                });
            }
        }
        "clone" => {
            let p = read(args, CLONE, false);
            for h in p.hits_of("separate-git-dir") {
                out.push(Target::exact(
                    "git clone --separate-git-dir",
                    h.value.map(str::to_string),
                    Kind::Dir,
                ));
            }
            // `git clone SRC [DIR]`: the second operand is the destination;
            // without it git creates the directory named after SRC.
            match (p.operands.first(), p.operands.get(1)) {
                (_, Some(d)) => out.push(Target::exact(
                    "git clone",
                    Some((*d).to_string()),
                    Kind::Dir,
                )),
                (Some(src), None) => {
                    let src = first_shell_word(src).unwrap_or_else(|| (*src).to_string());
                    let bare = p.has("bare") || p.has("mirror");
                    match clone_derived_name(&src, bare) {
                        Some(name) if !has_unresolvable_expansion(&name) => {
                            out.push(Target::exact("git clone", Some(name), Kind::Dir))
                        }
                        // A name that is only known at run time lands in the
                        // cwd: the cwd itself is what can be judged.
                        _ => out.push(Target {
                            action: "git clone".to_string(),
                            value: Some(".".to_string()),
                            kind: Kind::Dir,
                            base: Base::Cwd,
                            mode: Mode::Soft,
                        }),
                    }
                }
                (None, None) => {}
            }
            add_guesses(&mut out, "git clone", &p, args, Kind::Dir);
        }
        "push" => {
            // A push to a LOCAL repository writes its object store, refs and (with
            // `receive.denyCurrentBranch=updateInstead`) its work tree. A remote
            // NAME (`origin`) is not a path; a value spelled as one is judged as a
            // landing, only when it can be placed.
            let p = parse(
                args,
                &Spec {
                    tables: &[PUSH],
                    lenient: true,
                    digits: false,
                },
            );
            let remote = p
                .hits_of("repo")
                .next()
                .and_then(|h| h.value)
                .or_else(|| p.operands.first().copied());
            if let Some(r) = remote {
                let r = r.strip_prefix("file://").unwrap_or(r);
                if !r.contains("://") && (r.contains('/') || r.starts_with(['.', '~'])) {
                    out.push(Target {
                        action: "git push".to_string(),
                        value: Some(r.to_string()),
                        kind: Kind::Dir,
                        base: Base::Cwd,
                        mode: Mode::Soft,
                    });
                }
            }
        }
        "format-patch" => {
            let p = read(args, FORMAT_PATCH, true);
            for h in p.hits_of("output-directory") {
                out.push(Target::exact(
                    "git format-patch -o",
                    h.value.map(str::to_string),
                    Kind::Dir,
                ));
            }
            for h in p.hits_of("output") {
                out.push(Target::exact(
                    "git format-patch --output",
                    h.value.map(str::to_string),
                    Kind::File,
                ));
            }
            add_guesses(&mut out, "git format-patch", &p, args, Kind::Dir);
        }
        _ => {
            let table = if sub == "archive" { ARCHIVE } else { GENERIC };
            let p = parse(
                args,
                &Spec {
                    tables: &[table],
                    lenient: true,
                    digits: true,
                },
            );
            for h in p.hits_of("output") {
                out.push(Target::exact(
                    &format!("git {sub} --output"),
                    h.value.map(str::to_string),
                    Kind::File,
                ));
            }
            // A verb the table has no row for: every word that could be a
            // path is judged (an Ask on a protected / system landing).
            if judge_unmodelled && !unmodelled::is_ordinary(sub) {
                // Helpers drop scratch files in the cwd (`unpack-file` writes
                // `.merge_file_*` there), so the cwd is a landing too.
                out.push(Target {
                    action: format!("git {sub} (unmodelled subcommand, cwd)"),
                    value: Some(".".to_string()),
                    kind: Kind::Dir,
                    base: Base::Cwd,
                    mode: Mode::Guess,
                });
                for w in guess_words(args) {
                    out.push(Target {
                        action: format!("git {sub} (unmodelled subcommand)"),
                        value: Some(w),
                        kind: Kind::Dir,
                        base: Base::Cwd,
                        mode: Mode::Guess,
                    });
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
        // A NEW directory created directly inside a directory that holds
        // protected paths (`git worktree add ../w3` from `-C ~/.claude/hooks`
        // creates `~/.claude/w3`) populates that protected container.
        if let Some((parent, _)) = real.trim_end_matches('/').rsplit_once('/') {
            if let Some(d) = protected_landing_block(ctx, action, parent) {
                return Some(d);
            }
        }
    }
    None
}

/// The context git runs in: `Ok(ctx)` moved by the cumulative `-C` (and the
/// work tree when `globals` carry one), `Err(why)` when it cannot be placed.
fn placed<'a>(ctx: &Ctx<'a>, globals: &[&str]) -> Result<Ctx<'a>, String> {
    match git_moved_dir(ctx.scope, globals, ctx.base_for("git")) {
        Determination::Known(Some(dir)) => Ok(ctx.after_cd(&dir)),
        Determination::Known(None) => Ok(ctx.clone()),
        Determination::Undetermined(why) => Err(why.reason().as_str().to_string()),
    }
}

/// The verdict for a git subcommand's write targets, `Allow` when it has none.
/// `env` are the words before the command (`GIT_DIR=D git …`), `globals` the
/// words before the subcommand (`-C DIR` moves where a relative target is
/// placed), `args` the words after it.
pub(in crate::detect) fn git_write_target_verdict(
    ctx: &Ctx<'_>,
    env: &[&str],
    globals: &[&str],
    sub: &str,
    args: &[&str],
) -> Decision {
    let env = git_env(env);
    let global_work_tree = globals
        .iter()
        .any(|g| *g == "--work-tree" || g.starts_with("--work-tree="));
    let explicit_work_tree = global_work_tree || env.work_tree.is_some();
    // `-c alias.X=VERB …`: the alias hides the verb, so the verb it stands for
    // is judged too. The alias NAME is judged as an unmodelled verb only when
    // what it stands for is not itself a known verb (an alias to `log` is `log`).
    let alias = unmodelled::resolve_alias(globals, sub, args);
    let name_is_unknown_verb = alias
        .as_ref()
        .is_none_or(|(verb, _)| !unmodelled::is_ordinary(verb));
    let mut targets = git_write_targets(
        globals,
        &env,
        explicit_work_tree,
        sub,
        args,
        name_is_unknown_verb,
    );
    if let Some((verb, verb_args)) = alias {
        let verb_args: Vec<&str> = verb_args.iter().map(String::as_str).collect();
        targets.extend(git_write_targets(
            globals,
            &env,
            explicit_work_tree,
            &verb,
            &verb_args,
            true,
        ));
    }
    if targets.is_empty() {
        return Decision::Allow;
    }
    let cwd_globals = without_work_tree(globals);
    let cwd_placed = placed(ctx, &cwd_globals);
    // The work tree: `--work-tree` wins over `GIT_WORK_TREE`, as in git.
    let mut wt_globals: Vec<String> = globals.iter().map(|g| (*g).to_string()).collect();
    if let (false, Some(wt)) = (global_work_tree, &env.work_tree) {
        wt_globals.push(format!("--work-tree={wt}"));
    }
    let wt_refs: Vec<&str> = wt_globals.iter().map(String::as_str).collect();
    let wt_placed = placed(ctx, &wt_refs);
    for Target {
        action,
        value,
        kind,
        base,
        mode,
    } in targets
    {
        let Some(raw) = value else {
            if mode != Mode::Exact {
                continue;
            }
            return Decision::ask(format!(
                "{action} has no value, so blastguard cannot tell which path git writes — it \
refuses to guess"
            ));
        };
        let path = first_shell_word(&raw).unwrap_or_else(|| raw.clone());
        if path.is_empty() || has_unresolvable_expansion(&path) {
            if mode != Mode::Exact {
                continue;
            }
            return Decision::ask(format!(
                "{action} names `{raw}`, a path whose value is only known at run time — \
blastguard cannot tell whether git writes a protected gate/config path, so it refuses to guess"
            ));
        }
        let moved = if base == Base::WorkTree {
            &wt_placed
        } else {
            &cwd_placed
        };
        let wctx = match moved {
            Ok(c) => c,
            Err(why) => {
                // Only an absolute target is independent of the moved directory.
                if !path.starts_with('/') {
                    if mode == Mode::Soft {
                        continue;
                    }
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
            if mode == Mode::Guess {
                return Decision::ask(format!(
                    "{action}: blastguard could not read this git command line with certainty, \
and `{path}` is a protected or system path — it cannot tell whether git writes there, so it \
refuses to guess"
                ));
            }
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
