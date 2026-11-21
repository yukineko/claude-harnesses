//! Redirect targets — and the target operands of path-judged commands —
//! RELINKED by another command of the same line.
//!
//! [`operand_landing_verdict`] applies the same model to command operands
//! (`cp`/`mv`/`install`/`ln` destinations, `rm` targets, `tee`, `touch`, …):
//! `ln -sfn /etc d && rm -rf d/hosts` is judged as `rm -rf /private/etc/hosts`.
//!
//! Every redirect axis observes the target's placement (symlinks included)
//! when the hook runs. `ln -sf /etc/hosts a.rs && echo x > a.rs` then judged
//! `a.rs` — a regular in-tree file at hook time — although the kernel opens
//! `/etc/hosts`, because the `ln` runs first. The same holds for `ln -sfn /etc
//! d && echo x > d/hosts`, a hard link, an `mv` of a symlink into place, and a
//! `cp` that copies symlinks as symlinks (`-R`/`-r`/`-a`/`-P`/`-d`) or makes
//! them (`-s`).
//!
//! [`redirect_landing`] models those commands. When one of them — in a
//! segment that runs before the write, concurrently with it (`|`, `&`), or
//! anywhere on a line with a loop — creates or replaces the target or one of
//! its ancestors, the target is replaced by the path the write really lands
//! on (every symlink of the existing prefix resolved), and every redirect axis
//! judges THAT path, exactly as the direct spelling of the landing is judged.
//! A link that points inside the tree lands inside the tree and is judged
//! there, so ordinary in-tree links keep their verdict.
//!
//! A segment's commands are not only its own command word: the commands of
//! a `$( )` / backquote / `<( )` in its words (also in the write's own
//! segment: they run before its redirections are opened) and the body of an
//! escape it runs (`bash -c 'ln …'`, `vim -c '!ln …'`) are read by the same
//! model, each as a command of its own ([`nested`], backlog 00c6c2b8). When
//! the segment's own word scan reads it as an unmodelled relinker only
//! because of such words, that reading is dropped: the relinkers in them are
//! modelled. What [`nested`] cannot list or read is an unmodelled relinker.
//!
//! A non-literal word whose values the AST evaluator proved (a `for` variable
//! over a literal list, `ast::eval` unrolls it) is not a reason to give up:
//! each relinking command is judged once per value ([`expand`]), and an
//! operand with several values is judged per value and its landings must
//! agree ([`operand_landing`]). The strictest answer stands. The SOURCE of a
//! symlink (`ln -s`, `cp -s`) is never replaced by such a value: the
//! evaluator places a value against the cwd, while a symlink's text is
//! resolved from the directory holding the link, so a relative source would
//! be placed at the wrong path; it stays spelled, an `Ask`.
//!
//! An `ln`/`cp` with a long option this layer does not know by its full name
//! — including an abbreviation GNU getopt_long accepts (`ln --sym` is
//! `--symbolic`, `cp --sym` is `--symbolic-link`) — is [`Kind::Opaque`]: it
//! cannot be told whether it makes a symlink, so it is not read as a hard
//! link or a plain copy, and its sources are never replaced by a value.
//!
//! When the landing cannot be determined — a non-literal source or
//! destination with no proven values (`$(ls)`, a glob), an unknown cwd,
//! `rsync`/`install -l`, an `ln`/`cp` with an unknown long option, a source
//! that another
//! command of the line relinks too, two readings of one command, or an
//! unmodelled command that names a relinking verb together with a component
//! of the target or a non-literal word — the redirect is an `Ask`, never an
//! `Allow` (CLAUDE.md §3).
//!
//! Not modelled (residual, recorded in docs/specs/blastguard.md): a relink by
//! a command whose text names neither a relinking verb nor the target (an
//! archive extraction, a build script, `git checkout` of a tracked symlink).

mod nested;

use super::ast::targets::{self, Role};
use super::{
    analyze_segment, compound_views, first_shell_word, has_glob_meta, has_unresolvable_expansion,
    is_assignment, normalized_command, positional_operands, protected_path_block_at,
    split_segments_with_separators, Ctx, CwdState, SegmentSep,
};
use crate::exclude;
use crate::model::Decision;
use harness_core::verdict::Determination;

/// What [`redirect_landing`] tells the redirect loop to do with one target.
pub(super) enum Step {
    /// Judge this `(target, physical spelling)` pair with the usual axes.
    Judge(String, String),
    /// A verdict already reached on the real landing (a protected path).
    Decided(Decision),
    /// The landing could not be determined: record this `Ask`, then judge the
    /// spelled `(target, physical)` pair with the usual axes. The Ask does not
    /// stand in for them: a system-path Deny on the spelled target outranks it
    /// (`Deny > Ask > Allow`, memory `system-harm-always-deny`).
    Ask(Decision, String, String),
}

