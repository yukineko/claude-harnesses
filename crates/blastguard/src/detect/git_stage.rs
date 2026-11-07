//! Whole-tree staging: `git add -A` / `--all` / `-u` / `.` and `git commit -a`
//! (backlog e9325654; memory `condukt-operator-gotchas`, "Committing workers
//! must not touch foreign dirty files"). In a working tree that other sessions
//! also edit, staging "everything" sweeps their uncommitted work into this
//! session's commit. Staging explicit paths stays Allow.
//!
//! The verdict is purely about the command's argv; it does not depend on
//! WHICH repository git acts on. So the git globals before the subcommand
//! (`-C DIR`, `--git-dir`, `--work-tree`, `-c k=v`) do not change it, and no
//! repository probe runs: a sweep is a sweep in any checkout. There is no
//! linked-worktree exemption either — the memory rule is addressed to every
//! committing worker, worktree workers included.
//!
//! Ask (never Allow) when:
//!
//! * `git add` / `git stage` (its synonym) has `-A`, `--all`,
//!   `--no-ignore-removal`, `-u`, `--update` or `--renormalize` (which implies
//!   `-u`) with NO pathspec: each stages every change in the whole tree
//!   (`-u`: every tracked modification, foreign edits included). With a
//!   pathspec they are limited to it, and the pathspec is judged instead.
//! * `git commit` has `-a` / `--all`, alone or in any short-option cluster
//!   (`-am`, `-va`), with or without a pathspec (with one, git refuses the
//!   combination and nothing is committed — the Ask costs nothing there).
//!   Short options are parsed the way git's parse-options does: in `-mfoo` /
//!   `-Ffile` / `-Cref` / `-cref` / `-tfile` the rest of the cluster is the
//!   value, and `-u<mode>` / `-S<keyid>` take an optional stuck value, so an
//!   `a` there is not the flag.
//! * any pathspec of `add`/`stage`/`commit` is whole-tree: one whose path
//!   components are all `.` / `..` (the current directory or an ancestor —
//!   `.`, `./`, `..`), one starting with `:` (pathspec magic: `:/` is the
//!   repository root, `:!x` / `:^x` mean "everything except x", and the other
//!   magic words are not modelled), or one holding a glob character
//!   (`*`, `?`, `[`; git matches a pathspec as a glob across the whole tree
//!   even when the shell did not expand it, so `'*'` and `'*.rs'` are not
//!   explicit paths). `git commit <pathspec>` commits the current content of
//!   every tracked file the pathspec matches, so a whole-tree pathspec there
//!   is the same sweep as `-a`.
//! * `--pathspec-from-file`: the paths are not on the command line.
//! * a long option abbreviation that could be one of the above (`--al`,
//!   `--up`, `--ren`): git accepts any unambiguous prefix, and an ambiguous
//!   one is resolved to the restrictive side.
//! * the argv cannot be determined (CLAUDE.md §3): an unclosed quote or
//!   substitution, a backslash before whitespace, an unknown option or short
//!   flag, a word whose content is a runtime expansion (`$VAR`, `$(…)`,
//!   `` `…` ``, `$'…'`, a leading `~`) in a flag or pathspec position, or an
//!   unquoted expansion / glob / brace list anywhere (the shell may split it
//!   into extra words, e.g. `-m *` passes every file as a pathspec). An
//!   expansion INSIDE double quotes as the value of a value-taking option
//!   (`-m "$(cat <<'EOF' … EOF)"`) is one word git consumes as the value and
//!   stays Allow.
//!
//! Residual, measured as not covered: an explicit directory pathspec
//! (`git add src/`, or an absolute path naming the repository root) stages
//! everything under it and stays Allow (a directory is not told apart from a
//! file without reading the filesystem); a git alias
//! whose body is `add -A` / `commit -a`; the `GIT_*` environment.
//!
//! Here-documents inside `$(…)`: the substitution is read with
//! [`closed_substitution_end_strict`], which models a here-document body as
//! data (an apostrophe in it, `don't`, is not a quote; a `)` in it closes
//! nothing), so `-m "$(cat <<'EOF'` … `EOF` … `)"` is one closed word and
//! Allow. The argument text is read from the raw segment, newlines included
//! (see `raw_args`). Over-approximation, deliberately on the Ask side: a body
//! with no exact terminator line (indented, a substring of a longer line, or
//! missing), an unreadable delimiter, an UNQUOTED delimiter whose body holds
//! `$`, `` ` `` or `\` (it is expanded), a here-document inside a backtick
//! body, and argument tokens that cannot be matched back to the raw segment
//! (they are then re-joined with spaces, so every here-document looks
//! unterminated) all make the word undeterminable and the line Ask.

