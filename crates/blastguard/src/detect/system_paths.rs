//! System-file mutators: the non-`rm` commands that remove, rename, rewrite or
//! re-permission a file named on their command line (backlog 3c750df2, round 2).
//!
//! The per-command arms in `analyze_command_at` (`mv`, `unlink`/`rmdir`,
//! `sed`, `chmod`, `chown`/`chgrp`) only ever asked whether a target was a
//! PROTECTED gate/config path, and their non-recursive branches answered
//! `Allow` for everything else — so `unlink /etc/passwd`, `mv /bin/ls /tmp/ls`,
//! `sed -i '' d /etc/passwd` and `chmod 000 /etc/passwd` were all Allow while
//! `rm /etc/passwd` was a system-harm Deny. `rsync` had no arm at all, and
//! `perl -i` went only through the interpreter path.
//!
//! [`system_mutation_verdict`] closes that one class in one place: given the
//! command word and its tokens, it lists the files the command would modify or
//! remove, and answers [`Decision::deny_system_harm`] when one of them lies
//! inside a system directory ([`crate::scope::is_inside_system_dir`], the same
//! predicate `rm` uses — `/var/folders` temp space and `/dev/null` are not
//! system directories). The files judged, per command:
//!
//! * `unlink` / `rmdir` — every operand;
//! * `mv` — every SOURCE (all operands but the last, or all of them with `-t`);
//! * `sed -i` / `--in-place` — the input files (the script operand is dropped
//!   unless `-e`/`-f` supplied the script);
//! * `perl -i` — the input files after the program (`-e`) or script file;
//! * `chmod` / `chown` / `chgrp` — the targets after the mode / owner spec;
//! * `rsync` with a `--del*` flag — the destination; with
//!   `--remove-source-files` — the sources.
//!
//! Operands are re-split quote-aware and unquoted the way the shell would
//! (`'…'`, `"…"`, `\x`). A leading `~` / `$HOME` / `${HOME}` is expanded with
//! the session's home. Any other `$`/backtick expansion in a judged operand
//! cannot be placed, and that is answered `Ask` (fail closed), never `Allow`.
//! A wildcard or brace operand is judged by its literal directory prefix.
//! A `{}` operand is a `find -exec` placeholder whose paths are `find`'s to
//! judge, and is skipped. A relative operand with no known base is not judged
//! here (the same as `rm`); every other rule still applies to it.
//!
//! A path inside a safe root escapes exactly as for `rm` (a project checked
//! out under `/opt`).
//!
//! This function only ever ADDS a Deny or an Ask; `Allow` leaves every
//! per-command rule as it was.

use super::vars::quote_aware_words;
use super::{
    chmod_mode_and_targets, chown_owner_and_targets, is_short_flag, positional_operands, Ctx,
    Decision,
};
use harness_core::verdict::Determination;

/// The system-harm verdict for `cmd rest…`: `Allow` when no file it would
/// modify or remove lies inside a system directory (the per-command rules then
/// decide), `Ask` when such a file is spelled with an expansion blastguard
/// cannot resolve.
pub(super) fn system_mutation_verdict(cmd: &str, rest: &[&str], ctx: &Ctx<'_>) -> Decision {
    system_mutation(cmd, rest, ctx).unwrap_or(Decision::Allow)
}

fn system_mutation(cmd: &str, rest: &[&str], ctx: &Ctx<'_>) -> Option<Decision> {
    if !matches!(
        cmd,
        "unlink" | "rmdir" | "mv" | "sed" | "perl" | "chmod" | "chown" | "chgrp" | "rsync"
    ) {
        return None;
    }
    let words = quote_aware_words(&rest.join(" "));
    let words: Vec<&str> = words.iter().map(String::as_str).collect();
    let (what, targets) = mutated_files(cmd, &words)?;
    let mut ask: Option<Decision> = None;
    for raw in targets {
        match judge(cmd, raw, ctx) {
            Judged::System(hit) => {
                let shown = if hit == raw {
                    format!("`{raw}`")
                } else {
                    format!("`{raw}` (which lands on `{hit}`)")
                };
                return Some(Decision::deny_system_harm(format!(
                    "`{cmd}` {what} {shown}, which is inside a system directory — removing, \
renaming, rewriting or re-permissioning an OS file or directory (a binary, a library, `/etc` \
configuration) can break the machine itself, so blastguard refuses it"
                )));
            }
            Judged::Unresolved if ask.is_none() => {
                ask = Some(Decision::ask(format!(
                    "`{cmd}` {what} `{raw}`, whose path depends on an expansion blastguard cannot \
resolve — it cannot tell whether that lands inside a system directory, so it refuses to guess"
                )));
            }
            Judged::Unresolved | Judged::Elsewhere => {}
        }
    }
    ask
}

