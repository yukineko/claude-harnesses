//! `condukt testscope` — PreToolUse(Bash) gate that enforces the test-scope
//! rulings in code instead of in prompt prose.
//!
//! User rulings (2026-11-07 / 2026-11-08 / 2026-11-12 / 2026-11-14): a full
//! suite is nightly-only; per change only the targeted test files run, once;
//! a run does not chase other sessions' work on main until it is done. Prompt
//! text did not hold (a delegated run executed
//! `cargo test -p blastguard --no-fail-fast` and re-merged main mid-run), so
//! the rules are judged here, deterministically:
//!
//! 1. **Full suite** — `cargo test` / `cargo nextest run` with no target
//!    selector (`--test`, `--lib`, `--bin`, `--doc`, `--example`, `--bench`),
//!    or with `--workspace` / `--all` / `--tests`, is denied.
//! 2. **Mid-run main merge** — inside a condukt task worktree
//!    (`$HOME/.condukt/worktrees/…`), `git merge|rebase` onto main /
//!    origin/main and any `git pull` are denied. A run integrates once, at
//!    the end, through `condukt worktree merge` (which runs git itself, not
//!    through this hook).
//! 3. **Same-tree rerun** — an allowed test command is recorded with the
//!    identity of the tree it ran on; the same command on the same tree is
//!    denied. The key is the same whichever route runs the command: a bare
//!    Bash `cargo test`, `bash -c`, `condukt sandbox run --cmd`, `tdd
//!    red|green --cmd <c>` / `--cmd=<c>` (whose `<c>` is judged like a bare
//!    command for every rule), and `condukt state test`, which calls
//!    [`judge`] itself before running anything. Two exceptions to the
//!    same-command key, both for `tdd green --cmd <c>` (any nesting depth):
//!    (a) backlog 8f82f09d: it is checked only against earlier `tdd green`
//!    runs of `<c>`, so a bare run cannot block the GREEN evidence the F->P
//!    oracle requires; it still records `<c>`'s bare key, refusing a bare run
//!    after it, and its own green key, refusing a second green on the same tree.
//!    (b) backlog 0c301e8b: it is also checked against an earlier `tdd red
//!    --cmd <c>` of `<c>`. On the same unchanged tree a confirmed red denies
//!    the green, and a pending red asks (rule R3 of spec
//!    `testscope-confirmed-run`), never allows it. A `tdd red --cmd <c>` is
//!    checked against the bare key like a bare run, and records a red key
//!    besides it (see `check_rerun`).
//! 4. **Task scope** (2026-11-14) — when the cwd is inside a run task's
//!    recorded worktree, each `--test <name>` must appear as `--test <name>`
//!    in that task's `reproduction_tests` or be a `tests/<name>.rs` the
//!    worktree added or changed against `default_branch` (`<base>...HEAD`,
//!    uncommitted, untracked), or (user ruling 2026-11-20, backlog 6f653d96:
//!    a fix must be able to prove it leaves the crate's pinned asserts
//!    unchanged) be an existing test of a crate the worktree changed: every
//!    `-p`/`--package` of that command names a package owning a path in the
//!    same three-source diff, the owner being the nearest `Cargo.toml` with a
//!    `[package]` at or above the path. `-p` is also read inside short-flag
//!    clusters as cargo parses them (`-qp y`, `-qpy`, `-vp y`); a cluster with
//!    a letter that is not modelled (anything but `q v h r p j F Z`) or a
//!    value-taking letter with no value makes the package set undetermined
//!    and denies. A command with no `-p`, a spec that is not a plain name, a
//!    `-p` of a crate the worktree did not change, or a changed path no
//!    manifest owns admits nothing through this branch; a manifest that
//!    exists but cannot be read or parsed denies. A `--test` value that is not
//!    a plain name (`[A-Za-z0-9_-]`; cargo reads `*`, `?`, `[]` as a glob that
//!    can select every integration test) is denied by every branch, including
//!    reproduction_tests.
//!    `--lib`/`--doc`/`--bin`/`--example`/`--bench` only when
//!    `reproduction_tests` names the same selector. Outside every recorded
//!    task worktree this rule does not apply. A cwd owned by several tasks,
//!    any run-state file that cannot be read or parsed, an unreadable
//!    decomposition or a failing git is denied. Rules 1 and 3 still apply to
//!    every command this rule admits.
//!
//! The hook also writes a per-session marker ([`write_session_marker`]) that
//! `condukt state set --status running` requires, so a session that never
//! loaded this hook cannot start a task.
//!
//! Every cannot-determine resolves to the restricted side (CLAUDE.md §3):
//! [`Decision::Deny`] for a command that cannot be judged at all (unbalanced
//! quotes around a cargo test, a tree identity git cannot give, an unreadable
//! or unwritable ledger, an unparseable `ledger.v2.tsv`), and
//! [`Decision::Ask`] for a rerun whose only prior was never confirmed as
//! executed (see "Confirmed runs" below). A command that contains no test /
//! merge invocation is never judged and is allowed by printing nothing.
//!
//! **Confirmed runs** (spec `testscope-confirmed-run`, backlog 64fe4e19). Rule
//! 3 counts only a test run that is known to have executed. PreToolUse writes a
//! *pending* row (it cannot know whether another PreToolUse hook will stop the
//! call); the `condukt testscope-confirm` hook, registered for PostToolUse and
//! PostToolUseFailure of Bash, promotes the rows carrying the same
//! `tool_use_id` to *confirmed* (an interrupted run is a run). A rerun of a
//! confirmed key on the same tree is denied; a rerun whose only prior is
//! pending (or a legacy `ledger.tsv` row nobody confirmed, or a Post that
//! matched no pending row for the same command and cwd) resolves to
//! [`Decision::Ask`], because a pending row proves nothing about whether the
//! test ran (a call that a hook denied or asked about never fires a Post). The
//! new records live in `ledger.v2.tsv`, append-only; confirmation also appends
//! the old 3-column row to `ledger.tsv` so an older cached binary still
//! refuses a confirmed rerun. The ledger itself (rule 3's `check_rerun`, the
//! confirm / invalidate records) lives in the [`rerun`] submodule.
//!
//! **Test-only fault seams** (read from the environment only, never from
//! the payload): `CONDUKT_TESTSCOPE_FAULT=confirm_panic` makes the confirm hook
//! panic once it has found a test command; `CONDUKT_TESTSCOPE_FAULT=
//! invalidate_append_fail` makes [`invalidate`] fail instead of appending.
//! Both only withhold a confirmation / an invalidation, i.e. move toward the
//! restricted side, so they cannot loosen the gate.
//!
//! **Where a test runs** (backlog 214b39b5): a `cd <dir>` that is certain to
//! run before a test (head of a list followed by `&&` / `;`, or inside an
//! all-`&&` chain) moves the test into `<dir>`; rule 3 identifies THAT tree,
//! and rule 4 is judged both in `<dir>` and in the payload cwd. A `cd` whose
//! target is not a literal path (`$VAR`, `-`, globs, `pushd`/`popd`, `~user`,
//! a bare relative name while `CDPATH` is set) or which is conditional
//! (`||`, `|`, `&`, after a non-head `&&` member followed by `;`) or followed
//! by an `&` redirection (`cd x &>f | …`; its effect is not modelled) leaves the
//! directory undetermined, and a test there is denied. `env cd` / `nice cd` /
//! `timeout N cd` run an external `cd` and move nothing, so they leave the
//! directory unchanged; a `cd` behind assignments or `command` leaves it
//! undetermined. See [`judge_pieces`].
//! The rerun key is that run tree's identity plus the command only (user
//! ruling 2026-11-15): the payload cwd's tree never contributes, so editing
//! only the payload cwd does not permit a rerun, and a test run directly in a
//! tree and a `cd` route into the same tree always collide. The identity
//! covers untracked file contents, not just their names (backlog b6543e39).
//!
//! Not covered (stated, not hidden): `--manifest-path` and `git -C`-style
//! redirection inside cargo, directory changes by anything other than a
//! literal `cd` at the head of a simple command (a sourced script, `builtin
//! cd`, `time cd`, `{ cd x; }`, `if cd x; then …`, `eval`), and `tdd
//! red|green` without `--cmd` (which runs tdd's configured `test_cmd`) are not
//! judged — such a test is attributed to the directory the walk last knew,
//! which can key it on the wrong tree (a rerun-ledger dodge; RED
//! `backlog_20261115_testscope_verifier_suspicions.rs` a1-a4, filed in the
//! backlog 2026-11-15).