use super::{closed_substitution_end_strict, Decision};

/// The reason for every provable whole-tree stage. ONE wording so
/// [`crate::rule_id`] files the class under one signature.
pub(super) const STAGE_SWEEP_REASON: &str =
    "whole-tree staging (git add -A/--all/-u/., git commit -a/--all, or a pathspec matching \
the whole tree) sweeps other sessions' uncommitted work into this commit — stage explicit \
paths instead (`git add <path>…`, `git commit -m … -- <path>…`)";

/// The reason when the argv cannot be determined.
fn undetermined(why: &str) -> Decision {
    Decision::ask(format!(
        "blastguard cannot determine whether this git add/commit is whole-tree staging \
({why}), which sweeps other sessions' uncommitted work into this commit — stage explicit \
paths instead; a commit message is readable as `git commit -F - <<'EOF'` … `EOF`"
    ))
}

/// One argv word after quote removal.
#[derive(Default)]
struct Word {
    /// The literal text (expansions contribute nothing).
    text: String,
    /// Some of the content is a runtime expansion: the text is not the word.
    opaque: bool,
    /// An UNQUOTED expansion, glob or brace list: the shell may turn this one
    /// token into any number of words, so not even the word count is known.
    split: bool,
}

/// Skip a `$`-expansion or backquote starting at `chars[i]`, marking `w`
/// opaque. Returns the index just past it.
fn expansion(chars: &[char], i: usize, w: &mut Word) -> Result<usize, &'static str> {
    let next = chars.get(i + 1).copied();
    if chars[i] == '`' || next == Some('(') {
        let end =
            closed_substitution_end_strict(chars, i).ok_or("an unclosed command substitution")?;
        w.opaque = true;
        return Ok(end + 1);
    }
    match next {
        Some('{') => {
            let close = chars[i + 2..]
                .iter()
                .position(|&c| c == '}')
                .ok_or("an unclosed ${…}")?;
            w.opaque = true;
            Ok(i + 2 + close + 1)
        }
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {
            let mut j = i + 1;
            while chars
                .get(j)
                .is_some_and(|c| c.is_ascii_alphanumeric() || *c == '_')
            {
                j += 1;
            }
            w.opaque = true;
            Ok(j)
        }
        Some(c) if c.is_ascii_digit() || "@*#?$!-".contains(c) => {
            w.opaque = true;
            Ok(i + 2)
        }
        _ => {
            w.text.push('$');
            Ok(i + 1)
        }
    }
}

/// Body of a `"…"` starting just past the opening quote. Returns the index
/// just past the closing quote.
fn double_quoted(chars: &[char], mut i: usize, w: &mut Word) -> Result<usize, &'static str> {
    while let Some(&c) = chars.get(i) {
        match c {
            '"' => return Ok(i + 1),
            '\\' => match chars.get(i + 1) {
                Some(&d) if matches!(d, '$' | '`' | '"' | '\\') => {
                    w.text.push(d);
                    i += 2;
                }
                // The words may have been re-joined with single spaces (see
                // `raw_args`), so whether this was an escaped newline (a
                // continuation) is not known in general; not modelled.
                Some(d) if d.is_whitespace() => return Err("a backslash before whitespace"),
                Some(_) => {
                    w.text.push('\\');
                    i += 1;
                }
                None => return Err("an unclosed double quote"),
            },
            '$' | '`' => i = expansion(chars, i, w)?,
            c => {
                w.text.push(c);
                i += 1;
            }
        }
    }
    Err("an unclosed double quote")
}

