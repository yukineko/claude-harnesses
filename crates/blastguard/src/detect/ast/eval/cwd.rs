//! The statically tracked cwd of `detect::ast::eval` ([`Cwd`]) and how a
//! relative path is placed against it ([`resolve`]). A child module of
//! `eval.rs` (split out for the file-growth cap when the finite candidate
//! set of user ruling 2026-11-18 was added).
//!
//! After a `cd` that may have failed (`cd x; cmd`, `cd x || true; cmd`) the
//! paths that reach `cmd` have different cwds. When every one of them is
//! known (the hook's cwd or a literal directory) and there are at most
//! [`MAX_CWD_CANDIDATES`], the joined cwd is [`Cwd::OneOf`] that set; the
//! judge reads it through `SimpleCommand::cwd_candidates` and judges every
//! candidate (most restrictive wins). To every reader that takes ONE cwd
//! ([`Cwd::value`], [`resolve`]) a `OneOf` is `Undetermined`, as before.
//!
//! User ruling 2026-11-19: (1) the evaluator holds the hook's cwd (the
//! payload `cwd`, set per detection by [`with_hook_dir`]) as a known value,
//! so a relative `cd` (`cd .githooks`) from the hook's cwd is resolved; (2) a
//! relative path with a `..` component is resolved against the REAL path
//! ([`real_join`]): the existing directory the `..` steps out of is
//! canonicalized before the join. A target that does not exist, a walk that
//! passes through a symlink, or a canonicalize that fails stays
//! `Undetermined` (Ask), as before.

use super::super::support::join_path;
use super::super::{CwdCandidates, Value};
use super::HOOK_CWD;
use std::cell::RefCell;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