use std::path::{Path, PathBuf};
use std::process::Command;

/// The decision for one Bash command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Deny(String),
    /// Not a verdict on the command: the rerun ledger holds only unconfirmed
    /// (pending) priors, so whether the test already ran is unknown. The
    /// caller must not run the command without a human answer; the text never
    /// says "deny".
    Ask(String),
}

const FULL_SUITE_REASON: &str = "full suite is nightly-only (user ruling 2026-11-07/11-12): \
     run only the RED file, its controls and the module's direct tests via \
     --test <file> / --lib <filter>";

const MAIN_MERGE_REASON: &str = "a condukt run integrates once at the end via `condukt worktree \
     merge`; do not merge, pull or rebase main into a task worktree mid-run, and ignore other \
     worktrees' work until this run is done (user ruling 2026-11-08/11-14)";

/// One simple command: its words after shell tokenizing.
type Words = Vec<String>;

/// Prefixed to the words of a test run through `tdd green --cmd` (backlog
/// 8f82f09d). Bare test words start with `cargo`, so the prefix cannot occur in
/// them.
const TDD_GREEN_ROUTE: [&str; 3] = ["tdd", "green", "--cmd"];

/// Prefixed to the words of a test run through `tdd red --cmd` (backlog
/// 0c301e8b), the same way as [`TDD_GREEN_ROUTE`]. A red run is still checked
/// against its bare key like any bare run; the tagged key is only recorded, so
/// that a later `tdd green --cmd` of the same command on the same tree can see
/// it.
const TDD_RED_ROUTE: [&str; 3] = ["tdd", "red", "--cmd"];

/// `w` with every leading [`TDD_GREEN_ROUTE`] / [`TDD_RED_ROUTE`] tag removed:
/// the bare test words, and whether a green tag / a red tag was among them.
fn untagged(w: &[String]) -> (&[String], bool, bool) {
    let green = TDD_GREEN_ROUTE.map(String::from);
    let red = TDD_RED_ROUTE.map(String::from);
    let (mut rest, mut is_green, mut is_red) = (w, false, false);
    loop {
        if rest.starts_with(&green) {
            rest = &rest[green.len()..];
            is_green = true;
        } else if rest.starts_with(&red) {
            rest = &rest[red.len()..];
            is_red = true;
        } else {
            return (rest, is_green, is_red);
        }
    }
}

/// `tw` with `route` prepended.
fn with_route(tw: Words, route: [&str; 3]) -> Words {
    let tag: Words = route.iter().map(|s| s.to_string()).collect();
    [tag, tw].concat()
}

/// `tw` tagged with [`TDD_GREEN_ROUTE`] exactly once, at any nesting depth.
fn green_tagged(tw: Words) -> Words {
    let (_, green, _) = untagged(&tw);
    if green {
        tw
    } else {
        with_route(tw, TDD_GREEN_ROUTE)
    }
}

/// `tw` tagged with [`TDD_RED_ROUTE`] exactly once, at any nesting depth.
fn red_tagged(tw: Words) -> Words {
    let (_, _, red) = untagged(&tw);
    if red {
        tw
    } else {
        with_route(tw, TDD_RED_ROUTE)
    }
}

/// What the tokenizer and the `cd` walk say about a command line, before any
/// task-scope or ledger rule is applied.
enum Parsed {
    /// Decided without a ledger: `Allow` (no test / merge invocation) or `Deny`.
    Final(Decision),
    /// Test invocations, grouped by the directory each runs in.
    Tests(Vec<(PathBuf, Vec<Words>)>),
}

/// Tokenize `command`, judge rules 1 and 2 and resolve the directory every test
/// runs in. Shared by the PreToolUse judge and the PostToolUse confirm hook
/// (which only needs to know whether the call contained a test command).
fn parse_tests(command: &str, cwd: &Path, home: Option<&Path>) -> Parsed {
    let stripped = strip_heredoc_bodies(command);
    let Some(pieces) = split_pieces(&stripped) else {
        return Parsed::Final(if mentions_test_or_merge(&stripped) {
            Decision::Deny(format!(
                "condukt testscope cannot tokenize this command (unbalanced quote), and it \
                 mentions a cargo test / git merge, so whether it is a full suite or a \
                 mid-run main merge is undetermined: {FULL_SUITE_REASON}"
            ))
        } else {
            Decision::Allow
        });
    };
    let ctx = Ctx {
        wt_root: home.map(|h| h.join(".condukt").join("worktrees")),
        cwd_in_wt: home
            .map(|h| cwd.starts_with(h.join(".condukt").join("worktrees")))
            .unwrap_or(false),
        home: home.map(Path::to_path_buf),
    };

    let test_cmds = match judge_pieces(&pieces, &Dir::Known(cwd.to_path_buf()), &ctx, 0, "") {
        Judged::Deny(r) => return Parsed::Final(Decision::Deny(r)),
        Judged::Test(t) => t,
        Judged::Other => return Parsed::Final(Decision::Allow),
    };
    // Every test must run in a directory we can name; a `cd` we could not
    // resolve leaves the tree under test undetermined (CLAUDE.md §3).
    let mut by_dir: Vec<(PathBuf, Vec<Words>)> = Vec::new();
    for (words, dir) in test_cmds {
        let d = match dir {
            Dir::Known(d) => d,
            Dir::Undetermined(why) => {
                return Parsed::Final(Decision::Deny(format!(
                    "condukt testscope cannot determine the directory `{}` runs in ({why}), so \
                     its tree, its task scope and whether it re-runs a test already run on the \
                     same tree are undetermined; run it without that `cd`, or with a leading \
                     `cd <literal absolute path> &&`",
                    words.join(" ")
                )));
            }
        };
        match by_dir.iter_mut().find(|(p, _)| *p == d) {
            Some((_, v)) => v.push(words),
            None => by_dir.push((d, vec![words])),
        }
    }
    Parsed::Tests(by_dir)
}

/// Judge a Bash command line run in `cwd`. `home` is `$HOME` (for the condukt
/// worktree root); `ledger` is where same-tree runs are recorded;
/// `tool_use_id` is the id of the PreToolUse call (`None` when the payload has
/// none: such a row is never promoted by a Post, so its rerun asks).
///
/// A [`Decision::Allow`] or [`Decision::Ask`] for a test command has written a
/// pending row for this attempt; [`confirm`] promotes it once the call is known
/// to have executed.
pub fn judge(
    command: &str,
    cwd: &Path,
    home: Option<&Path>,
    ledger: &Path,
    tool_use_id: Option<&str>,
) -> Decision {
    let by_dir = match parse_tests(command, cwd, home) {
        Parsed::Final(d) => return d,
        Parsed::Tests(t) => t,
    };
    // Rule C (task scope) is judged in the directory each test runs in AND in
    // the payload cwd: a worker inside its task worktree does not escape its
    // scope by `cd`-ing elsewhere, and a `cd` into a task worktree does not
    // escape that task's scope (backlog 214b39b5).
    for (d, tests) in &by_dir {
        match check_task_scope(tests, d) {
            Decision::Allow => {}
            stop @ (Decision::Deny(_) | Decision::Ask(_)) => return stop,
        }
        if d != cwd {
            match check_task_scope(tests, cwd) {
                Decision::Allow => {}
                stop @ (Decision::Deny(_) | Decision::Ask(_)) => return stop,
            }
        }
    }
    check_rerun(
        &by_dir,
        ledger,
        &Attempt {
            tool_use_id: tool_use_id.unwrap_or(""),
            raw_command: command,
            cwd,
        },
    )
}

