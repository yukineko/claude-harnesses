//! The main-merge rule of `condukt testscope`: inside a condukt task worktree,
//! `git pull` and `git merge|rebase <main>` are denied ([`MAIN_MERGE_REASON`]).
//!
//! Moved here from `testscope.rs` to keep that file under the file-growth cap
//! (backlog 643e641f, 2026-11-22). New with that backlog item: the default
//! branch is recognised by how a ref spelling *resolves*, not by a fixed
//! string list ([`main_ref`]). Before, only `main | master | origin/main |
//! origin/master | @{u} | @{upstream}` were denied, so `refs/heads/main` or
//! `refs/remotes/origin/main` slipped through.
//!
//! What is judged (operands of `merge` / `rebase`, see [`main_ref`]). An
//! operand is a token not starting with `-`, every token after `--`, the
//! bare `-` (git's shorthand for `@{-1}`, judged as `@{-1}`, i.e.
//! undetermined), and the attached value of a ref-valued long option
//! (`rebase --onto=<v>`, also any unambiguous abbreviation such as
//! `--ont=<v>`; the separated `--onto <v>` value is an ordinary operand):
//! - main (`main`/`master`, compared ASCII-case-insensitively, since a loose
//!   ref on a case-insensitive filesystem resolves `MAIN` to `main`) spelled
//!   as a short name, `heads/<m>`, `refs/heads/<m>`, `<remote>/<m>`,
//!   `remotes/<remote>/<m>`, `refs/remotes/<remote>/<m>`; a remote's `HEAD`
//!   (`origin/HEAD`, `refs/remotes/origin/HEAD`), which points at its default
//!   branch;
//! - any of those with a revision suffix (`~N`, `^N`, `^{…}`, `@{N}` /
//!   `@{date}`): `main~0`, `main^0`, `origin/main@{1}` are main;
//! - an upstream / push / previous-branch suffix (`@{u}`, `@{upstream}`,
//!   `@{push}`, `@{-N}`) on any branch, or bare: what it names is not known
//!   from the text, so it is undetermined and denied;
//! - an operand with shell-expansion or glob characters (`$`, `` ` ``, `*`,
//!   `?`, `[`, `\`, or `{`/`}` before the first revision suffix, i.e.
//!   outside an `@{…}` / `^{…}` suffix): its value is not known from the
//!   text, so it is undetermined and denied;
//! - `tags/main`, `tags/master`, `tags/HEAD`, `heads/HEAD`: ambiguous (a
//!   remote may be named `tags` / `heads`), so undetermined and denied
//!   (user ruling 2026-11-27, bd0bf1f9). `heads/main` stays main: both
//!   readings are main;
//! - any other ref under `refs/` whose last component is a main name or
//!   `HEAD` (`refs/foo/main`): denied (fail closed).
//!
//! An ordinary branch name (`feature-x`, `condukt/<run>/t1`, `fix/main-ui`) is
//! allowed: it is not main and nothing in its spelling is undetermined. A
//! `<one-component>/<main name>` spelling (`upstream/main`) is read as a
//! remote-tracking main and denied, even though a local branch could carry
//! that name (fail closed).
//!
//! NOT judged (allowed; the rule works on the text only, nothing is
//! resolved through git): a commit sha of main, a tag or another branch that
//! points at main's commit, a branch whose configured upstream is main merged
//! by its plain name, the attached values of `--opt=<v>` options that do not
//! take a ref (`--strategy-option=`, `--message=`, `--exec=`, ...), short
//! options with an attached value (`-X<opt>`, `-m<msg>`), and history-moving
//! subcommands other than `pull` / `merge` / `rebase` (`reset`,
//! `cherry-pick`, `checkout`, ...).

use super::{Judged, MAIN_MERGE_REASON};