/// Split the argument text into the argv words bash would produce, with
/// quote removal, without performing any expansion.
fn shell_words(text: &str) -> Result<Vec<Word>, &'static str> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut w = Word::default();
    let mut in_word = false;
    let mut i = 0;
    while let Some(&c) = chars.get(i) {
        if c.is_whitespace() {
            if in_word {
                out.push(std::mem::take(&mut w));
                in_word = false;
            }
            i += 1;
            continue;
        }
        let at_start = !in_word;
        in_word = true;
        match c {
            '\'' => {
                let end = chars[i + 1..]
                    .iter()
                    .position(|&d| d == '\'')
                    .ok_or("an unclosed single quote")?;
                w.text.extend(&chars[i + 1..i + 1 + end]);
                i += end + 2;
            }
            '"' => i = double_quoted(&chars, i + 1, &mut w)?,
            '\\' => match chars.get(i + 1) {
                Some(&d) if !d.is_whitespace() => {
                    w.text.push(d);
                    i += 2;
                }
                _ => return Err("a backslash before whitespace or at the end"),
            },
            '$' if chars.get(i + 1) == Some(&'\'') => {
                // ANSI-C quoting: the escapes are not decoded, the word is
                // opaque.
                let mut j = i + 2;
                loop {
                    match chars.get(j) {
                        Some('\\') => j += 2,
                        Some('\'') => break,
                        Some(_) => j += 1,
                        None => return Err("an unclosed $'…'"),
                    }
                }
                w.opaque = true;
                i = j + 1;
            }
            '$' if chars.get(i + 1) == Some(&'"') => i = double_quoted(&chars, i + 2, &mut w)?,
            '$' | '`' => {
                i = expansion(&chars, i, &mut w)?;
                // An unquoted expansion is word-split by the shell.
                w.split |= w.opaque;
            }
            '*' | '?' | '[' => {
                w.text.push(c);
                w.split = true;
                i += 1;
            }
            '{' => {
                // Brace list or sequence later in the same token.
                let tail: String = chars[i + 1..]
                    .iter()
                    .take_while(|c| !c.is_whitespace())
                    .collect();
                if tail.contains(',') || tail.contains("..") {
                    w.split = true;
                }
                w.text.push(c);
                i += 1;
            }
            '~' if at_start => {
                w.text.push(c);
                w.opaque = true;
                i += 1;
            }
            c => {
                w.text.push(c);
                i += 1;
            }
        }
    }
    if in_word {
        out.push(w);
    }
    Ok(out)
}

/// True for a pathspec that matches the whole tree (or the current
/// directory's whole subtree, or an ancestor's) — see the module doc.
fn whole_tree_pathspec(p: &str) -> bool {
    if p.starts_with(':') || p.contains(['*', '?', '[']) {
        return true;
    }
    // Lexically normalised, nothing below the starting directory remains
    // (`.`, `./`, `..`, `a/..`, the empty pathspec, `/`). Any other path —
    // an absolute path naming the repository root included — is a directory
    // or file pathspec: the residual named in the module doc.
    let mut depth: usize = 0;
    for c in p.split('/') {
        match c {
            "" | "." => {}
            ".." => depth = depth.saturating_sub(1),
            _ => depth += 1,
        }
    }
    depth == 0
}

/// How a long option behaves, by full name.
#[derive(Clone, Copy, PartialEq)]
enum Long {
    /// Stages the whole tree (judged with the pathspec for `add`).
    Sweep,
    /// Paths come from a file: never Allow.
    FromFile,
    /// Takes a required value (`--opt v` or `--opt=v`).
    Value,
    /// Takes no value, or an optional one that must be stuck (`--opt=v`).
    Flag,
}

const ADD_LONG: &[(&str, Long)] = &[
    ("all", Long::Sweep),
    ("no-ignore-removal", Long::Sweep),
    ("update", Long::Sweep),
    ("renormalize", Long::Sweep),
    ("pathspec-from-file", Long::FromFile),
    ("chmod", Long::Value),
    ("dry-run", Long::Flag),
    ("verbose", Long::Flag),
    ("interactive", Long::Flag),
    ("patch", Long::Flag),
    ("edit", Long::Flag),
    ("force", Long::Flag),
    ("intent-to-add", Long::Flag),
    ("refresh", Long::Flag),
    ("ignore-errors", Long::Flag),
    ("ignore-missing", Long::Flag),
    ("ignore-removal", Long::Flag),
    ("no-all", Long::Flag),
    ("sparse", Long::Flag),
    ("pathspec-file-nul", Long::Flag),
    ("no-warn-embedded-repo", Long::Flag),
];