/// Where a simple command runs, as far as the leading `cd`s of its command
/// line determine it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Dir {
    Known(PathBuf),
    /// A `cd` whose target or whose execution cannot be decided statically
    /// (variable, `-`, `pushd`, conditional on another command, …).
    Undetermined(String),
}

impl Dir {
    fn undetermined(why: impl Into<String>) -> Self {
        Dir::Undetermined(why.into())
    }
}

/// Per-command-line facts the segment judge needs.
struct Ctx {
    /// `$HOME/.condukt/worktrees`, when `$HOME` is known.
    wt_root: Option<PathBuf>,
    /// Whether the payload cwd is inside `wt_root`.
    cwd_in_wt: bool,
    home: Option<PathBuf>,
}

impl Ctx {
    /// Rule 2 applies when the payload cwd or the directory the command runs
    /// in is a condukt task worktree; an undetermined directory might be one.
    fn in_task_worktree(&self, dir: &Dir) -> bool {
        self.cwd_in_wt
            || match dir {
                Dir::Known(d) => self.wt_root.as_ref().is_some_and(|r| d.starts_with(r)),
                Dir::Undetermined(_) => true,
            }
    }
}

enum Judged {
    Deny(String),
    /// Scoped (allowed so far) test invocations, normalized, each with the
    /// directory it runs in.
    Test(Vec<(Words, Dir)>),
    Other,
}

/// Judge every simple command of a tokenized command line, tracking the
/// directory leading `cd`s move into. `what` names the program for messages
/// ("" for the hook's own command line).
///
/// A `cd` changes the directory of later commands only when it is certain to
/// have run before them and they are certain to run after it:
///
/// * at the head of a list (start, after `;` / newline / `(`) and followed by
///   `&&`, `;`, newline, `)` or the end — every later command of the list runs
///   in its target (if the `cd` fails, a `;`-joined successor runs in the old
///   directory, but then the target does not exist and its tree identity
///   fails, which denies);
/// * later in an all-`&&` list and followed by `&&` — its target holds only
///   while the `&&` run continues; at the first `;`, `||`, `|` or `&` the
///   directory becomes undetermined (an earlier member may have failed, so
///   the `cd` may not have run).
///
/// Any other `cd` (after `||` / `|` / `&`, or followed by them or by any
/// redirection with `&` such as `&>f` / `>&2`, `pushd`, `popd`) makes the
/// directory undetermined. `( … )` and `` ` … ` `` restore
/// the directory they started with.
fn judge_pieces(pieces: &[Piece], start: &Dir, ctx: &Ctx, depth: usize, what: &str) -> Judged {
    let mut dir = start.clone();
    // (dir, provisional) saved at each `(` / opening backtick.
    let mut stack: Vec<(Dir, bool)> = Vec::new();
    let mut in_tick = false;
    let mut at_head = true;
    let mut all_and = true;
    // `dir` was set by a non-head `cd` and holds only while the `&&` run lasts.
    let mut provisional = false;
    let mut found: Vec<(Words, Dir)> = Vec::new();
    let break_provisional = |dir: &mut Dir, provisional: &mut bool| {
        if *provisional {
            *dir = Dir::undetermined(
                "a `cd` that ran only if an earlier `&&` member succeeded is followed by a \
                 command that runs either way",
            );
            *provisional = false;
        }
    };
    for (idx, piece) in pieces.iter().enumerate() {
        let seg = match piece {
            Piece::Op(op) => {
                match op {
                    Op::Seq => {
                        break_provisional(&mut dir, &mut provisional);
                        at_head = true;
                        all_and = true;
                    }
                    Op::And | Op::Redirect => {}
                    Op::Or | Op::Pipe | Op::Bg => {
                        break_provisional(&mut dir, &mut provisional);
                        all_and = false;
                    }
                    Op::Open => {
                        stack.push((dir.clone(), provisional));
                        provisional = false;
                        at_head = true;
                        all_and = true;
                    }
                    Op::Close => {
                        if let Some((d, p)) = stack.pop() {
                            dir = d;
                            provisional = p;
                        }
                        at_head = false;
                    }
                    Op::Tick => {
                        if in_tick {
                            if let Some((d, p)) = stack.pop() {
                                dir = d;
                                provisional = p;
                            }
                            at_head = false;
                        } else {
                            stack.push((dir.clone(), provisional));
                            provisional = false;
                            at_head = true;
                            all_and = true;
                        }
                        in_tick = !in_tick;
                    }
                }
                continue;
            }
            Piece::Seg(s) => s,
        };
        let Some(words) = shlex::split(seg) else {
            if mentions_test_or_merge(seg) {
                return Judged::Deny(if what.is_empty() {
                    format!(
                        "condukt testscope cannot tokenize `{seg}`, and it mentions a cargo \
                         test / git merge: {FULL_SUITE_REASON}"
                    )
                } else {
                    format!(
                        "condukt testscope cannot tokenize `{seg}` in {what}: \
                         {FULL_SUITE_REASON}"
                    )
                });
            }
            at_head = false;
            continue;
        };
        // Only the shell's own `cd` moves the shell. `env` / `nice` /
        // `timeout` run an external `cd` (e.g. /usr/bin/cd) that cannot
        // change the directory of later commands, so [`strip_prefixes`] is
        // NOT used here; such a `cd` falls through to `judge_words` and leaves
        // `dir` unchanged. A `cd` behind assignments or `command` is
        // recognised but its target is undetermined (see below).
        let stripped_words = {
            let mut i = 0;
            while words
                .get(i)
                .is_some_and(|w| is_assignment(w) || w == "command")
            {
                i += 1;
            }
            &words[i..]
        };
        if let Some(first) = stripped_words.first() {
            if matches!(first.as_str(), "cd" | "pushd" | "popd") {
                // The piece right after the `cd`. A redirection `&` (`&>f`,
                // `>&2`) is reported as `Op::Redirect`, which no arm below
                // accepts: what follows the redirect target (`&>f | …`,
                // `&>f & …`) is not looked at, so such a `cd` is undetermined
                // rather than taken as certain.
                let next = match pieces.get(idx + 1) {
                    None => None,
                    Some(Piece::Op(o)) => Some(*o),
                    Some(Piece::Seg(_)) => Some(Op::Redirect),
                };
                let target = if stripped_words.len() != words.len() {
                    // `VAR=1 cd`, `command cd`, `command VAR=1 cd`, `'A=1' cd`:
                    // whether the builtin runs depends on word order and on
                    // quoting that shlex has already removed, so the move is
                    // not modelled (fail closed).
                    Dir::undetermined(format!(
                        "`{}`: a `cd` behind assignments or `command` is not resolved",
                        words.join(" ")
                    ))
                } else {
                    resolve_cd(stripped_words, &dir, ctx.home.as_deref())
                };
                if at_head && matches!(next, None | Some(Op::Seq | Op::And | Op::Close)) {
                    dir = target;
                    provisional = false;
                } else if all_and && next == Some(Op::And) {
                    dir = target;
                    provisional = true;
                } else {
                    dir = Dir::undetermined(format!(
                        "`{}` is not certain to have run before the commands after it",
                        words.join(" ")
                    ));
                    provisional = false;
                }
                at_head = false;
                continue;
            }
        }
        at_head = false;
        match judge_words(&words, &dir, ctx, depth) {
            Judged::Deny(r) => return Judged::Deny(r),
            Judged::Test(t) => found.extend(t),
            Judged::Other => {}
        }
    }
    if found.is_empty() {
        Judged::Other
    } else {
        Judged::Test(found)
    }
}