/// What the command does to its targets (for the reason) and the raw operand
/// tokens it does it to. `None` when this invocation modifies no named file
/// (`sed` without `-i`, `rsync` without a delete flag, …).
fn mutated_files<'a>(cmd: &str, words: &[&'a str]) -> Option<(&'static str, Vec<&'a str>)> {
    match cmd {
        "unlink" => Some(("removes", positional_operands(words, &[]))),
        "rmdir" => Some(("removes the directory", positional_operands(words, &[]))),
        "mv" => {
            let ops = positional_operands(words, MV_VALUE_FLAGS);
            let has_target_dir = words.iter().any(|w| {
                *w == "-t"
                    || w.starts_with("--target-directory")
                    || (w.starts_with("-t") && !w.starts_with("--"))
            });
            let sources = if has_target_dir {
                ops
            } else {
                ops.split_last()
                    .map(|(_, init)| init.to_vec())
                    .unwrap_or_default()
            };
            Some(("moves away", sources))
        }
        "sed" => sed_in_place_files(words).map(|f| ("rewrites in place", f)),
        "perl" => perl_in_place_files(words).map(|f| ("rewrites in place", f)),
        "chmod" => Some(("re-permissions", chmod_mode_and_targets(words).1)),
        "chown" | "chgrp" => Some(("re-owns", chown_owner_and_targets(words).1)),
        "rsync" => rsync_mutated(words),
        _ => None,
    }
}

const MV_VALUE_FLAGS: &[&str] = &["-t", "--target-directory", "-S", "--suffix"];
const SED_VALUE_FLAGS: &[&str] = &["-e", "--expression", "-f", "--file", "-l", "--line-length"];
const RSYNC_VALUE_FLAGS: &[&str] = &[
    "-e",
    "--rsh",
    "-f",
    "--filter",
    "--exclude",
    "--include",
    "--exclude-from",
    "--include-from",
    "--files-from",
    "--password-file",
    "-T",
    "--temp-dir",
    "--backup-dir",
    "--compare-dest",
    "--copy-dest",
    "--link-dest",
    "--partial-dir",
    "--log-file",
    "--chmod",
    "--chown",
    "-B",
    "--block-size",
    "--max-size",
    "--min-size",
    "--timeout",
    "--port",
    "--suffix",
    "--rsync-path",
    "--out-format",
];

/// `sed -i` input files. The first positional is the SCRIPT unless a script
/// was given with `-e`/`-f`; an empty operand (BSD `-i ''`'s suffix) names no
/// file.
fn sed_in_place_files<'a>(words: &[&'a str]) -> Option<Vec<&'a str>> {
    let in_place = words.iter().any(|t| {
        *t == "--in-place" || t.starts_with("--in-place=") || (is_short_flag(t) && t.contains('i'))
    });
    if !in_place {
        return None;
    }
    let script_flag = words.iter().any(|t| {
        t.starts_with("-e")
            || t.starts_with("-f")
            || t.starts_with("--expression")
            || t.starts_with("--file")
    });
    let mut ops: Vec<&str> = positional_operands(words, SED_VALUE_FLAGS)
        .into_iter()
        .filter(|w| unquote(w).map(|u| !u.is_empty()).unwrap_or(true))
        .collect();
    if !script_flag && !ops.is_empty() {
        ops.remove(0);
    }
    Some(ops)
}

/// `perl -i` input files: the operands after the options, minus the script
/// file when no `-e`/`-E` program was given. `None` when `-i` is absent.
fn perl_in_place_files<'a>(words: &[&'a str]) -> Option<Vec<&'a str>> {
    let mut in_place = false;
    let mut inline_program = false;
    let mut i = 0;
    while i < words.len() {
        let w = words[i];
        if w == "--" {
            i += 1;
            break;
        }
        if !w.starts_with('-') || w == "-" {
            break;
        }
        // Walk the cluster: options that take the rest of the cluster as their
        // argument end it; `-e`/`-E` with nothing attached take the next word.
        let flags: Vec<char> = w.chars().skip(1).collect();
        let mut j = 0;
        while j < flags.len() {
            match flags[j] {
                'i' => {
                    in_place = true;
                    break;
                }
                'e' | 'E' => {
                    inline_program = true;
                    if j + 1 == flags.len() {
                        i += 1;
                    }
                    break;
                }
                'M' | 'm' | 'I' | 'x' | 'd' | 'D' | 'F' | 'C' => break,
                'l' | '0' => {
                    j += 1;
                    while j < flags.len() && flags[j].is_ascii_hexdigit() {
                        j += 1;
                    }
                }
                _ => j += 1,
            }
        }
        i += 1;
    }
    if !in_place {
        return None;
    }
    let mut ops: Vec<&str> = words.get(i..).unwrap_or(&[]).to_vec();
    if !inline_program && !ops.is_empty() {
        ops.remove(0);
    }
    Some(ops)
}

/// `rsync` with a delete flag empties the destination of whatever the source
/// lacks; `--remove-source-files` removes the sources.
fn rsync_mutated<'a>(words: &[&'a str]) -> Option<(&'static str, Vec<&'a str>)> {
    let ops = positional_operands(words, RSYNC_VALUE_FLAGS);
    let deletes = words.iter().any(|w| w.starts_with("--del"));
    let removes_sources = words.contains(&"--remove-source-files");
    let mut out = Vec::new();
    if let Some((dest, sources)) = ops.split_last() {
        if deletes {
            out.push(*dest);
        }
        if removes_sources {
            out.extend_from_slice(sources);
        }
    }
    if out.is_empty() {
        return None;
    }
    Some(("deletes files from", out))
}