/// The redirect `target` (already placed by `place_redirect_target`, with
/// its `physical` spelling) of segment `seg_idx` of `line`, re-expressed as
/// the path it really lands on when another command of the line relinks it.
/// `op` is the operator as typed (`>` or `>>`).
pub(super) fn redirect_landing(
    ctx: &Ctx<'_>,
    op: &str,
    line: &str,
    seg_idx: Option<usize>,
    seg_cwds: &[CwdState],
    target: String,
    physical: String,
) -> Step {
    if matches!(
        exclude::normalize(&target).as_str(),
        "/dev/null" | "/dev/stdout" | "/dev/stderr"
    ) {
        return Step::Judge(target, physical);
    }
    let action = if op == ">>" {
        "append redirect"
    } else {
        "redirect"
    };
    let base = ctx.raw_base.as_deref();
    match relinked(line, seg_idx, seg_cwds, &target, &CwdState::Root, base) {
        Ok(None) => Step::Judge(target, physical),
        Ok(Some(real)) => match protected_path_block_at(ctx, action, &real, &real) {
            Some(d) => Step::Decided(d),
            None => Step::Judge(real.clone(), real),
        },
        Err(why) => Step::Ask(
            Decision::ask(format!(
                "'{op} {target}': {why}, so blastguard cannot tell which file the write really \
lands on"
            )),
            target,
            physical,
        ),
    }
}

/// How a path-judged command reaches one of its operands.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Reach {
    /// The file the path names: every symlink of it is followed (a `cp`
    /// destination, a `tee`/`touch`/`truncate`/`chmod` target).
    Follows,
    /// The directory entry itself, written: a symlink in the LAST component
    /// is replaced, not followed (a `-T` or `ln -n` destination).
    Entry,
    /// The directory entry itself, removed or renamed away (`rm`, `unlink`,
    /// `rmdir`, a `mv` source): resolved like [`Reach::Entry`], but it is not
    /// a write, so the system-directory WRITE axis is not applied to it.
    Removed,
}