/// The directory `cd <args>` moves into from `dir`, resolved statically:
/// a literal absolute path, a literal relative path (lexically against
/// `dir`), `~` / `~/…` against `$HOME`, or no argument (`$HOME`). Anything
/// else — `-`, a variable, a glob, command substitution, several operands,
/// `pushd` / `popd`, a bare relative name while `CDPATH` is set — is
/// undetermined.
fn resolve_cd(words: &[String], dir: &Dir, home: Option<&Path>) -> Dir {
    let shown = words.join(" ");
    if words[0] != "cd" {
        return Dir::undetermined(format!("`{shown}` changes the directory stack"));
    }
    let mut ops: Vec<&str> = Vec::new();
    let mut opts_done = false;
    for a in &words[1..] {
        if !opts_done && a == "--" {
            opts_done = true;
        } else if !opts_done && a.starts_with('-') && a != "-" {
            if !matches!(a.as_str(), "-L" | "-P" | "-e" | "-LP" | "-PL") {
                return Dir::undetermined(format!("`{shown}`: unknown cd option `{a}`"));
            }
        } else {
            ops.push(a.as_str());
        }
    }
    let home_dir = || match home {
        Some(h) => Dir::Known(h.to_path_buf()),
        None => Dir::undetermined(format!("`{shown}` moves to $HOME, which is unknown")),
    };
    let arg = match ops.as_slice() {
        [] => return home_dir(),
        [a] => *a,
        _ => return Dir::undetermined(format!("`{shown}` has several operands")),
    };
    if arg.is_empty() || arg == "-" || arg.contains(['$', '`', '*', '?', '[', '{']) {
        return Dir::undetermined(format!("`{shown}`: the target is not a literal path"));
    }
    let target = if arg == "~" {
        match home_dir() {
            Dir::Known(h) => h,
            u => return u,
        }
    } else if let Some(rest) = arg.strip_prefix("~/") {
        match home_dir() {
            Dir::Known(h) => h.join(rest),
            u => return u,
        }
    } else if arg.starts_with('~') {
        return Dir::undetermined(format!("`{shown}`: `~user` is not resolved"));
    } else if Path::new(arg).is_absolute() {
        PathBuf::from(arg)
    } else {
        let dotted = arg == "." || arg == ".." || arg.starts_with("./") || arg.starts_with("../");
        if !dotted && std::env::var_os("CDPATH").is_some_and(|v| !v.is_empty()) {
            return Dir::undetermined(format!("`{shown}`: CDPATH is set"));
        }
        match dir {
            Dir::Known(d) => d.join(arg),
            Dir::Undetermined(why) => return Dir::Undetermined(why.clone()),
        }
    };
    Dir::Known(normalize_lexically(&target))
}

/// Resolve `.` and `..` components without touching the filesystem (the
/// logical `cd -L` view).
fn normalize_lexically(p: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn judge_words(words: &[String], dir: &Dir, ctx: &Ctx, depth: usize) -> Judged {
    let in_task_worktree = ctx.in_task_worktree(dir);
    let w = strip_prefixes(words);
    let Some(first) = w.first() else {
        return Judged::Other;
    };
    let base = Path::new(first.as_str())
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(first.as_str());
    match base {
        "bash" | "sh" | "zsh" if depth < 4 => {
            if let Some(pos) = w.iter().position(|a| a == "-c" || a == "-lc") {
                if let Some(payload) = w.get(pos + 1) {
                    return judge_payload(payload, dir, ctx, depth + 1);
                }
            }
            Judged::Other
        }
        // `condukt sandbox run --cmd '<program>'` runs <program> in a container;
        // the program is judged like a `-c` payload.
        "condukt" if depth < 4 && w.get(1).map(String::as_str) == Some("sandbox") => {
            match cmd_flag(w) {
                Some(p) => judge_payload(&p, dir, ctx, depth + 1),
                None => Judged::Other,
            }
        }
        // `tdd red|green … --cmd '<c>'` runs <c> as the test command; it is
        // judged like the bare command (full suite, task scope, rerun key). A
        // `tdd green` test is tagged once at any depth ([`green_tagged`]), and
        // a `tdd red` test likewise ([`red_tagged`]), so each route records its
        // own key.
        "tdd" if depth < 4 && matches!(w.get(1).map(String::as_str), Some("red" | "green")) => {
            let tag: fn(Words) -> Words = if w[1] == "green" {
                green_tagged
            } else {
                red_tagged
            };
            match cmd_flag(w) {
                Some(p) => match judge_payload(&p, dir, ctx, depth + 1) {
                    Judged::Test(t) => {
                        Judged::Test(t.into_iter().map(|(tw, d)| (tag(tw), d)).collect())
                    }
                    other => other,
                },
                None => Judged::Other,
            }
        }
        "cargo" => judge_cargo(&w[1..], dir),
        "git" if in_task_worktree => judge_git(&w[1..]),
        _ => Judged::Other,
    }
}

/// A nested program (`bash -c`, `--cmd`) starts in the directory of the
/// command that runs it; its own `cd`s do not leak out.
fn judge_payload(payload: &str, dir: &Dir, ctx: &Ctx, depth: usize) -> Judged {
    let stripped = strip_heredoc_bodies(payload);
    let Some(pieces) = split_pieces(&stripped) else {
        return if mentions_test_or_merge(&stripped) {
            Judged::Deny(format!(
                "condukt testscope cannot tokenize the -c program: {FULL_SUITE_REASON}"
            ))
        } else {
            Judged::Other
        };
    };
    judge_pieces(&pieces, dir, ctx, depth, "the -c program")
}

/// The value of `--cmd <v>` / `--cmd=<v>` in `w`, if any.
fn cmd_flag(w: &[String]) -> Option<String> {
    w.iter().enumerate().find_map(|(i, a)| {
        if a == "--cmd" {
            w.get(i + 1).cloned()
        } else {
            a.strip_prefix("--cmd=").map(str::to_string)
        }
    })
}

/// Drop leading `VAR=val` assignments and the `env` / `timeout N` / `nice`
/// wrappers so the real command word comes first.
fn strip_prefixes(words: &[String]) -> &[String] {
    let mut i = 0;
    loop {
        let Some(w) = words.get(i) else {
            return &words[i..];
        };
        if is_assignment(w) {
            i += 1;
        } else if w == "env" || w == "nice" || w == "command" || w == "exec" {
            i += 1;
            while words.get(i).is_some_and(|a| a.starts_with('-')) {
                i += 1;
            }
        } else if w == "timeout" {
            i += 1;
            while words.get(i).is_some_and(|a| a.starts_with('-')) {
                i += 1;
            }
            i += 1; // the duration
        } else {
            return &words[i..];
        }
    }
}

fn is_assignment(w: &str) -> bool {
    match w.split_once('=') {
        Some((name, _)) => {
            !name.is_empty()
                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                && !name.starts_with(|c: char| c.is_ascii_digit())
        }
        None => false,
    }
}

fn judge_cargo(args: &[String], dir: &Dir) -> Judged {
    let mut i = 0;
    // `cargo +toolchain …` and global flags before the subcommand.
    while args
        .get(i)
        .is_some_and(|a| a.starts_with('+') || a.starts_with('-'))
    {
        i += 1;
    }
    let rest: &[String] = match args.get(i).map(String::as_str) {
        Some("test") | Some("t") => &args[i + 1..],
        Some("nextest") if args.get(i + 1).map(String::as_str) == Some("run") => &args[i + 2..],
        _ => return Judged::Other,
    };
    let opts: Vec<&String> = rest.iter().take_while(|a| a.as_str() != "--").collect();
    let flag = |name: &str| {
        opts.iter()
            .any(|a| a.as_str() == name || a.starts_with(&format!("{name}=")))
    };
    if flag("--workspace") || flag("--all") || flag("--tests") {
        return Judged::Deny(format!(
            "`cargo {}` runs every test target: {FULL_SUITE_REASON}",
            args.join(" ")
        ));
    }
    let selected = ["--test", "--lib", "--bin", "--doc", "--example", "--bench"]
        .iter()
        .any(|s| flag(s));
    if !selected {
        return Judged::Deny(format!(
            "`cargo {}` names no test target, so it builds and runs the whole suite: \
             {FULL_SUITE_REASON}",
            args.join(" ")
        ));
    }
    let mut norm = vec!["cargo".to_string()];
    norm.extend(args.iter().cloned());
    Judged::Test(vec![(norm, dir.clone())])
}

fn judge_git(args: &[String]) -> Judged {
    let mut i = 0;
    // `git -C dir …` / `git -c k=v …`
    while let Some(a) = args.get(i) {
        if a == "-C" || a == "-c" {
            i += 2;
        } else if a.starts_with('-') {
            i += 1;
        } else {
            break;
        }
    }
    let Some(sub) = args.get(i) else {
        return Judged::Other;
    };
    let operands: Vec<&str> = args[i + 1..]
        .iter()
        .filter(|a| !a.starts_with('-'))
        .map(String::as_str)
        .collect();
    let is_main = |s: &str| {
        matches!(
            s,
            "main" | "master" | "origin/main" | "origin/master" | "@{u}" | "@{upstream}"
        )
    };
    let deny = match sub.as_str() {
        "pull" => true,
        "merge" | "rebase" => operands.iter().any(|o| is_main(o)),
        _ => false,
    };
    if deny {
        Judged::Deny(format!("`git {}`: {MAIN_MERGE_REASON}", args.join(" ")))
    } else {
        Judged::Other
    }
}

fn mentions_test_or_merge(s: &str) -> bool {
    (s.contains("cargo") && s.contains("test"))
        || (s.contains("git")
            && (s.contains("merge") || s.contains("pull") || s.contains("rebase")))
}

/// Remove here-document bodies: after a line holding `<<DELIM` / `<<-DELIM`
/// (optionally quoted), drop the following lines up to the delimiter line.
/// Body text is data, never a command.
fn strip_heredoc_bodies(cmd: &str) -> String {
    let mut out = String::new();
    let mut pending: Vec<(String, bool)> = Vec::new();
    let mut lines = cmd.split('\n').peekable();
    while let Some(line) = lines.next() {
        if let Some((delim, dash)) = pending.first().cloned() {
            let probe = if dash {
                line.trim_start_matches('\t')
            } else {
                line
            };
            if probe == delim {
                pending.remove(0);
            }
            continue;
        }
        out.push_str(line);
        if lines.peek().is_some() {
            out.push('\n');
        }
        pending.extend(heredoc_delims(line));
    }
    out
}

fn heredoc_delims(line: &str) -> Vec<(String, bool)> {
    let mut found = Vec::new();
    let b = line.as_bytes();
    let mut i = 0;
    let mut quote: Option<u8> = None;
    while i < b.len() {
        let c = b[i];
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                } else if c == b'\\' && q == b'"' {
                    i += 1;
                }
            }
            None => {
                if c == b'\'' || c == b'"' {
                    quote = Some(c);
                } else if c == b'\\' {
                    i += 1;
                } else if c == b'<' && b.get(i + 1) == Some(&b'<') && b.get(i + 2) != Some(&b'<') {
                    let mut j = i + 2;
                    let dash = b.get(j) == Some(&b'-');
                    if dash {
                        j += 1;
                    }
                    while b.get(j) == Some(&b' ') {
                        j += 1;
                    }
                    let mut delim = String::new();
                    while let Some(&ch) = b.get(j) {
                        if ch == b'\'' || ch == b'"' || ch == b'\\' {
                            j += 1;
                            continue;
                        }
                        if ch.is_ascii_alphanumeric() || ch == b'_' || ch == b'-' || ch == b'.' {
                            delim.push(ch as char);
                            j += 1;
                        } else {
                            break;
                        }
                    }
                    if !delim.is_empty() {
                        found.push((delim, dash));
                    }
                    i = j;
                    continue;
                }
            }
        }
        i += 1;
    }
    found
}