enum Judged {
    /// Lies inside a system directory: the system path hit.
    System(String),
    /// Spelled with an expansion that cannot be resolved here.
    Unresolved,
    /// Not a system path, or not placeable for a reason other than expansion.
    Elsewhere,
}

fn judge(cmd: &str, raw: &str, ctx: &Ctx<'_>) -> Judged {
    if raw == "{}" {
        return Judged::Elsewhere;
    }
    // Remote rsync operands (`host:/path`, `rsync://…`) are not local files.
    if cmd == "rsync" && raw.split('/').next().is_some_and(|h| h.contains(':')) {
        return Judged::Elsewhere;
    }
    let Some(path) = expand_home(raw, ctx) else {
        return Judged::Unresolved;
    };
    let spelled = match literal_prefix(&path) {
        Some(p) => p,
        None => return Judged::Elsewhere,
    };
    let lexical = match ctx.scope.lexical_absolute(&spelled, ctx.base_for(cmd)) {
        Determination::Known(l) => l,
        Determination::Undetermined(_) => return Judged::Elsewhere,
    };
    // The final component is what the command acts on, so only the PARENT is
    // resolved through symlinks (`<proj>/esc/etc/passwd` with `esc -> /`).
    let parent_real = lexical.rsplit_once('/').and_then(|(parent, name)| {
        let parent = if parent.is_empty() { "/" } else { parent };
        ctx.scope.resolve(parent).map(|p| {
            let p = match p.strip_prefix("/System/Volumes/Data/") {
                Some(rest) => format!("/{rest}"),
                None => p,
            };
            if name.is_empty() {
                p
            } else {
                format!("{}/{name}", p.trim_end_matches('/'))
            }
        })
    });
    let is_system = |p: &str| p == "/" || crate::scope::is_inside_system_dir(p);
    let hit = if is_system(&lexical) {
        lexical.clone()
    } else {
        match parent_real.filter(|r| is_system(r)) {
            Some(r) => r,
            None => return Judged::Elsewhere,
        }
    };
    if ctx.confined_root(cmd, &[lexical.as_str()]).is_some() {
        return Judged::Elsewhere;
    }
    Judged::System(hit)
}

/// The operand unquoted, with a leading `~`, `$HOME` or `${HOME}` replaced by
/// the session's home. `None` when any other expansion remains, or a home
/// prefix is present and the home is unknown.
fn expand_home(raw: &str, ctx: &Ctx<'_>) -> Option<String> {
    for prefix in ["${HOME}", "$HOME", "\"${HOME}\"", "\"$HOME\"", "~"] {
        if let Some(rest) = raw.strip_prefix(prefix) {
            if rest.is_empty() || rest.starts_with('/') || rest.starts_with("\"/") {
                let home = ctx.scope.home_real()?.trim_end_matches('/').to_string();
                return unquote(rest).map(|r| format!("{home}{r}"));
            }
        }
    }
    unquote(raw)
}

/// One shell word unquoted (`'…'` literal, `"…"` with `\"`/`\\`/`\$`/`` \` ``
/// escapes, bare `\x`). `None` when a `$` or backtick expansion would run
/// (unquoted or inside double quotes, including `$'…'`).
fn unquote(word: &str) -> Option<String> {
    let mut out = String::new();
    let mut chars = word.chars();
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                for d in chars.by_ref() {
                    if d == '\'' {
                        break;
                    }
                    out.push(d);
                }
            }
            '"' => {
                while let Some(d) = chars.next() {
                    match d {
                        '"' => break,
                        '$' | '`' => return None,
                        '\\' => match chars.next() {
                            Some(e @ ('"' | '\\' | '$' | '`')) => out.push(e),
                            Some(e) => {
                                out.push('\\');
                                out.push(e);
                            }
                            None => out.push('\\'),
                        },
                        d => out.push(d),
                    }
                }
            }
            '\\' => {
                if let Some(d) = chars.next() {
                    out.push(d);
                }
            }
            '$' | '`' => return None,
            c => out.push(c),
        }
    }
    Some(out)
}

/// The path up to the first component holding a glob or brace pattern. A
/// relative pattern with no literal component is judged as its base (`.`); an
/// absolute one whose first component is a pattern is `/`.
fn literal_prefix(path: &str) -> Option<String> {
    let is_pattern = |c: &str| c.contains(['*', '?', '[', '{']);
    if !path.split('/').any(is_pattern) {
        return (!path.is_empty()).then(|| path.to_string());
    }
    let literal: Vec<&str> = path.split('/').take_while(|c| !is_pattern(c)).collect();
    let joined = literal.join("/");
    Some(if joined.is_empty() {
        if path.starts_with('/') {
            "/".to_string()
        } else {
            ".".to_string()
        }
    } else {
        joined
    })
}