/// Operands of a command blastguard judges by target path, as `(index into
/// rest, value prefix, reach)`; empty for every other command (reads included).
/// The prefix is the part of the word before the value (`-o` of `-oF`, `-st`
/// of `ln -stDIR`), kept when the value is rewritten.
fn path_operands<'a>(head: &str, rest: &[&'a str]) -> Vec<(usize, &'a str, Reach)> {
    let index = |w: &str| rest.iter().position(|r| std::ptr::eq(*r, w));
    let all = |reach| {
        positional_operands(rest, &[])
            .into_iter()
            .filter_map(|w| index(w).map(|j| (j, "", reach)))
            .collect::<Vec<_>>()
    };
    let short = |c: char| {
        rest.iter()
            .take_while(|w| **w != "--")
            .any(|w| !w.starts_with("--") && w.starts_with('-') && w.contains(c))
    };
    let long = |name: &str| rest.iter().take_while(|w| **w != "--").any(|w| *w == name);
    // `sort`'s output file (`-o F`, `-oF`, `--output=F`).
    let sort_output = || {
        rest.iter().enumerate().find_map(|(j, w)| {
            if *w == "-o" || *w == "--output" {
                (j + 1 < rest.len()).then_some((j + 1, ""))
            } else if w.len() > 2 && w.starts_with("-o") {
                Some((j, "-o"))
            } else {
                w.starts_with("--output=").then_some((j, "--output="))
            }
        })
    };
    match head {
        "rm" | "unlink" | "rmdir" => all(Reach::Removed),
        "tee" | "touch" | "truncate" | "shred" | "chmod" | "chown" | "chgrp" => all(Reach::Follows),
        // `sed -i`: the files after the script (the first operand, unless the
        // script comes from `-e`/`-f`).
        "sed" | "gsed" if short('i') || rest.iter().any(|w| w.starts_with("--in-place")) => {
            let ops = positional_operands(rest, &["-e", "-f", "--expression", "--file"]);
            let script_given = short('e') || short('f') || long("--expression") || long("--file");
            ops.into_iter()
                .skip(usize::from(!script_given))
                .filter_map(|w| index(w).map(|j| (j, "", Reach::Follows)))
                .collect()
        }
        "sort" => sort_output()
            .map(|(j, p)| (j, p, Reach::Follows))
            .into_iter()
            .collect(),
        // `uniq IN OUT`: the second operand is written.
        "uniq" => positional_operands(rest, &["-f", "-s", "-w"])
            .get(1)
            .and_then(|w| index(w))
            .map(|j| (j, "", Reach::Follows))
            .into_iter()
            .collect(),
        "cp" | "gcp" | "mv" | "gmv" | "install" | "ginstall" | "ln" | "gln" => {
            let is_ln = matches!(head, "ln" | "gln");
            let dest_reach = if short('T')
                || long("--no-target-directory")
                || (is_ln && (short('n') || short('h') || long("--no-dereference")))
            {
                Reach::Entry
            } else {
                Reach::Follows
            };
            // One getopt split for the operands AND the `-t` directory, so a
            // short cluster (`ln -st DIR`, `cp -at DIR`) is read as `-s -t DIR`
            // (backlog 42d5fa32: `-t` was matched only as a whole word).
            let split = super::fileops::copy_split(head, rest);
            let mut sources: Vec<usize> = split.operands.into_iter().filter_map(index).collect();
            let dest = match split.target_at {
                Some((j, off)) => rest
                    .get(j)
                    .and_then(|w| w.get(..off))
                    .map(|prefix| (j, prefix)),
                None => sources.pop().map(|j| (j, "")),
            };
            // A source is read (`cp`, `install`) or is link text (`ln`); only
            // a `mv` source is an entry the command changes.
            if !matches!(head, "mv" | "gmv") {
                sources.clear();
            }
            let mut ops: Vec<(usize, &'a str, Reach)> = sources
                .into_iter()
                .map(|j| (j, "", Reach::Removed))
                .collect();
            ops.extend(dest.map(|(j, prefix)| (j, prefix, dest_reach)));
            ops
        }
        _ => Vec::new(),
    }
}

/// The verdict for segment `seg_idx` of `line` (its simple command
/// `command`) on the paths its target operands REALLY reach when another
/// command of the line relinks them or a parent of them — the mirror of
/// [`redirect_landing`] for command operands (`cp`/`mv`/`install`/`ln`
/// destinations, `rm`/`unlink`/`rmdir` targets, `tee`, `touch`, `truncate`,
/// `shred`, `chmod`, `chown`, `sed`, `sort`, `uniq`).
///
/// A relinked WRITE operand (not a [`Reach::Removed`] one) is first judged on
/// its landing with the protected-path and system-directory axes, as a
/// redirect onto that landing is. Then each relinked operand is replaced by
/// its landing and the segment is judged
/// once more with every rule (`seg_ctx`), IN ADDITION to the judgement of the
/// text as written — so this can only add verdicts. An [`Reach::Entry`]
/// operand is resolved through its PARENT only (`rm d` after `ln -s /etc d`
/// removes the link). An operand whose landing cannot be determined is an
/// `Ask` (CLAUDE.md §3).
pub(super) fn operand_landing_verdict(
    command: &str,
    line: &str,
    seg_idx: usize,
    seg_cwds: &[CwdState],
    depth: usize,
    ctx: &Ctx<'_>,
    seg_ctx: &Ctx<'_>,
) -> Decision {
    // The caller decides, ONCE per line, whether this runs at all: on a line
    // for which [`line_names_relink_verb`] is false, `parse` finds no
    // relinking command, so the verdict is `Allow` without calling this.
    let words = plain_words(command);
    let start = command_start(&words);
    let Some(head) = words.get(start).map(|w| normalized_command(w)) else {
        return Decision::Allow;
    };
    let rest = &words[start + 1..];
    let base = ctx.raw_base.as_deref();
    let cwd = seg_cwds.get(seg_idx).cloned().unwrap_or(CwdState::Unknown);
    let mut out: Vec<String> = words.iter().map(|w| w.to_string()).collect();
    let mut changed = false;
    for (j, prefix, reach) in path_operands(&head, rest) {
        let word = unquote(&rest[j][prefix.len()..]);
        let ask = |why: &str| {
            Decision::ask(format!(
                "`{head}` operand `{word}`: {why}, so blastguard cannot tell which file the \
command really reaches"
            ))
        };
        let real = match operand_landing(&word, reach, line, seg_idx, seg_cwds, &cwd, base) {
            Ok(None) => continue,
            Ok(Some(real)) if !real.contains(char::is_whitespace) => real,
            Ok(Some(_)) => return ask("its landing cannot be re-spelled as one word"),
            Err(why) => return ask(&why),
        };
        // A WRITE is judged on the landing itself first, as a redirect onto it
        // is, so the verdict names the landing whatever the verb's own rule
        // checks (`sort -o`, `touch`, `truncate` have no system-path axis).
        if reach != Reach::Removed {
            let action = format!("{head} target");
            if let Some(d) = protected_path_block_at(ctx, &action, &real, &real) {
                return d;
            }
            match super::real_system_path_block(&action, &real, ctx) {
                Ok(Some(d)) => return d,
                Ok(None) => {}
                Err(why) => return ask(&why),
            }
        }
        out[start + 1 + j] = format!("{prefix}{real}");
        changed = true;
    }
    if changed {
        analyze_segment(&out.join(" "), depth, line, seg_idx, seg_ctx)
    } else {
        Decision::Allow
    }
}

/// Whether `line` names a relinking verb ([`is_relink_word`]) at all. When it
/// does not, no command of the line relinks anything and
/// [`operand_landing_verdict`] is `Allow` for every segment, so the caller
/// computes this once per line instead of once per segment — rescanning the
/// whole line per segment made long `;`-chains quadratic (backlog 3c1a3f35:
/// the 300-unit budget input spent ~80% of its time here).
pub(super) fn line_names_relink_verb(line: &str) -> bool {
    // Also a line whose substitutions / escapes may run a relinker:
    // nested.rs reads those commands (backlog 00c6c2b8).
    plain_words(line).into_iter().any(is_relink_word) || nested::may_run_nested(line)
}

/// The physical path `word` (an operand of segment `seg_idx`, run in `cwd`)
/// really reaches, when another command of `line` relinks it or a parent of
/// it and that differs from where it resolves now; see [`relinked`].
fn operand_landing(
    word: &str,
    reach: Reach,
    line: &str,
    seg_idx: usize,
    seg_cwds: &[CwdState],
    cwd: &CwdState,
    base: Option<&str>,
) -> Result<Option<String>, String> {
    // A word the AST evaluator proved takes several values (a `for` variable
    // over a literal list): each value is judged on its own, and the landings
    // must agree, or the operand is an Ask.
    if let Some(values) = proven_values(word, Role::Argv) {
        let mut landed: Option<Option<String>> = None;
        for value in &values {
            let real =
                operand_landing(value, reach, line, seg_idx, seg_cwds, &CwdState::Root, base)?;
            match &landed {
                None => landed = Some(real),
                Some(prev) if *prev == real => {}
                Some(_) => {
                    return Err(
                        "it lands on different paths for different values of a loop variable"
                            .into(),
                    )
                }
            }
        }
        return Ok(landed.flatten());
    }
    let at =
        |path: &str, from: &CwdState| relinked(line, Some(seg_idx), seg_cwds, path, from, base);
    let Some(t) = absolute(word, cwd, base) else {
        // Unplaceable: harmless unless some command of the line relinks.
        return at(word, cwd);
    };
    // A trailing `/` makes even `rm` follow a symlink in the last component.
    let (parent, name) = match t.rsplit_once('/') {
        Some((p, n)) if reach != Reach::Follows && !word.ends_with('/') && !n.is_empty() => {
            (if p.is_empty() { "/" } else { p }, n)
        }
        _ => {
            return Ok(
                at(&t, &CwdState::Root)?.filter(|real| physical(&t).ok().as_ref() != Some(real))
            )
        }
    };
    let join = |p: String| format!("{}/{name}", p.trim_end_matches('/'));
    Ok(at(parent, &CwdState::Root)?
        .map(join)
        .filter(|real| physical(parent).ok().map(join).as_ref() != Some(real)))
}

/// How a modelled command makes its destination name another file.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// `ln -s`, `cp -s`: the entry becomes a symlink to the source TEXT.
    Symbolic,
    /// `ln`, `link`: the entry becomes the source file itself.
    Hard,
    /// `mv`: the entry becomes what the source was.
    Moved,
    /// A link-preserving `cp`: symlinks in the copied part stay symlinks.
    CopiedLinks,
    /// `rsync`, `install -l`, an `ln`/`cp` with a long option outside
    /// [`LN_LONG`]/[`CP_LONG`]: what it creates is not modelled.
    Opaque,
}