/// Split a command line into simple-command segments at `;`, `&`, `|`,
/// newlines and parentheses outside quotes. `None` when a quote is unbalanced.
fn split_segments(cmd: &str) -> Option<Vec<String>> {
    Some(
        split_pieces(cmd)?
            .into_iter()
            .filter_map(|p| match p {
                Piece::Seg(s) => Some(s),
                Piece::Op(_) => None,
            })
            .collect(),
    )
}

/// The control operator that separated two segments.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    /// `;` or newline.
    Seq,
    And,
    Or,
    /// `|` or `|&`.
    Pipe,
    /// A lone `&` (background).
    Bg,
    Open,
    Close,
    Tick,
    /// The `&` of a redirection (`2>&1`, `<&0`, `&>file`): not a separator,
    /// but the segment is still split there as before.
    Redirect,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Piece {
    Seg(String),
    Op(Op),
}

/// [`split_segments`] with the operators kept, so the walk can tell which
/// commands a `cd` is certain to precede. `None` when a quote is unbalanced.
fn split_pieces(cmd: &str) -> Option<Vec<Piece>> {
    let mut segs: Vec<Piece> = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut chars = cmd.chars().peekable();
    while let Some(c) = chars.next() {
        match quote {
            Some(q) => {
                cur.push(c);
                if c == q {
                    quote = None;
                } else if c == '\\' && q == '"' {
                    if let Some(n) = chars.next() {
                        cur.push(n);
                    }
                }
            }
            None => match c {
                '\'' | '"' => {
                    quote = Some(c);
                    cur.push(c);
                }
                '\\' => {
                    match chars.next() {
                        Some('\n') => {} // line continuation
                        Some(n) => {
                            cur.push('\\');
                            cur.push(n);
                        }
                        None => cur.push('\\'),
                    }
                }
                ';' | '&' | '|' | '\n' | '(' | ')' | '`' => {
                    let op = match c {
                        ';' | '\n' => Op::Seq,
                        '&' if cur.ends_with(['>', '<']) => Op::Redirect,
                        '&' if chars.peek() == Some(&'>') => Op::Redirect,
                        '&' if chars.peek() == Some(&'&') => {
                            chars.next();
                            Op::And
                        }
                        '&' => Op::Bg,
                        '|' if chars.peek() == Some(&'|') => {
                            chars.next();
                            Op::Or
                        }
                        '|' => {
                            if chars.peek() == Some(&'&') {
                                chars.next();
                            }
                            Op::Pipe
                        }
                        '(' => Op::Open,
                        ')' => Op::Close,
                        _ => Op::Tick,
                    };
                    if !cur.trim().is_empty() {
                        segs.push(Piece::Seg(std::mem::take(&mut cur)));
                    }
                    cur.clear();
                    segs.push(Piece::Op(op));
                }
                _ => cur.push(c),
            },
        }
    }
    if quote.is_some() {
        return None;
    }
    if !cur.trim().is_empty() {
        segs.push(Piece::Seg(cur));
    }
    Some(segs)
}

/// The identity of the tree a test runs against: the repo top level, HEAD and
/// a hash of everything that differs from HEAD — the tracked diff, the list of
/// untracked (not ignored) paths, and the CONTENTS of each untracked path
/// (backlog b6543e39: hashing the names only made an edit to a new, not yet
/// added test file look like the same tree). An untracked file is hashed by
/// its bytes, an untracked symlink by its target, and an untracked nested
/// repository (`git ls-files` lists it as `dir/`) by its own identity. A path
/// that cannot be read makes the identity undetermined (`Err`), never "same".
/// All of it is read from the repository top level, so any subdirectory of a
/// tree has that tree's identity.
/// A tree with no untracked paths hashes exactly as before this change, so
/// existing ledger records for clean trees keep colliding.
fn tree_identity(cwd: &Path) -> Result<(String, String), String> {
    tree_identity_at(cwd, 0)
}

