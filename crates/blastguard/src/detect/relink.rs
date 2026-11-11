//! Redirect targets RELINKED by another command of the same line.
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
//! When the landing cannot be determined — a non-literal source or
//! destination, an unknown cwd, `rsync`/`install -l`, a source that another
//! command of the line relinks too, two readings of one command, or an
//! unmodelled command that names a relinking verb together with a component
//! of the target or a non-literal word — the redirect is an `Ask`, never an
//! `Allow` (CLAUDE.md §3).
//!
//! Not modelled (residual, recorded in docs/specs/blastguard.md): a relink by
//! a command whose text names neither a relinking verb nor the target (an
//! archive extraction, a build script, `git checkout` of a tracked symlink).

use super::{
    compound_views, first_shell_word, has_glob_meta, has_unresolvable_expansion, is_assignment,
    normalized_command, positional_operands, protected_path_block_at,
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
    /// The landing could not be determined: record this `Ask`, move on.
    Ask(Decision),
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
    match relinked(line, seg_idx, seg_cwds, &target, ctx.raw_base.as_deref()) {
        Ok(None) => Step::Judge(target, physical),
        Ok(Some(real)) => match protected_path_block_at(ctx, action, &real, &real) {
            Some(d) => Step::Decided(d),
            None => Step::Judge(real.clone(), real),
        },
        Err(why) => Step::Ask(Decision::ask(format!(
            "'{op} {target}': {why}, so blastguard cannot tell which file the write really \
lands on"
        ))),
    }
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
    /// `rsync`, `install -l`: what it creates is not modelled.
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

/// `Ok(None)` when no command of `line` relinks `target`; `Ok(Some(real))`
/// with the physical landing when exactly one modelled command does;
/// `Err(why)` when that cannot be determined.
fn relinked(
    line: &str,
    seg_idx: Option<usize>,
    seg_cwds: &[CwdState],
    target: &str,
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
        if let Some(i) = seg_idx {
            // A later segment runs after the write unless a pipe or `&` makes
            // it concurrent, or a loop runs it before the next write.
            let concurrent = segs.get(i..k).is_some_and(|between| {
                between
                    .iter()
                    .any(|s| matches!(s.sep_after, SegmentSep::Pipe | SegmentSep::Background))
            });
            if k == i || (k > i && !has_loop && !concurrent) {
                continue;
            }
        }
        let command = views
            .get(k)
            .map_or(seg.text.as_str(), |v| v.command.as_str());
        let cwd = seg_cwds.get(k).cloned().unwrap_or(CwdState::Unknown);
        match parse(command, cwd) {
            Parsed::Inert => {}
            Parsed::Op(op) => ops.push(op),
            Parsed::Unmodelled => unmodelled.push(command.trim().to_string()),
        }
    }
    if ops.is_empty() && unmodelled.is_empty() {
        return Ok(None);
    }
    let t = absolute(target, &CwdState::Root, base).ok_or_else(|| {
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
    let words: Vec<&str> = command
        .split_whitespace()
        .map(|w| w.trim_start_matches('(').trim_end_matches(')'))
        .collect();
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
    let Some(head) = words.get(i).map(|w| normalized_command(w)) else {
        return Parsed::Inert;
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
    let is_ln = matches!(head.as_str(), "ln" | "gln" | "link");
    let kind = match head.as_str() {
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
    let target_dir = rest.iter().enumerate().find_map(|(j, w)| {
        if *w == "-t" || *w == "--target-directory" {
            rest.get(j + 1).map(|d| d.to_string())
        } else {
            w.strip_prefix("--target-directory=").map(str::to_string)
        }
    });
    let mut operands: Vec<String> = positional_operands(&rest, super::fileops::COPY_VALUE_FLAGS)
        .into_iter()
        .map(unquote)
        .collect();
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