const COMMIT_LONG: &[(&str, Long)] = &[
    ("all", Long::Sweep),
    ("pathspec-from-file", Long::FromFile),
    ("message", Long::Value),
    ("file", Long::Value),
    ("reuse-message", Long::Value),
    ("reedit-message", Long::Value),
    ("template", Long::Value),
    ("author", Long::Value),
    ("date", Long::Value),
    ("cleanup", Long::Value),
    ("trailer", Long::Value),
    ("fixup", Long::Value),
    ("squash", Long::Value),
    ("gpg-sign", Long::Flag),
    ("untracked-files", Long::Flag),
    ("amend", Long::Flag),
    ("no-edit", Long::Flag),
    ("edit", Long::Flag),
    ("allow-empty", Long::Flag),
    ("allow-empty-message", Long::Flag),
    ("no-post-rewrite", Long::Flag),
    ("no-verify", Long::Flag),
    ("verify", Long::Flag),
    ("signoff", Long::Flag),
    ("no-signoff", Long::Flag),
    ("no-gpg-sign", Long::Flag),
    ("reset-author", Long::Flag),
    ("include", Long::Flag),
    ("only", Long::Flag),
    ("interactive", Long::Flag),
    ("patch", Long::Flag),
    ("dry-run", Long::Flag),
    ("short", Long::Flag),
    ("branch", Long::Flag),
    ("porcelain", Long::Flag),
    ("long", Long::Flag),
    ("null", Long::Flag),
    ("verbose", Long::Flag),
    ("quiet", Long::Flag),
    ("status", Long::Flag),
    ("no-status", Long::Flag),
    ("ahead-behind", Long::Flag),
    ("no-ahead-behind", Long::Flag),
    ("pathspec-file-nul", Long::Flag),
    ("no-all", Long::Flag),
];

/// Resolve a long option name the way git's parse-options does: an exact
/// name, else every name it is a prefix of. `None` = no such option (git
/// errors, but blastguard cannot tell what was meant: undetermined). When
/// several names match, the most restrictive kind wins (`Sweep`/`FromFile`
/// over the rest; `Value` over `Flag`, so a possible value is skipped rather
/// than read as a pathspec).
fn long_kind(table: &[(&str, Long)], name: &str) -> Option<Long> {
    if let Some((_, k)) = table.iter().find(|(n, _)| *n == name) {
        return Some(*k);
    }
    let matches: Vec<Long> = table
        .iter()
        .filter(|(n, _)| !name.is_empty() && n.starts_with(name))
        .map(|(_, k)| *k)
        .collect();
    [Long::Sweep, Long::FromFile, Long::Value, Long::Flag]
        .into_iter()
        .find(|k| matches.contains(k))
}

/// Which `git` subcommand is being judged.
#[derive(Clone, Copy, PartialEq)]
enum Sub {
    Add,
    Commit,
}

/// Short options of `git add`: all boolean; `A` and `u` sweep.
fn add_short(cluster: &str) -> Result<bool, &'static str> {
    let mut sweep = false;
    for c in cluster.chars() {
        match c {
            'A' | 'u' => sweep = true,
            'n' | 'v' | 'f' | 'i' | 'p' | 'e' | 'N' => {}
            _ => return Err("an unknown git add short option"),
        }
    }
    Ok(sweep)
}

/// Short options of `git commit`. Returns (sweeps, the NEXT word is a value).
fn commit_short(cluster: &str) -> Result<(bool, bool), &'static str> {
    let mut sweep = false;
    let chars: Vec<char> = cluster.chars().collect();
    for (k, &c) in chars.iter().enumerate() {
        match c {
            'a' => sweep = true,
            // Required value: the rest of the cluster, or the next word.
            'm' | 'F' | 'C' | 'c' | 't' => return Ok((sweep, k + 1 == chars.len())),
            // Optional value, stuck only: the rest of the cluster is it.
            'u' | 'S' => return Ok((sweep, false)),
            'p' | 's' | 'n' | 'e' | 'i' | 'o' | 'v' | 'q' | 'z' | 'h' => {}
            _ => return Err("an unknown git commit short option"),
        }
    }
    Ok((sweep, false))
}