/// One relinking command: sources and destination as typed, plus the cwd
/// its segment runs in.
struct LinkOp {
    kind: Kind,
    sources: Vec<String>,
    dest: String,
    /// `-t DIR`, several sources, a trailing `/`, or `ln -s SRC` alone:
    /// entries go INTO `dest`.
    into_dir: bool,
    /// `ln -n`/`-h`, `-T`: a symlink at `dest` is replaced, not followed.
    no_deref: bool,
    cwd: CwdState,
}

enum Parsed {
    /// Creates no entry that could name another file.
    Inert,
    Op(LinkOp),
    /// Not a modelled command, but it names a relinking verb ([`mentions`]).
    Unmodelled,
}

/// `Ok(None)` when no command of `line` relinks `target` (placed from
/// `target_cwd`); `Ok(Some(real))` with the physical landing when exactly one
/// modelled command does; `Err(why)` when that cannot be determined.
fn relinked(
    line: &str,
    seg_idx: Option<usize>,
    seg_cwds: &[CwdState],
    target: &str,
    target_cwd: &CwdState,
    base: Option<&str>,
) -> Result<Option<String>, String> {
    let segs = split_segments_with_separators(line);
    let views = compound_views(line);
    let has_loop = segs
        .iter()
        .any(|s| s.text.split_whitespace().any(|w| w == "done"));
    let mut ops: Vec<LinkOp> = Vec::new();
    let mut unmodelled: Vec<String> = Vec::new();
    for (k, seg) in segs.iter().enumerate() {
        // The write's own segment: its own command runs after its
        // redirections are opened, but the substitutions in its words run
        // before (backlog 00c6c2b8).
        let own = seg_idx == Some(k);
        if let Some(i) = seg_idx {
            // A later segment runs after the write unless a pipe or `&` makes
            // it concurrent, or a loop runs it before the next write.
            let concurrent = segs.get(i..k).is_some_and(|between| {
                between
                    .iter()
                    .any(|s| matches!(s.sep_after, SegmentSep::Pipe | SegmentSep::Background))
            });
            if k > i && !has_loop && !concurrent {
                continue;
            }
        }
        let command = views
            .get(k)
            .map_or(seg.text.as_str(), |v| v.command.as_str());
        let cwd = seg_cwds.get(k).cloned().unwrap_or(CwdState::Unknown);
        // Relinkers in substitutions and escape bodies (nested.rs).
        let found = nested::collect(command, &cwd, !own);
        let inert = |text: &str, cwd: &CwdState| matches!(parse(text, cwd.clone()), Parsed::Inert);
        if !own {
            match parse(command, cwd.clone()) {
                Parsed::Inert => {}
                Parsed::Op(op) => ops.push(op),
                // Read as a relinker only for words whose commands `found`
                // lists: those are modelled there instead.
                Parsed::Unmodelled
                    if found
                        .outer_reduced
                        .as_deref()
                        .is_some_and(|r| inert(r, &cwd)) => {}
                Parsed::Unmodelled => unmodelled.push(command.trim().to_string()),
            }
        }
        for c in &found.commands {
            match parse(&c.text, c.cwd.clone()) {
                Parsed::Inert => {}
                Parsed::Op(op) => ops.push(op),
                Parsed::Unmodelled if inert(&c.reduced, &c.cwd) => {}
                Parsed::Unmodelled => unmodelled.push(c.text.clone()),
            }
        }
        if found.undetermined {
            unmodelled.push(command.trim().to_string());
        }
    }
    if ops.is_empty() && unmodelled.is_empty() {
        return Ok(None);
    }
    // A relinking command whose words the AST evaluator resolved (a `for`
    // variable) is judged once per value.
    let ops: Vec<LinkOp> = ops.into_iter().flat_map(expand).collect();
    let t = absolute(target, target_cwd, base).ok_or_else(|| {
        "a command that may relink it runs first and the path cannot be placed".to_string()
    })?;
    if let Some(cmd) = unmodelled.iter().find(|c| mentions(c, &t, base)) {
        return Err(format!(
            "`{cmd}` may create or rename an entry on this path and is not modelled"
        ));
    }
    let mut landing: Option<String> = None;
    for (n, op) in ops.iter().enumerate() {
        let Some(spelling) = hit(op, &t, base)? else {
            continue;
        };
        if landing.is_some() {
            return Err("more than one command of the line relinks this path".into());
        }
        // A source another command of the line creates or replaces is a
        // chain this does not model.
        if ops
            .iter()
            .enumerate()
            .any(|(m, o)| m != n && covers(o, &spelling, base).unwrap_or(true))
        {
            return Err("its new source is itself relinked on this line".into());
        }
        let real = physical(&spelling)?;
        landing = Some(
            if op.kind == Kind::CopiedLinks && copies_plainly(&spelling, &real) {
                // No symlink in the copied part: the copy holds plain files and
                // the write lands on the copy, i.e. the target itself.
                physical(&t)?
            } else {
                real
            },
        );
    }
    Ok(landing)
}