fn tree_identity_at(cwd: &Path, depth: usize) -> Result<(String, String), String> {
    let git_in = |dir: &Path, args: &[&str]| -> Result<String, String> {
        let out = Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .map_err(|e| format!("git {}: {e}", args.join(" ")))?;
        if !out.status.success() {
            return Err(format!(
                "git {} exited {}: {}",
                args.join(" "),
                out.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    };
    let top = git_in(cwd, &["rev-parse", "--show-toplevel"])?
        .trim()
        .to_string();
    // Everything below is read from the top level, so the identity of a tree
    // does not depend on which of its subdirectories the test runs in (a
    // `cd <top>/sub &&` must collide with a run at `<top>`).
    let topdir = PathBuf::from(&top);
    let git = |args: &[&str]| git_in(&topdir, args);
    let head = git(&["rev-parse", "HEAD"])?.trim().to_string();
    let diff = git(&["diff", "HEAD", "--binary"])?;
    let untracked = git(&["ls-files", "--others", "--exclude-standard"])?;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    std::hash::Hash::hash(&diff, &mut h);
    std::hash::Hash::hash(&untracked, &mut h);
    // The list above C-quotes unusual names; for the contents take the raw
    // (`-z`) names, relative to the top level.
    let raw = git(&["ls-files", "--others", "--exclude-standard", "-z"])?;
    if !raw.is_empty() {
        for rel in raw.split('\0').filter(|s| !s.is_empty()) {
            let p = topdir.join(rel);
            std::hash::Hash::hash(rel, &mut h);
            let meta = std::fs::symlink_metadata(&p)
                .map_err(|e| format!("untracked {}: {e}", p.display()))?;
            if meta.file_type().is_symlink() {
                let t = std::fs::read_link(&p)
                    .map_err(|e| format!("untracked symlink {}: {e}", p.display()))?;
                std::hash::Hash::hash(&t, &mut h);
            } else if meta.is_dir() {
                if depth >= 4 {
                    return Err(format!(
                        "untracked nested repository {} is nested too deep to identify",
                        p.display()
                    ));
                }
                let (ntop, ntree) = tree_identity_at(&p, depth + 1)?;
                std::hash::Hash::hash(&ntop, &mut h);
                std::hash::Hash::hash(&ntree, &mut h);
            } else {
                let bytes =
                    std::fs::read(&p).map_err(|e| format!("untracked {}: {e}", p.display()))?;
                std::hash::Hash::hash(&bytes, &mut h);
            }
        }
    }
    let dirty = std::hash::Hasher::finish(&h);
    Ok((top, format!("{head}:{dirty:016x}")))
}

// The rerun ledger (LedgerKey, ledger.v2.tsv, check_rerun, confirm, invalidate,
// ledger_path, ...) lives in `testscope/rerun.rs`; see that module's doc.
mod rerun;
use rerun::{check_rerun, Attempt};
pub use rerun::{confirm_post, ledger_path, self_attempt_id, settle_self_run};

// ── session marker (rule A) ─────────────────────────────────────────────────

// Lives in `testscope_hook.rs` (the hook is its only writer); re-exported so
// `testscope::session_marker_present` keeps its path for `main.rs`.
pub use crate::testscope_hook::{session_marker_present, write_session_marker};

// ── task scope (rule C) ─────────────────────────────────────────────────────

/// One target selector of a `cargo test` / `cargo nextest run` invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Selector {
    /// `--test <name>` / `--test=<name>`.
    Test(String),
    /// `--lib`, `--doc`, or `--bin|--example|--bench [<name>]`.
    Kind(String, Option<String>),
}

/// The selectors among `words` (options before `--` only). `Err` for a
/// `--test` with no name (it lists targets instead of naming one, so which test
/// file runs is undetermined).
fn selectors(words: &[String]) -> Result<Vec<Selector>, String> {
    let opts: Vec<&String> = words.iter().take_while(|a| a.as_str() != "--").collect();
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(a) = opts.get(i) {
        let a = a.as_str();
        let value_after = |i: usize| {
            opts.get(i + 1)
                .filter(|v| !v.starts_with('-'))
                .map(|v| v.to_string())
        };
        if a == "--test" {
            match value_after(i) {
                Some(v) => {
                    out.push(Selector::Test(v));
                    i += 1;
                }
                None => {
                    return Err("`--test` names no test target".to_string());
                }
            }
        } else if let Some(v) = a.strip_prefix("--test=") {
            out.push(Selector::Test(v.to_string()));
        } else if a == "--lib" || a == "--doc" {
            out.push(Selector::Kind(a.to_string(), None));
        } else if a == "--bin" || a == "--example" || a == "--bench" {
            let v = value_after(i);
            if v.is_some() {
                i += 1;
            }
            out.push(Selector::Kind(a.to_string(), v));
        } else if let Some((flag, v)) = a.split_once('=') {
            if matches!(flag, "--bin" | "--example" | "--bench") {
                out.push(Selector::Kind(flag.to_string(), Some(v.to_string())));
            }
        }
        i += 1;
    }
    Ok(out)
}

/// Every selector named anywhere in a task's `reproduction_tests` text.
/// Unparseable text is `Err` (what it allows is undetermined).
fn reproduction_selectors(text: &str) -> Result<Vec<Selector>, String> {
    let stripped = strip_heredoc_bodies(text);
    let segs = split_segments(&stripped)
        .ok_or_else(|| "reproduction_tests has an unbalanced quote".to_string())?;
    let mut out = Vec::new();
    for seg in segs {
        let words = shlex::split(&seg)
            .ok_or_else(|| format!("reproduction_tests: cannot tokenize `{seg}`"))?;
        out.extend(selectors(&words)?);
    }
    Ok(out)
}

/// Run git in `dir` and return stdout; a spawn failure or non-zero exit is an
/// `Err` naming it (a checker that died is not a checker that passed).
fn git_stdout(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = harness_core::git_env::scrub(&mut Command::new("git"))
        .args(args)
        .current_dir(dir)
        .output()
        .map_err(|e| format!("git {}: {e}", args.join(" ")))?;
    if !out.status.success() {
        return Err(format!(
            "git {} exited {}: {}",
            args.join(" "),
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The `tests/<name>.rs` stems the worktree at `wt` added or changed against
/// `base`: committed (`git diff --name-only <base>...HEAD`), uncommitted
/// (`git diff --name-only HEAD`) and untracked files.
fn worktree_changed_tests(wt: &Path, base: &str) -> Result<Vec<String>, String> {
    let range = format!("{base}...HEAD");
    let mut paths = String::new();
    paths.push_str(&git_stdout(wt, &["diff", "--name-only", &range])?);
    paths.push('\n');
    paths.push_str(&git_stdout(wt, &["diff", "--name-only", "HEAD"])?);
    paths.push('\n');
    paths.push_str(&git_stdout(
        wt,
        &["ls-files", "--others", "--exclude-standard"],
    )?);
    let mut out: Vec<String> = Vec::new();
    for line in paths.lines() {
        let p = Path::new(line.trim());
        let in_tests = p
            .parent()
            .and_then(|d| d.file_name())
            .and_then(|n| n.to_str())
            == Some("tests");
        if in_tests && p.extension().and_then(|e| e.to_str()) == Some("rs") {
            if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                if !out.iter().any(|s| s == stem) {
                    out.push(stem.to_string());
                }
            }
        }
    }
    Ok(out)
}

/// The packages owning the paths the worktree at `wt` added, changed or
/// deleted against `base` (the same three sources as
/// [`worktree_changed_tests`], read with `-z`). A path is owned by the package
/// of the nearest `Cargo.toml` with a `[package]` table at or above the path's
/// directory, within the worktree's top level; a path with no such manifest
/// (a root `Cargo.lock`, `docs/…`, a virtual-workspace-only path) adds no
/// package — it never widens the set. A failing git, or a manifest that
/// exists but cannot be read or parsed, or whose `[package].name` is not a
/// string, is an `Err` (undetermined; the caller denies).
fn worktree_changed_packages(wt: &Path, base: &str) -> Result<Vec<String>, String> {
    let top = PathBuf::from(
        git_stdout(wt, &["rev-parse", "--show-toplevel"])?
            .trim()
            .to_string(),
    );
    let range = format!("{base}...HEAD");
    let mut raw = String::new();
    for args in [
        &["diff", "--name-only", "-z", &range][..],
        &["diff", "--name-only", "-z", "HEAD"][..],
        &["ls-files", "--others", "--exclude-standard", "-z"][..],
    ] {
        raw.push_str(&git_stdout(&top, args)?);
        raw.push('\0');
    }
    // dir -> package named by `dir/Cargo.toml` (None: no manifest there, or a
    // manifest without `[package]`).
    let mut seen: std::collections::HashMap<PathBuf, Option<String>> =
        std::collections::HashMap::new();
    let mut out: Vec<String> = Vec::new();
    for rel in raw.split('\0').filter(|s| !s.is_empty()) {
        let mut dir = match top.join(rel).parent() {
            Some(d) => d.to_path_buf(),
            None => continue,
        };
        while dir.starts_with(&top) {
            let pkg = match seen.get(&dir) {
                Some(p) => p.clone(),
                None => {
                    let p = manifest_package_name(&dir.join("Cargo.toml"))?;
                    seen.insert(dir.clone(), p.clone());
                    p
                }
            };
            if let Some(name) = pkg {
                if !out.contains(&name) {
                    out.push(name);
                }
                break;
            }
            if dir == top || !dir.pop() {
                break;
            }
        }
    }
    Ok(out)
}

/// The `[package].name` of the manifest at `path`. `Ok(None)` when no file
/// exists there (an observed absence) or the manifest has no `[package]`
/// table (a virtual workspace root); `Err` when it exists but cannot be read
/// or parsed, or its name is not a string.
fn manifest_package_name(path: &Path) -> Result<Option<String>, String> {
    let txt = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("cannot read manifest {}: {e}", path.display())),
    };
    let v: toml::Value = toml::from_str(&txt)
        .map_err(|e| format!("unparseable manifest {}: {e}", path.display()))?;
    match v.get("package") {
        None => Ok(None),
        Some(p) => match p.get("name").and_then(toml::Value::as_str) {
            Some(n) => Ok(Some(n.to_string())),
            None => Err(format!(
                "manifest {} has a [package] table without a string name",
                path.display()
            )),
        },
    }
}

/// The package specs of `--package <spec>` / `--package=<spec>` and of every
/// short-flag cluster among `words` (options before `--` only). A cluster is
/// a word with a single leading `-` (`-p x`, `-px`, `-p=x`, `-qp x`, `-qpx`,
/// `-vp x`): cargo parses it letter by letter, so it is parsed the same way
/// here. Value-less letters `q`, `v`, `h`, `r` are skipped; `p` takes the
/// rest of the cluster (a leading `=` dropped) or else the next word as a
/// spec; `j`, `F`, `Z` take the rest or the next word as a value that is not a
/// spec. Any other letter, or a value-taking letter with no value, is `Err`:
/// whether the cluster carries a `p` is undetermined.
fn package_specs(words: &[String]) -> Result<Vec<String>, String> {
    let opts: Vec<&String> = words.iter().take_while(|a| a.as_str() != "--").collect();
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(a) = opts.get(i) {
        let a = a.as_str();
        if a == "--package" {
            match opts.get(i + 1).filter(|v| !v.starts_with('-')) {
                Some(v) => {
                    out.push(v.to_string());
                    i += 1;
                }
                None => return Err(format!("`{a}` names no package")),
            }
        } else if let Some(v) = a.strip_prefix("--package=") {
            out.push(v.to_string());
        } else if let Some(cluster) = a.strip_prefix('-').filter(|c| !c.starts_with('-')) {
            for (pos, c) in cluster.char_indices() {
                match c {
                    'q' | 'v' | 'h' | 'r' => {}
                    'p' | 'j' | 'F' | 'Z' => {
                        let rest = &cluster[pos + c.len_utf8()..];
                        let value = if rest.is_empty() {
                            let next = opts.get(i + 1).map(|s| s.as_str());
                            match next.filter(|v| c != 'p' || !v.starts_with('-')) {
                                Some(v) => {
                                    i += 1;
                                    v.to_string()
                                }
                                None => {
                                    return Err(format!("`-{c}` in `{a}` has no value"));
                                }
                            }
                        } else {
                            rest.strip_prefix('=').unwrap_or(rest).to_string()
                        };
                        if c == 'p' {
                            out.push(value);
                        }
                        break;
                    }
                    other => {
                        return Err(format!(
                            "short-flag cluster `{a}` has `-{other}`, which is not modelled, so \
                             whether it selects a package is undetermined"
                        ));
                    }
                }
            }
        }
        i += 1;
    }
    Ok(out)
}

/// Whether `name` is a plain target / package name (`[A-Za-z0-9_-]+`). cargo
/// treats `*`, `?` and `[]` in `--test` as a Unix glob, so a name with any
/// other character may select several targets.
fn is_plain_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

const TASK_SCOPE_REASON: &str = "inside a condukt task worktree only the task's own tests may \
     run: a `--test <name>` named in the task's reproduction_tests, a tests/<name>.rs this \
     worktree added or changed, or a `--test <name>` whose command names, with `-p` (also \
     inside short-flag clusters such as `-qp`), only packages that own a path this worktree \
     changed against its base (no `-p`, a package spec that is not a plain name, a short-flag \
     cluster that is not modelled, or an unreadable diff or manifest is refused); `<name>` \
     must be a plain name, never a glob (`*`, `?`, `[]`); `--lib`/`--bin`/`--doc` only when \
     reproduction_tests names that selector (user rulings 2026-11-14 and 2026-11-20)";

/// `state::all_runs` for a GATE: the same dirs and the same canonical-first
/// precedence, but any run-state file that cannot be read or parsed (and any
/// state dir that exists but cannot be listed) is an `Err` naming it instead of
/// a stderr warning. A gate that asks "which task owns this path" cannot answer
/// from a partial list — the unread file may be the owner (CLAUDE.md §3). A
/// missing dir still contributes no runs (ENOENT is an observed absence).
fn all_runs_strict(
    cfg: &crate::config::Config,
    cwd: &Path,
) -> anyhow::Result<Vec<crate::state::RunState>> {
    use crate::state::{is_run_state_sidecar, project_state_dirs, RunState};
    use anyhow::{bail, Context};
    let mut runs: Vec<RunState> = Vec::new();
    for dir in project_state_dirs(cfg, cwd)? {
        let rd = match std::fs::read_dir(&dir) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => bail!("cannot list run state dir {}: {e}", dir.display()),
        };
        for entry in rd {
            let entry = entry.with_context(|| format!("unreadable entry in {}", dir.display()))?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let fname = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if is_run_state_sidecar(fname) {
                continue;
            }
            let txt = std::fs::read_to_string(&path)
                .with_context(|| format!("cannot read run state {}", path.display()))?;
            let rs: RunState = serde_json::from_str(&txt)
                .with_context(|| format!("unparseable run state {}", path.display()))?;
            if !runs.iter().any(|r| r.run_id == rs.run_id) {
                runs.push(rs);
            }
        }
    }
    runs.sort_by(|a, b| a.run_id.cmp(&b.run_id));
    Ok(runs)
}

/// Rule C: when `cwd` is inside a run task's recorded worktree, every selector
/// of every test command must be in that task's scope (reproduction_tests, a
/// test file the worktree changed, or a `--test` whose `-p` packages are all
/// crates the worktree changed — see [`worktree_changed_packages`]). Outside
/// every recorded task worktree this allows. Undetermined (config / run state
/// / decomposition / git / manifest unreadable, cwd owned by several tasks)
/// denies.
fn check_task_scope(tests: &[Words], cwd: &Path) -> Decision {
    let undetermined = |why: String| {
        Decision::Deny(format!(
            "condukt testscope cannot determine whether this test is in a condukt task's scope \
             ({why}), so it is refused: {TASK_SCOPE_REASON}"
        ))
    };
    let cfg = match crate::config::Config::load() {
        Ok(c) => c,
        Err(e) => return undetermined(format!("condukt config unreadable: {e}")),
    };
    let runs = match all_runs_strict(&cfg, cwd) {
        Ok(r) => r,
        Err(e) => return undetermined(format!("run state unreadable: {e:#}")),
    };
    let owners = crate::subagent_stop::tasks_owning_cwd(cwd, &runs);
    let (rs, task) = match owners.as_slice() {
        [] => {
            // A task whose recorded worktree is empty or relative names no
            // directory, so it owns no cwd (backlog b4a2780b: `""` used to own
            // every cwd). But while such a task is still live, a cwd under the
            // condukt worktree base could be its worktree, so ownership there
            // is undetermined. Outside the worktree base, and for terminal
            // tasks, it can be no-one's.
            let live_unusable: Vec<String> =
                crate::subagent_stop::tasks_with_unusable_worktree(&runs)
                    .iter()
                    .filter(|(_, t)| {
                        matches!(
                            t.status,
                            crate::state::Status::Pending
                                | crate::state::Status::Running
                                | crate::state::Status::Done
                                | crate::state::Status::Failed
                        )
                    })
                    .map(|(rs, t)| {
                        format!(
                            "{}/{} (worktree {:?})",
                            rs.run_id,
                            t.id,
                            t.worktree.as_deref().unwrap_or_default()
                        )
                    })
                    .collect();
            if !live_unusable.is_empty() && cwd.starts_with(&cfg.worktree_base) {
                return undetermined(format!(
                    "cwd {} is under the condukt worktree base {} and non-terminal task(s) {} \
                     record a worktree that is not an absolute path, so whether one of them \
                     owns this cwd is undetermined",
                    cwd.display(),
                    cfg.worktree_base.display(),
                    live_unusable.join(", ")
                ));
            }
            return Decision::Allow;
        }
        [one] => *one,
        many => {
            let ids: Vec<String> = many
                .iter()
                .map(|(rs, t)| format!("{}/{}", rs.run_id, t.id))
                .collect();
            return undetermined(format!(
                "cwd {} is inside the recorded worktree of {} tasks ({}); whose scope applies \
                 is undetermined",
                cwd.display(),
                ids.len(),
                ids.join(", ")
            ));
        }
    };
    use harness_core::verdict::Required;
    let repro_text =
        match crate::state::load_decomposition_determined(&cfg, cwd, &rs.run_id).require() {
            Required::Blocked(v) => {
                return undetermined(format!(
                    "decomposition of run '{}' unreadable: {}",
                    rs.run_id,
                    v.as_str()
                ))
            }
            Required::Determined(None) => None,
            Required::Determined(Some(raw)) => {
                let dec: crate::model::Decomposition = match serde_json::from_str(&raw) {
                    Ok(d) => d,
                    Err(e) => {
                        return undetermined(format!(
                            "decomposition of run '{}' unparseable: {e}",
                            rs.run_id
                        ))
                    }
                };
                match dec.tasks.iter().find(|t| t.id == task.id) {
                    Some(t) => t.reproduction_tests.clone(),
                    None => {
                        return undetermined(format!(
                            "task '{}' is not in run '{}''s decomposition",
                            task.id, rs.run_id
                        ))
                    }
                }
            }
        };
    let repro = match repro_text.as_deref().map(reproduction_selectors) {
        None => Vec::new(),
        Some(Ok(s)) => s,
        Some(Err(e)) => return undetermined(e),
    };
    let wt = PathBuf::from(task.worktree.clone().unwrap_or_default());
    let mut changed: Option<Vec<String>> = None;
    let mut changed_pkgs: Option<Vec<String>> = None;
    for words in tests {
        let sels = match selectors(words) {
            Ok(s) => s,
            Err(e) => return undetermined(format!("`{}`: {e}", words.join(" "))),
        };
        // Parsed only when the changed-crate branch needs it, so a cluster we
        // do not model does not refuse a test the other branches admit.
        let pkgs = package_specs(words);
        for sel in sels {
            // A `--test` glob (`*`, `?`, `[]`) may select every integration
            // test of the crate; no branch admits it, reproduction_tests
            // included.
            let glob = matches!(&sel, Selector::Test(n) if !is_plain_name(n));
            if !glob && repro.contains(&sel) {
                continue;
            }
            let allowed = match &sel {
                Selector::Test(_) if glob => false,
                Selector::Test(name) => {
                    if changed.is_none() {
                        match worktree_changed_tests(&wt, &cfg.default_branch) {
                            Ok(c) => changed = Some(c),
                            Err(e) => {
                                return undetermined(format!(
                                    "which tests worktree {} changed is unknown: {e}",
                                    wt.display()
                                ))
                            }
                        }
                    }
                    if changed.as_ref().is_some_and(|c| c.contains(name)) {
                        true
                    } else if let Err(e) = &pkgs {
                        return undetermined(format!("`{}`: {e}", words.join(" ")));
                    } else if pkgs.as_ref().is_ok_and(|p| p.is_empty()) {
                        // No `-p`: which package(s) cargo selects depends on
                        // the cwd and on default-members, so whether the
                        // test belongs to a changed crate is undetermined.
                        false
                    } else {
                        if changed_pkgs.is_none() {
                            match worktree_changed_packages(&wt, &cfg.default_branch) {
                                Ok(c) => changed_pkgs = Some(c),
                                Err(e) => {
                                    return undetermined(format!(
                                        "which crates worktree {} changed is unknown: {e}",
                                        wt.display()
                                    ))
                                }
                            }
                        }
                        // Every selected package must be a changed crate (a
                        // `--test` applies to each `-p`); a spec that is not
                        // a plain package name (glob, `name@ver`, URL) is not
                        // resolved here and so admits nothing.
                        let specs: &[String] = pkgs.as_deref().unwrap_or(&[]);
                        !specs.is_empty()
                            && changed_pkgs.as_ref().is_some_and(|c| {
                                specs
                                    .iter()
                                    .all(|p| is_plain_name(p) && c.iter().any(|n| n == p))
                            })
                    }
                }
                Selector::Kind(..) => false,
            };
            if !allowed {
                let what = match &sel {
                    Selector::Test(n) => format!("--test {n}"),
                    Selector::Kind(k, Some(n)) => format!("{k} {n}"),
                    Selector::Kind(k, None) => k.clone(),
                };
                return Decision::Deny(format!(
                    "`{}` selects `{what}`, which is outside task '{}' of run '{}' (worktree {}): \
                     {TASK_SCOPE_REASON}",
                    words.join(" "),
                    task.id,
                    rs.run_id,
                    wt.display()
                ));
            }
        }
    }
    Decision::Allow
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(cmd: &str) -> Vec<String> {
        shlex::split(cmd).unwrap_or_default()
    }

    /// Rule 1 alone (no ledger, no task scope): what `full_suite_reason`
    /// asserted before `state test` moved onto [`judge`].
    #[test]
    fn rule1_flags_bare_cargo_test() {
        let ctx = Ctx {
            wt_root: None,
            cwd_in_wt: false,
            home: None,
        };
        let dir = Dir::Known(PathBuf::from("/r"));
        assert!(matches!(
            judge_words(&words("cargo test"), &dir, &ctx, 0),
            Judged::Deny(_)
        ));
        assert!(matches!(
            judge_words(&words("cargo test -p x --test y"), &dir, &ctx, 0),
            Judged::Test(_)
        ));
    }
}
