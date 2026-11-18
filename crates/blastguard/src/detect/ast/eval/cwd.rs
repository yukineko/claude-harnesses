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

use super::super::support::join_path;
use super::super::{CwdCandidates, Value};
use super::HOOK_CWD;
use std::collections::BTreeSet;

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
/// literal `cd` is joined to it; relative after an undetermined `cd`, after a
/// `cd` that may have failed ([`Cwd::OneOf`]), or with a `..` component after
/// a `cd` is `Undetermined`.
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
        Cwd::Known(c) => match join_path(c, &p) {
            Some(j) => Value::Literal(j),
            None => Value::Undetermined(format!(
                "`{p}` after cd {c}: `..` / empty path not resolved (symlinks make it unsound)"
            )),
        },
    }
}