/// Git global options (from `git --help` / git(1) "OPTIONS") that take a value.
/// With `=` the value is attached; otherwise the next token is the value.
const GIT_VALUE_OPTS: &[&str] = &[
    "--git-dir",
    "--work-tree",
    "--namespace",
    "--super-prefix",
    "--config-env",
    "--attr-source",
];
/// Git global options that never take a separate value token.
const GIT_FLAG_OPTS: &[&str] = &[
    "-p",
    "-P",
    "--paginate",
    "--no-pager",
    "--bare",
    "--no-replace-objects",
    "--literal-pathspecs",
    "--glob-pathspecs",
    "--noglob-pathspecs",
    "--icase-pathspecs",
    "--no-optional-locks",
    "--no-lazy-fetch",
    "--no-advice",
    "--exec-path",
    "--html-path",
    "--man-path",
    "--info-path",
    "--list-cmds",
    "-v",
    "--version",
    "-h",
    "--help",
];

/// Subcommand options whose separate next token is free text (a message, a
/// file, a shell command), never a ref: skipped so the text is not judged as
/// a ref spelling. Per subcommand, because `-m` is `--message <msg>` for
/// `merge` but the value-less `--merge` for `rebase`.
fn text_value_opts(sub: &str) -> &'static [&'static str] {
    match sub {
        "merge" => &["-m", "--message", "-F", "--file"],
        "rebase" => &["-x", "--exec"],
        _ => &[],
    }
}

/// Long subcommand options whose value is a ref (`rebase --onto <newbase>`).
/// The separated form needs no special case: its value token is collected
/// as an ordinary operand.
fn ref_value_opts(sub: &str) -> &'static [&'static str] {
    match sub {
        "rebase" => &["onto"],
        _ => &[],
    }
}

/// The attached value of a ref-valued long option (`--onto=<v>`), if `a` is
/// one. git's parse-options accepts any unambiguous prefix of a long option
/// name (`--ont=<v>`), so every non-empty prefix of a ref-valued option name
/// is treated as that option (fail closed: the value is only judged, so a
/// value that is not main stays allowed). Values of other `--opt=<v>`
/// options (`--strategy-option=`, `--message=`, ...) are not refs and are
/// not judged.
fn ref_valued_attached<'a>(sub: &str, a: &'a str) -> Option<&'a str> {
    let (name, value) = a.strip_prefix("--")?.split_once('=')?;
    (!name.is_empty() && ref_value_opts(sub).iter().any(|o| o.starts_with(name))).then_some(value)
}

/// How a ref spelling relates to the default branch.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum MainRef {
    /// The spelling names main (or a remote's default branch).
    Main,
    /// What the spelling names cannot be told from its text (shell
    /// expansion, glob, upstream / previous-branch suffix). Denied.
    Undetermined,
    /// An ordinary ref that is not main.
    NotMain,
}

fn is_main_name(s: &str) -> bool {
    s.eq_ignore_ascii_case("main") || s.eq_ignore_ascii_case("master")
}

/// Classify one ref spelling (see the module doc for the rules).
pub(super) fn main_ref(spelling: &str) -> MainRef {
    if spelling
        .chars()
        .any(|c| matches!(c, '$' | '`' | '*' | '?' | '[' | '\\'))
    {
        return MainRef::Undetermined;
    }
    // Split off revision suffixes: the base ends at the first `~`, `^` or `@{`.
    let cut = [spelling.find('~'), spelling.find('^'), spelling.find("@{")]
        .into_iter()
        .flatten()
        .min()
        .unwrap_or(spelling.len());
    let (base, suffix) = spelling.split_at(cut);
    // `{` is only meaningful inside an `@{…}` / `^{…}` suffix; anywhere else
    // it is brace-expansion text.
    if base.contains(['{', '}']) {
        return MainRef::Undetermined;
    }
    // Every `@{…}` in the suffix: a reflog index / date keeps the base; an
    // upstream / push / previous-branch selector names a branch the text
    // does not reveal.
    let mut rest = suffix;
    while let Some(at) = rest.find("@{") {
        let inner = &rest[at + 2..];
        let Some(close) = inner.find('}') else {
            return MainRef::Undetermined;
        };
        let sel = inner[..close].to_ascii_lowercase();
        if sel == "u" || sel == "upstream" || sel == "push" || sel.starts_with('-') {
            return MainRef::Undetermined;
        }
        rest = &inner[close + 1..];
    }
    if base.is_empty() || base == "@" {
        // A bare `@{N}` / `@^` etc. is HEAD's own history (bare `@{u}` was
        // handled above).
        return MainRef::NotMain;
    }
    let comps: Vec<&str> = base.split('/').collect();
    let main_or_head = |s: &str| is_main_name(s) || s == "HEAD";
    // `tags/<main|HEAD>` and `heads/HEAD` are ambiguous: a remote may be named
    // `tags` / `heads`, so they read as either a tag/branch or a remote-tracking
    // main. The text cannot tell which (user ruling 2026-11-27, bd0bf1f9), so
    // fail closed. `heads/main` is not ambiguous: both readings are main.
    if let ["tags", m] = comps.as_slice() {
        if main_or_head(m) {
            return MainRef::Undetermined;
        }
    }
    if comps.as_slice() == ["heads", "HEAD"] {
        return MainRef::Undetermined;
    }
    let verdict = match comps.as_slice() {
        [m] => is_main_name(m),
        ["heads", m] | ["refs", "heads", m] => is_main_name(m),
        [_remote, m] | ["remotes", _remote, m] | ["refs", "remotes", _remote, m] => main_or_head(m),
        ["refs", "tags", ..] | ["tags", ..] => false,
        ["refs", .., last] => main_or_head(last),
        _ => false,
    };
    if verdict {
        MainRef::Main
    } else {
        MainRef::NotMain
    }
}