thread_local! {
    /// The hook's cwd for the detection running on this thread (user ruling
    /// 2026-11-19 ①). `None` when the caller has no payload cwd.
    static HOOK_DIR: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// Runs `f` with `dir` as the hook's cwd the evaluator resolves a relative
/// `cd` against. Only an absolute `dir` is held; anything else is no hook cwd
/// (a relative `cd` from it stays `Undetermined`). The previous value is
/// restored when `f` returns or unwinds.
pub(crate) fn with_hook_dir<R>(dir: Option<&str>, f: impl FnOnce() -> R) -> R {
    struct Restore(Option<String>);
    impl Drop for Restore {
        fn drop(&mut self) {
            let prev = self.0.take();
            HOOK_DIR.with(|h| *h.borrow_mut() = prev);
        }
    }
    let dir = dir.filter(|d| d.starts_with('/')).map(str::to_string);
    let _restore = Restore(HOOK_DIR.with(|h| h.replace(dir)));
    f()
}

/// The hook's cwd set by [`with_hook_dir`], if any.
pub(super) fn hook_dir() -> Option<String> {
    HOOK_DIR.with(|h| h.borrow().clone())
}

/// The cwd after `cd {shown}` from the directory `base`, `rel` being the
/// operand relative to `base` ([`real_join`]).
pub(super) fn cd_join(base: &str, rel: &str, shown: &str) -> Cwd {
    match real_join(base, rel) {
        Ok(j) => Cwd::Known(j),
        Err(why) => Cwd::Undetermined(format!("cwd: `cd {shown}` not resolved: {why}")),
    }
}

/// `rel` joined to the directory `base` (user ruling 2026-11-19 ②).
///
/// Without a `..` component the join is lexical ([`join_path`]). With one,
/// `..` is the REAL parent: `base` must already be its own canonical path
/// (else the logical cwd a shell `cd` keeps and the physical one the kernel
/// resolves `..` against differ — a symlink), and every component up to the
/// last `..` must exist and not be a symlink; the components after it are
/// joined lexically. Every failure is the reason the path is undetermined.
pub(super) fn real_join(base: &str, rel: &str) -> Result<String, String> {
    let parts: Vec<&str> = rel.split('/').collect();
    let Some(last_up) = parts.iter().rposition(|p| *p == "..") else {
        return join_path(base, rel).ok_or_else(|| format!("`{rel}` is an empty path"));
    };
    let canon = std::fs::canonicalize(base)
        .map_err(|e| format!("`{rel}` after cd {base}: canonicalize {base} failed ({e})"))?;
    if canon != Path::new(base).components().collect::<PathBuf>() {
        return Err(format!(
            "`{rel}` after cd {base}: {base} passes through a symlink (real path {})",
            canon.display()
        ));
    }
    let mut walk = canon;
    for part in &parts[..=last_up] {
        match *part {
            "" | "." => {}
            ".." => {
                walk.pop();
            }
            p => {
                walk.push(p);
                match std::fs::symlink_metadata(&walk) {
                    Ok(m) if m.file_type().is_symlink() => {
                        return Err(format!(
                            "`{rel}` after cd {base}: {} is a symlink",
                            walk.display()
                        ))
                    }
                    Ok(m) if m.is_dir() => {}
                    Ok(_) => {
                        return Err(format!(
                            "`{rel}` after cd {base}: {} is not a directory",
                            walk.display()
                        ))
                    }
                    Err(e) => {
                        return Err(format!(
                            "`{rel}` after cd {base}: {} does not exist ({e})",
                            walk.display()
                        ))
                    }
                }
            }
        }
    }
    let Some(dir) = walk.to_str() else {
        return Err(format!(
            "`{rel}` after cd {base}: the real path is not UTF-8"
        ));
    };
    let rest = parts[last_up + 1..].join("/");
    if rest.is_empty() {
        return Ok(dir.to_string());
    }
    join_path(dir, &rest).ok_or_else(|| format!("`{rel}` after cd {base}: empty path"))
}

/// Most directories a joined cwd may be one of (user ruling 2026-11-18)
/// before it is `Undetermined`.
const MAX_CWD_CANDIDATES: usize = 4;

/// One directory a [`Cwd::OneOf`] may be.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum CwdCand {
    /// The hook's cwd (no `cd` took effect on this path).
    Hook,
    Known(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Cwd {
    /// No `cd` applies: the hook's cwd.
    Hook,
    Known(String),
    /// Two to [`MAX_CWD_CANDIDATES`] directories reach here — a `cd` before
    /// this point may have failed — and every one of them is known (user
    /// ruling 2026-11-18). To every reader that takes one cwd it is
    /// `Undetermined` ([`Cwd::value`], [`resolve`]); the judge alone reads the
    /// set and judges each candidate, keeping the most restrictive verdict.
    OneOf(BTreeSet<CwdCand>),
    Undetermined(String),
}

impl Cwd {
    pub(super) fn value(&self) -> Value {
        match self {
            Cwd::Hook => Value::Undetermined(HOOK_CWD.to_string()),
            Cwd::Known(c) => Value::Literal(c.clone()),
            Cwd::OneOf(set) => Value::Undetermined(one_of_reason(set)),
            Cwd::Undetermined(r) => Value::Undetermined(r.clone()),
        }
    }

    /// The candidate directories: one for `Hook` / `Known`, the set for
    /// `OneOf`; for `Undetermined` no finite set is known and the error is
    /// its reason.
    pub(super) fn candidates(&self) -> Result<BTreeSet<CwdCand>, &str> {
        match self {
            Cwd::Hook => Ok(BTreeSet::from([CwdCand::Hook])),
            Cwd::Known(c) => Ok(BTreeSet::from([CwdCand::Known(c.clone())])),
            Cwd::OneOf(set) => Ok(set.clone()),
            Cwd::Undetermined(r) => Err(r.as_str()),
        }
    }

    /// The cwd that is one of `set`: a single candidate is that cwd, more
    /// than [`MAX_CWD_CANDIDATES`] is `Undetermined`.
    pub(super) fn from_candidates(set: BTreeSet<CwdCand>) -> Cwd {
        if set.len() > MAX_CWD_CANDIDATES {
            return Cwd::Undetermined(format!(
                "cwd: more than {MAX_CWD_CANDIDATES} directories reach here (a cd may have failed)"
            ));
        }
        let mut it = set.iter();
        match (it.next(), it.next()) {
            (Some(CwdCand::Hook), None) => Cwd::Hook,
            (Some(CwdCand::Known(c)), None) => Cwd::Known(c.clone()),
            (None, _) => Cwd::Undetermined("cwd: no path reaches here".into()),
            _ => Cwd::OneOf(set),
        }
    }

    /// `SimpleCommand::cwd_candidates`.
    pub(super) fn candidate_list(&self) -> CwdCandidates {
        match self {
            Cwd::OneOf(set) => CwdCandidates::OneOf(
                set.iter()
                    .map(|c| match c {
                        CwdCand::Hook => None,
                        CwdCand::Known(d) => Some(d.clone()),
                    })
                    .collect(),
            ),
            Cwd::Hook | Cwd::Known(_) => CwdCandidates::One,
            Cwd::Undetermined(r) => CwdCandidates::Undetermined(r.clone()),
        }
    }

    pub(super) fn from_value(v: &Value) -> Cwd {
        match v {
            Value::Literal(c) => Cwd::Known(c.clone()),
            Value::Undetermined(r) if r == HOOK_CWD => Cwd::Hook,
            Value::Undetermined(r) => Cwd::Undetermined(r.clone()),
        }
    }
}

/// The `Undetermined` reason of a [`Cwd::OneOf`] read as one cwd.
fn one_of_reason(set: &BTreeSet<CwdCand>) -> String {
    let names: Vec<String> = set
        .iter()
        .map(|c| match c {
            CwdCand::Hook => "the hook's cwd".to_string(),
            CwdCand::Known(d) => format!("`{d}`"),
        })
        .collect();
    format!(
        "cwd: differs between the paths that reach here (a cd may have failed): one of {}",
        names.join(", ")
    )
}

/// A redirect target / script operand as the pinned representation wants
/// it: absolute stays as written; relative stays as written when no `cd`
/// applies (the consumer reads it against the hook's cwd); relative after a
/// literal `cd` is joined to it ([`real_join`]: a `..` component is the real
/// parent, user ruling 2026-11-19); relative after an undetermined `cd`, after
/// a `cd` that may have failed ([`Cwd::OneOf`]), or whose `..` cannot be
/// resolved on the real path after a `cd` is `Undetermined`.
pub(super) fn resolve(cwd: &Cwd, v: Value) -> Value {
    let p = match v {
        Value::Literal(p) => p,
        u => return u,
    };
    if p.starts_with('/') {
        return Value::Literal(p);
    }
    match cwd {
        Cwd::Hook => Value::Literal(p),
        Cwd::Undetermined(r) => Value::Undetermined(format!(
            "relative path `{p}` after an undetermined cd ({r})"
        )),
        Cwd::OneOf(set) => Value::Undetermined(format!(
            "relative path `{p}` after an undetermined cd ({})",
            one_of_reason(set)
        )),
        Cwd::Known(c) => match real_join(c, &p) {
            Ok(j) => Value::Literal(j),
            Err(why) => Value::Undetermined(format!("relative path not resolved: {why}")),
        },
    }
}