/// `physical_landing` of an absolute spelling, or why it has none.
fn physical(spelling: &str) -> Result<String, String> {
    match super::protected::physical_landing(spelling, None) {
        Some(Determination::Known(real)) => Ok(real),
        Some(Determination::Undetermined(u)) => Err(u.to_string()),
        None => Err(format!("the relinked path `{spelling}` cannot be placed")),
    }
}

/// True when resolving `spelling` follows no symlink below its parent
/// directory: the parent's physical path plus the rest as written is `real`.
fn copies_plainly(spelling: &str, real: &str) -> bool {
    spelling.rsplit_once('/').is_some_and(|(parent, name)| {
        physical(if parent.is_empty() { "/" } else { parent })
            .is_ok_and(|p| format!("{}/{name}", p.trim_end_matches('/')) == real)
    })
}

/// Parse one segment's simple command. Leading assignments and the plain
/// exec wrappers are skipped; any other command that names a relinking verb
/// is [`Parsed::Unmodelled`].
fn parse(command: &str, cwd: CwdState) -> Parsed {
    let words = plain_words(command);
    let i = command_start(&words);
    let Some(head) = words.get(i).map(|w| normalized_command(w)) else {
        return Parsed::Inert;
    };
    // `git mv SRC… DEST` (no git option before `mv`) renames entries in the
    // work tree exactly as `mv` does, so it is modelled as `mv` — not left as
    // an unmodelled relink verb that asks about every path sharing a
    // component with it.
    let (head, i) = if head == "git" && words.get(i + 1) == Some(&"mv") {
        ("mv".to_string(), i + 1)
    } else {
        (head, i)
    };
    let rest: Vec<&str> = words[i + 1..].to_vec();
    let opts: Vec<&str> = rest
        .iter()
        .copied()
        .take_while(|w| *w != "--")
        .filter(|w| w.starts_with('-') && w.len() > 1)
        .collect();
    let short = |letters: &str| {
        opts.iter()
            .any(|o| !o.starts_with("--") && o[1..].chars().any(|c| letters.contains(c)))
    };
    let long = |names: &[&str]| opts.iter().any(|o| names.iter().any(|n| o.starts_with(n)));
    // A long option not known by its full name (an abbreviation such as
    // `--sym`, or an option this layer does not model) cannot be interpreted
    // with certainty: getopt_long may read it as `--symbolic`.
    let unknown_long = |known: &[&str]| {
        opts.iter().any(|o| {
            o.starts_with("--") && !known.contains(&o.split_once('=').map_or(*o, |(name, _)| name))
        })
    };
    let is_ln = matches!(head.as_str(), "ln" | "gln" | "link");
    let kind = match head.as_str() {
        "ln" | "gln" if unknown_long(LN_LONG) => Kind::Opaque,
        "cp" | "gcp" if unknown_long(CP_LONG) => Kind::Opaque,
        "ln" | "gln" if short("s") || long(&["--symbolic"]) => Kind::Symbolic,
        "ln" | "gln" | "link" => Kind::Hard,
        "cp" | "gcp" if short("s") || long(&["--symbolic-link"]) => Kind::Symbolic,
        "cp" | "gcp"
            if short("RraPd")
                || long(&[
                    "--recursive",
                    "--archive",
                    "--no-dereference",
                    "--preserve=links",
                ]) =>
        {
            Kind::CopiedLinks
        }
        // A plain `cp` (and `install`) writes regular files and never
        // creates a symlink.
        "cp" | "gcp" => return Parsed::Inert,
        "mv" | "gmv" => Kind::Moved,
        "install" | "ginstall" if short("l") => Kind::Opaque,
        "install" | "ginstall" => return Parsed::Inert,
        "rsync" => Kind::Opaque,
        _ if words.iter().any(|w| is_relink_word(w)) => return Parsed::Unmodelled,
        _ => return Parsed::Inert,
    };
    // `cp`/`mv`/`install`/`ln`: the shared getopt split, so `-t` inside a
    // short cluster (`ln -st DIR SRC`, `ln -sft DIR`, `cp -at DIR`) is the
    // target directory exactly as a separate `-t DIR` is (backlog 42d5fa32).
    // `link` / `rsync` keep the word heuristic: `rsync -t` is a boolean.
    let (target_dir, operands): (Option<String>, Vec<&str>) = match head.as_str() {
        "cp" | "gcp" | "mv" | "gmv" | "install" | "ginstall" | "ln" | "gln" => {
            let split = super::fileops::copy_split(&head, &rest);
            (split.target_dir, split.operands)
        }
        _ => (
            rest.iter().enumerate().find_map(|(j, w)| {
                if *w == "-t" || *w == "--target-directory" {
                    rest.get(j + 1).map(|d| d.to_string())
                } else {
                    w.strip_prefix("--target-directory=").map(str::to_string)
                }
            }),
            positional_operands(&rest, super::fileops::COPY_VALUE_FLAGS),
        ),
    };
    let mut operands: Vec<String> = operands.into_iter().map(unquote).collect();
    let (dest, into_dir) = match target_dir {
        Some(d) => (unquote(&d), true),
        // `ln -s SRC` alone links into the cwd.
        None if is_ln && operands.len() == 1 => (".".to_string(), true),
        None => match operands.pop() {
            Some(d) => {
                let into = operands.len() > 1 || d.ends_with('/');
                (d, into)
            }
            None => return Parsed::Inert,
        },
    };
    Parsed::Op(LinkOp {
        kind,
        sources: operands,
        dest,
        into_dir,
        no_deref: (is_ln && short("nh"))
            || short("T")
            || long(&["--no-dereference", "--no-target-directory"]),
        cwd,
    })
}