pub(super) fn judge_git(args: &[String]) -> Judged {
    let mut i = 0;
    // Global options before the subcommand. Value-taking ones consume their
    // value so it is not mistaken for the subcommand; an option not in the
    // known lists leaves the subcommand position undetermined -> deny.
    while let Some(a) = args.get(i) {
        if a == "-C" || a == "-c" {
            i += 2;
        } else if let Some(long) = a.strip_prefix("--") {
            let (name, inline) = match long.split_once('=') {
                Some((n, _)) => (n, true),
                None => (long, false),
            };
            let flag = format!("--{name}");
            if GIT_VALUE_OPTS.contains(&flag.as_str()) {
                i += if inline { 1 } else { 2 };
            } else if GIT_FLAG_OPTS.contains(&flag.as_str())
                || (inline && flag == "--exec-path")
                || (inline && flag == "--list-cmds")
            {
                i += 1;
            } else {
                return Judged::Deny(format!(
                    "`git {}`: unknown git global option `{a}`; the subcommand cannot be determined, so the main-merge check fails closed ({MAIN_MERGE_REASON})",
                    args.join(" ")
                ));
            }
        } else if a.starts_with('-') {
            if GIT_FLAG_OPTS.contains(&a.as_str()) {
                i += 1;
            } else {
                return Judged::Deny(format!(
                    "`git {}`: unknown git global option `{a}`; the subcommand cannot be determined, so the main-merge check fails closed ({MAIN_MERGE_REASON})",
                    args.join(" ")
                ));
            }
        } else {
            break;
        }
    }
    let Some(sub) = args.get(i) else {
        return Judged::Other;
    };
    if sub == "pull" {
        return Judged::Deny(format!("`git {}`: {MAIN_MERGE_REASON}", args.join(" ")));
    }
    if sub != "merge" && sub != "rebase" {
        return Judged::Other;
    }
    let skip = text_value_opts(sub);
    let mut operands: Vec<&str> = Vec::new();
    let mut rest = args[i + 1..].iter();
    let mut end_of_opts = false;
    while let Some(a) = rest.next() {
        if end_of_opts {
            operands.push(a);
        } else if a == "--" {
            // Everything after `--` is an operand, even if it starts with `-`.
            end_of_opts = true;
        } else if a == "-" {
            // git's shorthand for `@{-1}` (the previous branch); judged as
            // that spelling, i.e. undetermined.
            operands.push("@{-1}");
        } else if skip.contains(&a.as_str()) {
            rest.next();
        } else if let Some(v) = ref_valued_attached(sub, a) {
            operands.push(v);
        } else if !a.starts_with('-') {
            operands.push(a);
        }
    }
    for o in operands {
        match main_ref(o) {
            MainRef::Main => {
                return Judged::Deny(format!("`git {}`: {MAIN_MERGE_REASON}", args.join(" ")));
            }
            MainRef::Undetermined => {
                return Judged::Deny(format!(
                    "`git {}`: the ref `{o}` cannot be resolved from its text (shell expansion, glob, an ambiguous `tags/` / `heads/` spelling, or an upstream / previous-branch selector), so the main-merge check fails closed ({MAIN_MERGE_REASON})",
                    args.join(" ")
                ));
            }
            MainRef::NotMain => {}
        }
    }
    Judged::Other
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn main_spellings_are_main() {
        for s in [
            "main",
            "master",
            "MAIN",
            "origin/main",
            "origin/master",
            "upstream/main",
            "origin/HEAD",
            "heads/main",
            "refs/heads/main",
            "refs/heads/master",
            "remotes/origin/main",
            "refs/remotes/origin/main",
            "refs/remotes/origin/HEAD",
            "main~0",
            "main^0",
            "main^{commit}",
            "origin/main~2",
            "main@{1}",
            "refs/foo/main",
        ] {
            assert_eq!(main_ref(s), MainRef::Main, "{s}");
        }
    }

    #[test]
    fn undetermined_spellings_are_undetermined() {
        for s in [
            "@{u}",
            "@{upstream}",
            "@{push}",
            "@{-1}",
            "feature@{u}",
            "$BR",
            "`x`",
            "ma*n",
            "ma?n",
            "[m]ain",
            "{main,x}",
            "ma\\in",
            "main@{1",
        ] {
            assert_eq!(main_ref(s), MainRef::Undetermined, "{s}");
        }
    }

    #[test]
    fn ordinary_branches_are_not_main() {
        for s in [
            "feature-x",
            "condukt/run-20261122-035251-84424/t1",
            "fix/main-ui",
            "mainline",
            "refs/heads/feature",
            "refs/tags/main",
            "HEAD",
            "@",
            "@{1}",
            "feature~1",
        ] {
            assert_eq!(main_ref(s), MainRef::NotMain, "{s}");
        }
        // Re-anchored per user ruling 2026-11-27 (bd0bf1f9): `tags/main` is
        // ambiguous (refs/tags/main vs refs/remotes/tags/main when a remote
        // named `tags` exists), so it is Undetermined (fail closed), not NotMain.
        assert_eq!(main_ref("tags/main"), MainRef::Undetermined, "tags/main");
    }

    #[test]
    fn two_segment_tags_heads_ambiguity_is_undetermined() {
        // `tags/<m>` / `heads/HEAD` could be a remote named `tags` / `heads`.
        for s in ["tags/main", "tags/master", "tags/HEAD", "heads/HEAD"] {
            assert_eq!(main_ref(s), MainRef::Undetermined, "{s}");
        }
        // Both readings are main, so no ambiguity in the verdict.
        assert_eq!(main_ref("heads/main"), MainRef::Main);
        // A non-main name under `tags/` is not a main spelling under any reading.
        assert_eq!(main_ref("tags/v1"), MainRef::NotMain);
        assert!(matches!(
            judge_git(&words("merge tags/main")),
            Judged::Deny(_)
        ));
    }

    fn words(cmd: &str) -> Vec<String> {
        shlex::split(cmd).unwrap_or_default()
    }

    #[test]
    fn merge_message_text_is_not_a_ref() {
        assert!(matches!(
            judge_git(&words("merge -m 'why? $x' feature-x")),
            Judged::Other
        ));
        assert!(matches!(
            judge_git(&words("rebase -m main")),
            Judged::Deny(_)
        ));
        assert!(matches!(
            judge_git(&words("rebase --onto refs/heads/main a b")),
            Judged::Deny(_)
        ));
    }

    #[test]
    fn dash_operands_are_judged() {
        for cmd in [
            "rebase --onto=main x",
            "rebase --ont=origin/main x",
            "rebase --onto=$B x",
            "merge -",
            "rebase -",
            "merge -- main",
        ] {
            assert!(matches!(judge_git(&words(cmd)), Judged::Deny(_)), "{cmd}");
        }
        for cmd in [
            "rebase --onto=feature x",
            "merge --strategy-option=theirs feature-x",
            "merge --strategy-option=$X feature-x",
            "merge --message=main feature-x",
            "merge --into-name=main feature-x",
            "merge -- feature-x",
        ] {
            assert!(matches!(judge_git(&words(cmd)), Judged::Other), "{cmd}");
        }
    }
}