/// The text of `args` as it stands in `seg`, newlines included, when `args`
/// are exactly the trailing whitespace-separated tokens of `seg` (which they
/// are when the segment analyser split them from it). `None` when they are
/// not, or `seg` is unknown; the caller then re-joins the tokens with single
/// spaces, which loses every newline — a here-document inside `$(…)` is then
/// unreadable and [`closed_substitution_end_strict`] reports it unclosed (Ask).
fn raw_args<'s>(seg: Option<&'s str>, args: &[&str]) -> Option<&'s str> {
    let seg = seg?;
    let mut spans: Vec<(usize, usize)> = Vec::new();
    let mut start: Option<usize> = None;
    for (i, c) in seg.char_indices() {
        match (c.is_whitespace(), start) {
            (true, Some(s)) => {
                spans.push((s, i));
                start = None;
            }
            (false, None) => start = Some(i),
            _ => {}
        }
    }
    if let Some(s) = start {
        spans.push((s, seg.len()));
    }
    let first = spans.len().checked_sub(args.len())?;
    let tail = &spans[first..];
    if tail.iter().zip(args).any(|(&(s, e), a)| &seg[s..e] != *a) {
        return None;
    }
    Some(tail.first().map_or("", |&(s, _)| &seg[s..]))
}

fn verdict(sub: Sub, args: &[&str], seg: Option<&str>) -> Decision {
    let joined;
    let text = match raw_args(seg, args) {
        Some(raw) => raw,
        None => {
            joined = args.join(" ");
            joined.as_str()
        }
    };
    let words = match shell_words(text) {
        Ok(w) => w,
        Err(why) => return undetermined(why),
    };
    if words.iter().any(|w| w.split) {
        return undetermined("an unquoted expansion, glob or brace list the shell may split");
    }
    let table = if sub == Sub::Add {
        ADD_LONG
    } else {
        COMMIT_LONG
    };
    let mut sweep = false;
    let mut pathspecs: Vec<&Word> = Vec::new();
    let mut it = words.iter();
    let mut options_end = false;
    while let Some(w) = it.next() {
        if options_end || w.text == "-" || !w.text.starts_with('-') {
            if !options_end && w.opaque {
                // A runtime value could be an option as well as a path.
                return undetermined("a runtime expansion where an option may stand");
            }
            pathspecs.push(w);
            continue;
        }
        if w.opaque {
            return undetermined("a runtime expansion inside an option word");
        }
        if w.text == "--" {
            options_end = true;
            continue;
        }
        let takes_next = if let Some(long) = w.text.strip_prefix("--") {
            let (name, stuck) = match long.split_once('=') {
                Some((n, _)) => (n, true),
                None => (long, false),
            };
            match long_kind(table, name) {
                None => return undetermined("an unknown long option"),
                Some(Long::FromFile) => {
                    return undetermined("--pathspec-from-file: the paths are not on the line")
                }
                Some(Long::Sweep) => {
                    sweep = true;
                    false
                }
                Some(Long::Value) => !stuck,
                Some(Long::Flag) => false,
            }
        } else {
            let cluster = &w.text[1..];
            let res = if sub == Sub::Add {
                add_short(cluster).map(|s| (s, false))
            } else {
                commit_short(cluster)
            };
            match res {
                Ok((s, next)) => {
                    sweep |= s;
                    next
                }
                Err(why) => return undetermined(why),
            }
        };
        if takes_next && it.next().is_none() {
            return undetermined("an option is missing its value");
        }
    }
    if pathspecs.iter().any(|p| p.opaque) {
        return undetermined("a pathspec that is a runtime expansion");
    }
    if pathspecs.iter().any(|p| whole_tree_pathspec(&p.text)) {
        return Decision::ask(STAGE_SWEEP_REASON);
    }
    // `git add -A <path>` is limited to the path; `git commit -a` sweeps
    // whatever follows it.
    if sweep && (sub == Sub::Commit || pathspecs.is_empty()) {
        return Decision::ask(STAGE_SWEEP_REASON);
    }
    Decision::Allow
}

/// Verdict for `git add` / `git stage`; `args` are the raw tokens after the
/// subcommand.
pub(super) fn add_verdict(args: &[&str], seg: Option<&str>) -> Decision {
    verdict(Sub::Add, args, seg)
}