/// The long options of `ln` this layer reads by their FULL name; any other
/// `--` option (an abbreviation included) makes the command [`Kind::Opaque`].
const LN_LONG: &[&str] = &[
    "--backup",
    "--force",
    "--interactive",
    "--logical",
    "--no-dereference",
    "--no-target-directory",
    "--physical",
    "--suffix",
    "--symbolic",
    "--target-directory",
    "--verbose",
];

/// As [`LN_LONG`], for `cp`.
const CP_LONG: &[&str] = &[
    "--archive",
    "--backup",
    "--force",
    "--interactive",
    "--no-clobber",
    "--no-dereference",
    "--no-target-directory",
    "--preserve",
    "--recursive",
    "--remove-destination",
    "--suffix",
    "--symbolic-link",
    "--target-directory",
    "--update",
    "--verbose",
];

/// The whitespace words of `command`, subshell parentheses stripped.
fn plain_words(command: &str) -> Vec<&str> {
    command
        .split_whitespace()
        .map(|w| w.trim_start_matches('(').trim_end_matches(')'))
        .collect()
}

/// The index of the command word: leading assignments and the plain exec
/// wrappers (with their options) skipped.
fn command_start(words: &[&str]) -> usize {
    let mut i = 0;
    while let Some(w) = words.get(i) {
        if is_assignment(w) {
            i += 1;
        } else if matches!(
            normalized_command(w).as_str(),
            "sudo" | "doas" | "env" | "command" | "nohup" | "time" | "nice" | "exec" | "builtin"
        ) {
            i += 1;
            while words
                .get(i)
                .is_some_and(|w| w.starts_with('-') || is_assignment(w))
            {
                i += 1;
            }
        } else {
            break;
        }
    }
    i
}

/// A word naming a command (or a library call) able to create or rename a
/// directory entry.
fn is_relink_word(word: &str) -> bool {
    let w = word.trim_matches(|c: char| "'\";`".contains(c));
    let lower = w.to_ascii_lowercase();
    matches!(
        normalized_command(w).as_str(),
        "ln" | "gln" | "link" | "mv" | "gmv" | "cp" | "gcp" | "rsync" | "install" | "ditto"
    ) || ["symlink", "os.link", "rename", "os.replace", "hardlink"]
        .iter()
        .any(|k| lower.contains(k))
}