/// Verdict for `git commit`; `args` are the raw tokens after the subcommand.
pub(super) fn commit_verdict(args: &[&str], seg: Option<&str>) -> Decision {
    verdict(Sub::Commit, args, seg)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(sub: Sub, line: &str) -> &'static str {
        let toks: Vec<&str> = line.split_whitespace().collect();
        match verdict(sub, &toks, Some(line)) {
            Decision::Allow => "allow",
            Decision::Ask(_) => "ask",
            _ => "deny",
        }
    }

    #[test]
    fn add_cases() {
        for (line, want) in [
            ("-A", "ask"),
            ("--all", "ask"),
            ("--al", "ask"),
            ("--a", "ask"),
            ("-u", "ask"),
            ("--update", "ask"),
            ("--up", "ask"),
            ("--renormalize", "ask"),
            ("--no-ignore-removal", "ask"),
            ("-vA", "ask"),
            (".", "ask"),
            ("./", "ask"),
            ("..", "ask"),
            ("-- .", "ask"),
            (":/", "ask"),
            (":!foo", "ask"),
            ("'*'", "ask"),
            ("*", "ask"),
            ("src/*.rs", "ask"),
            ("'*.rs'", "ask"),
            ("$F", "ask"),
            ("\"$F\"", "ask"),
            ("~", "ask"),
            ("--pathspec-from-file=x", "ask"),
            ("--bogus", "ask"),
            ("-Z", "ask"),
            ("'unclosed", "ask"),
            // Controls.
            ("tracked.txt", "allow"),
            ("-- tracked.txt", "allow"),
            ("-A tracked.txt", "allow"),
            ("-u src/a.rs", "allow"),
            ("-N new.txt", "allow"),
            ("--chmod +x run.sh", "allow"),
            ("--chmod=+x run.sh", "allow"),
            ("\"a file.txt\"", "allow"),
            ("", "allow"),
        ] {
            assert_eq!(v(Sub::Add, line), want, "git add {line}");
        }
    }

    #[test]
    fn commit_cases() {
        for (line, want) in [
            ("-a -m wip", "ask"),
            ("-am wip", "ask"),
            ("-va", "ask"),
            ("-avm wip", "ask"),
            ("--all -m wip", "ask"),
            ("--al -m wip", "ask"),
            ("-a -m wip -- tracked.txt", "ask"),
            ("-m wip .", "ask"),
            ("-m wip -- .", "ask"),
            ("-m wip :/", "ask"),
            ("-m $MSG", "ask"),
            ("-m wip $F", "ask"),
            ("-m *", "ask"),
            ("--pathspec-from-file x", "ask"),
            ("-Q", "ask"),
            ("-m", "ask"),
            ("-m \"$(echo hi", "ask"),
            // Controls: `a` as a value, not the flag.
            ("-m msg", "allow"),
            ("-mabc", "allow"),
            ("-m -a", "allow"),
            ("-m 'use -a here'", "allow"),
            ("-F a.txt", "allow"),
            ("-Fa", "allow"),
            ("-C abc", "allow"),
            ("-Sabc -m x", "allow"),
            ("-uall -m x", "allow"),
            ("--message=-a", "allow"),
            ("--message -a", "allow"),
            ("--author a -m x", "allow"),
            ("-m msg -- tracked.txt", "allow"),
            // The here-document message as the Bash tool sends it (real
            // newlines) is Allow; the same text on ONE line has no terminator
            // line, so the here-document never ends: undeterminable, Ask.
            ("-m \"$(cat <<'EOF'\nfix: thing\nEOF\n)\"", "allow"),
            ("-m \"$(cat <<'EOF'\nfix: don't\nEOF\n)\"", "allow"),
            ("-m \"$(cat <<'EOF' fix: thing EOF )\"", "ask"),
            ("-m \"$(cat <<EOF\nfix: $(id)\nEOF\n)\"", "ask"),
            ("-m \"fix $X\"", "allow"),
            ("--amend --no-edit", "allow"),
            ("-m x --trailer \"Condukt-Task: r/t\"", "allow"),
        ] {
            assert_eq!(v(Sub::Commit, line), want, "git commit {line}");
        }
    }
}