/// An unmodelled command that names a relinking verb may relink `t` when it
/// also holds a non-literal word or names a component of `t` below `base`.
fn mentions(command: &str, t: &str, base: Option<&str>) -> bool {
    if has_unresolvable_expansion(command) || has_glob_meta(command) {
        return true;
    }
    let below_base = base
        .and_then(|b| t.strip_prefix(b.trim_end_matches('/')))
        .unwrap_or(t);
    let wanted: Vec<&str> = below_base.split('/').filter(|c| !c.is_empty()).collect();
    command
        .split(|c: char| c.is_whitespace() || "/'\"();=,".contains(c))
        .any(|c| !c.is_empty() && wanted.contains(&c))
}

fn unquote(word: &str) -> String {
    if word.contains(['\'', '"', '\\']) {
        first_shell_word(word).unwrap_or_else(|| word.to_string())
    } else {
        word.to_string()
    }
}

fn literal(word: &str) -> bool {
    !word.starts_with('~') && !has_unresolvable_expansion(word) && !has_glob_meta(word)
}

/// Most `LinkOp`s one relinking command expands into; beyond it the command
/// keeps its spelled words, which are an `Ask`.
const MAX_EXPANSIONS: usize = 256;

/// The absolute literal paths the AST evaluator proved `word` (read in `role`)
/// takes: a non-literal word such as `$D/$f.tmp` after `for f in a b`, unrolled
/// by `ast::eval`. `None` when the word is a literal (nothing to expand) or has
/// no proven values (`$(ls)`, a glob, an undetermined variable) — the caller
/// keeps its `Ask`. Every value must be absolute and literal: the evaluator
/// has already placed it against the cwd of the command it belongs to
/// (`ast::eval::resolve_at`), so it is only right where a word is read from
/// the cwd — never for a symlink's source text ([`expand`]).
fn proven_values(word: &str, role: Role) -> Option<Vec<String>> {
    if literal(word) {
        return None;
    }
    let values = targets::resolve(word, role)?;
    values
        .iter()
        .all(|v| v.starts_with('/') && literal(v))
        .then_some(values)
}

/// `op` with every destination and source word whose values the AST
/// evaluator proved replaced by each value: one `LinkOp` per combination, so
/// each value of a loop variable is judged as its own command. A word with no
/// proven values stays spelled (its landing stays undetermined, an `Ask`).
/// The sources of a [`Kind::Symbolic`] op are replaced only by values proven
/// absolute AS WRITTEN ([`Role::ArgvAsWritten`]): the link holds the source
/// TEXT, resolved from the link's own directory ([`hit`]), so a cwd-placed
/// value would misread a relative one (`ln -s $f /private/L` with `f=etc`
/// would be read as `<cwd>/etc` instead of `/private/etc`). A word any of
/// whose values is relative or unproven stays spelled, an `Ask` (backlog
/// 42d5fa32: `for f in /etc; do ln -st D $f; done` is the link `D/etc ->
/// /etc`). The sources of a [`Kind::Opaque`] op (an `ln --sym` whose kind is
/// not known) are never replaced. An op with nothing to expand, or more than
/// [`MAX_EXPANSIONS`] combinations, is returned unchanged.
fn expand(op: LinkOp) -> Vec<LinkOp> {
    let dest_values = proven_values(&op.dest, Role::Argv);
    let source_values: Vec<Option<Vec<String>>> = op
        .sources
        .iter()
        .map(|s| match op.kind {
            // The link holds the source TEXT: only values the evaluator
            // proved are absolute AS WRITTEN (before any cwd placement) are
            // that text; [`proven_values`] refuses the whole word when any
            // value is relative, so it stays spelled (an `Ask`).
            Kind::Symbolic => proven_values(s, Role::ArgvAsWritten),
            Kind::Opaque => None,
            _ => proven_values(s, Role::Argv),
        })
        .collect();
    if dest_values.is_none() && source_values.iter().all(Option::is_none) {
        return vec![op];
    }
    let alternatives = |spelled: &str, values: &Option<Vec<String>>| {
        values.clone().unwrap_or_else(|| vec![spelled.to_string()])
    };
    let mut source_combos: Vec<Vec<String>> = vec![Vec::new()];
    for (spelled, values) in op.sources.iter().zip(&source_values) {
        let alts = alternatives(spelled, values);
        source_combos = source_combos
            .iter()
            .flat_map(|combo| {
                alts.iter().map(move |alt| {
                    let mut next = combo.clone();
                    next.push(alt.clone());
                    next
                })
            })
            .collect();
        if source_combos.len() > MAX_EXPANSIONS {
            return vec![op];
        }
    }
    let dests = alternatives(&op.dest, &dest_values);
    if dests.len() * source_combos.len() > MAX_EXPANSIONS {
        return vec![op];
    }
    let mut out = Vec::new();
    for dest in &dests {
        for sources in &source_combos {
            out.push(LinkOp {
                kind: op.kind,
                sources: sources.clone(),
                dest: dest.clone(),
                into_dir: op.into_dir,
                no_deref: op.no_deref,
                cwd: op.cwd.clone(),
            });
        }
    }
    out
}

/// `word` as an absolute, lexically normalised path for a segment run in
/// `cwd` of a line whose own base is `base`; `None` when it cannot be placed.
fn absolute(word: &str, cwd: &CwdState, base: Option<&str>) -> Option<String> {
    if !literal(word) {
        return None;
    }
    if word.starts_with('/') {
        return Some(exclude::normalize(word));
    }
    let base = base.filter(|b| b.starts_with('/'))?.trim_end_matches('/');
    let dir = match cwd {
        CwdState::Root => base.to_string(),
        CwdState::Known(d) if d.starts_with('/') => d.clone(),
        CwdState::Known(d) => format!("{base}/{d}"),
        CwdState::Unknown => return None,
    };
    Some(exclude::normalize(&format!(
        "{}/{word}",
        dir.trim_end_matches('/')
    )))
}

/// The rest of `path` below `entry` (`""` when equal), or `None`.
fn below<'p>(path: &'p str, entry: &str) -> Option<&'p str> {
    match path.strip_prefix(entry.trim_end_matches('/')) {
        Some(r) if r.is_empty() || r.starts_with('/') => Some(r),
        _ => None,
    }
}

/// An entry a [`LinkOp`] may create or replace: the absolute path, the
/// source it then stands for, and whether its last component is unknown (a
/// non-literal source name: any child of `path` may be the entry).
struct Entry {
    path: String,
    source: String,
    any_child: bool,
}

/// The entries `op` may create or replace; `None` when its destination
/// cannot be placed.
fn entries(op: &LinkOp, base: Option<&str>) -> Option<Vec<Entry>> {
    let dest = absolute(&op.dest, &op.cwd, base)?;
    let link = std::fs::symlink_metadata(&dest);
    let absent = link.is_err();
    let is_link = link.is_ok_and(|m| m.file_type().is_symlink());
    let existing_dir =
        std::fs::metadata(&dest).is_ok_and(|m| m.is_dir()) && !(is_link && op.no_deref);
    // Into the directory, onto the name itself, or — for a destination that
    // does not exist yet and may be created as a directory first — both.
    let (into, onto) = if op.into_dir || existing_dir {
        (true, false)
    } else {
        (absent, true)
    };
    let mut out = Vec::new();
    for src in &op.sources {
        let name = src.trim_end_matches('/').rsplit('/').next().unwrap_or(src);
        if into {
            out.push(if literal(name) {
                Entry {
                    path: format!("{}/{name}", dest.trim_end_matches('/')),
                    source: src.clone(),
                    any_child: false,
                }
            } else {
                Entry {
                    path: dest.clone(),
                    source: src.clone(),
                    any_child: true,
                }
            });
        }
        if onto {
            out.push(Entry {
                path: dest.clone(),
                source: src.clone(),
                any_child: false,
            });
        }
    }
    Some(out)
}

/// Whether `op` may create or replace `path` or one of its ancestors;
/// `None` when that cannot be determined.
fn covers(op: &LinkOp, path: &str, base: Option<&str>) -> Option<bool> {
    if let Some(es) = entries(op, base) {
        return Some(es.iter().any(|e| match below(path, &e.path) {
            Some(rest) => !e.any_child || !rest.is_empty(),
            None => false,
        }));
    }
    // Only the literal directory part of the destination is known.
    let lit: String = op
        .dest
        .chars()
        .take_while(|c| !matches!(c, '$' | '`' | '*' | '?' | '[' | '~'))
        .collect();
    if op.dest.starts_with('~') {
        return None;
    }
    let dir = match lit.rsplit_once('/') {
        Some(("", _)) => "/",
        Some((d, _)) => d,
        None => ".",
    };
    match absolute(dir, &op.cwd, base) {
        Some(d) if below(path, &d).is_none() => Some(false),
        _ => None,
    }
}

/// When `op` creates or replaces `t` or an ancestor of it: the spelling the
/// write is then really opened through. `Err` when that cannot be
/// determined.
fn hit(op: &LinkOp, t: &str, base: Option<&str>) -> Result<Option<String>, String> {
    let Some(es) = entries(op, base) else {
        return match covers(op, t, base) {
            Some(false) => Ok(None),
            _ => Err(format!(
                "the destination `{}` of a relinking command cannot be placed",
                op.dest
            )),
        };
    };
    let mut found: Option<String> = None;
    for e in &es {
        let Some(rest) = below(t, &e.path) else {
            continue;
        };
        if op.kind == Kind::Opaque || (e.any_child && !rest.is_empty()) {
            return Err(format!(
                "a command whose effect is not modelled may relink it (into `{}`)",
                op.dest
            ));
        }
        if e.any_child {
            continue;
        }
        let src = &e.source;
        let src_abs = match op.kind {
            // A relative symlink is resolved from the directory holding it.
            Kind::Symbolic if !src.starts_with('/') && literal(src) => {
                let parent = e.path.rsplit_once('/').map_or("", |(p, _)| p);
                Some(format!("{parent}/{src}"))
            }
            _ => absolute(src, &op.cwd, base),
        };
        let Some(src_abs) = src_abs else {
            return Err(format!(
                "the source `{src}` of a command that relinks it cannot be placed"
            ));
        };
        let spelling = format!("{}{rest}", src_abs.trim_end_matches('/'));
        if found.as_ref().is_some_and(|f| *f != spelling) {
            return Err("a command that relinks it has more than one reading".into());
        }
        found = Some(spelling);
    }
    Ok(found)
}
