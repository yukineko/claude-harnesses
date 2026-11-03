//! PreToolUse gate: deny edits to registered targets that are not ratified.
//!
//! # Decision
//!
//! [`evaluate`] judges one PreToolUse payload and answers a [`Verdict`]; the
//! CLI (`run_gate` in `main.rs`) prints it as
//! `{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":
//! "allow"|"deny","permissionDecisionReason":...}}` via [`decision_json`]:
//! `Clean` is `allow` (exit 0); `Violation` and `Undetermined` are both `deny`
//! (exit 2). There is no path from `Undetermined` to `allow`.
//!
//! # Order
//!
//! 1. The payload is parsed first, so a malformed payload is denied even when
//!    nothing is registered. It must be a JSON object with a non-empty string
//!    `tool_name`. For `Edit` / `Write` / `MultiEdit` the path is
//!    `tool_input.file_path`, for `NotebookEdit` `tool_input.notebook_path`
//!    (a non-empty string; a relative one is joined onto the payload `cwd`,
//!    which must then be absolute). For `Bash`, `tool_input.command` must be a
//!    string and the payload `cwd` an absolute path. Anything missing or
//!    mistyped is `Undetermined`.
//! 2. The state directory ([`crate::target::state_dir`]) and the registered
//!    targets ([`crate::ratify::registered_targets`]) are read; an unresolved
//!    state dir, an unreadable listing or any corrupt registration is
//!    `Undetermined`. No registered target at all is `Clean` for every tool.
//! 3. Any other `tool_name` (outside the hook matcher
//!    `Bash|Edit|Write|MultiEdit|NotebookEdit`) is `Undetermined` while a
//!    target is registered: the gate does not know whether it edits files.
//! 4. Every registered target whose [`crate::ratify::status`] is `Ratified`
//!    is out of the question. Every other one (`Unratified`, or an
//!    `Undetermined` status) is *guarded*.
//! 5. A guarded target's area is its canonical root plus every work tree of
//!    its repository (`git worktree list`, each canonicalized), so a linked
//!    worktree created outside the target dir is inside.
//!    * Edit tools: the path is canonicalized ([`canonical_lenient`]: the
//!      nearest existing ancestor is canonicalized — symlinks resolved — and
//!      the not-yet-existing rest is appended, with `.` / `..` applied
//!      lexically). It is inside a target when it lies under the root or
//!      under any of its work trees.
//!    * Bash: inside when the canonicalized payload `cwd` lies in the area,
//!      or when the command string contains the text of the root or of any
//!      work tree path (canonical or as git printed it). Command parsing is
//!      not attempted: a target named through a relative path from outside,
//!      or through a non-canonical symlink spelling, is not detected.
//!    * Decision: inside an `Unratified` target is a `Violation`; inside a
//!      target whose status is `Undetermined` is `Undetermined`. Failing to
//!      canonicalize the path, or to determine the area (git probe failure;
//!      only needed when the path / cwd / command is not already under the
//!      root), is `Undetermined` — even for a path that might lie elsewhere.
//! 6. Nothing guarded touched: `Clean`.

use std::path::{Component, Path, PathBuf};

use harness_core::verdict::{Determination, Verdict};
use serde_json::Value;

use crate::git;
use crate::ratify;
use crate::target::{self, ResolvedTarget};

/// Tools whose path is in `tool_input.file_path`.
const FILE_PATH_TOOLS: &[&str] = &["Edit", "Write", "MultiEdit"];

/// One parsed PreToolUse request.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Request {
    /// A file-editing tool acting on `path` (absolute, not yet canonicalized).
    Edit { tool: String, path: PathBuf },
    /// A Bash command run in `cwd` (absolute, not yet canonicalized).
    Bash { cwd: PathBuf, command: String },
    /// A tool this gate does not judge.
    Other { tool: String },
}

/// Parse a PreToolUse payload (see the module doc, step 1).
#[track_caller]
fn parse(payload: &str) -> Determination<Request> {
    if payload.trim().is_empty() {
        return Determination::undetermined("the PreToolUse payload on stdin is empty");
    }
    let v: Value = match serde_json::from_str(payload) {
        Ok(v) => v,
        Err(e) => {
            return Determination::undetermined(format!(
                "the PreToolUse payload is not valid JSON: {e}"
            ))
        }
    };
    if !v.is_object() {
        return Determination::undetermined("the PreToolUse payload is not a JSON object");
    }
    let tool = match v.get("tool_name").and_then(Value::as_str) {
        Some(t) if !t.is_empty() => t.to_string(),
        _ => {
            return Determination::undetermined(
                "the PreToolUse payload has no non-empty string `tool_name`",
            )
        }
    };
    let cwd = v.get("cwd").and_then(Value::as_str).map(PathBuf::from);
    let input = v.get("tool_input");

    let field = if FILE_PATH_TOOLS.contains(&tool.as_str()) {
        Some("file_path")
    } else if tool == "NotebookEdit" {
        Some("notebook_path")
    } else {
        None
    };
    if let Some(field) = field {
        let raw = match input.and_then(|i| i.get(field)).and_then(Value::as_str) {
            Some(p) if !p.is_empty() => PathBuf::from(p),
            _ => {
                return Determination::undetermined(format!(
                    "the {tool} payload has no non-empty string `tool_input.{field}`"
                ))
            }
        };
        let path = if raw.is_absolute() {
            raw
        } else {
            match &cwd {
                Some(c) if c.is_absolute() => c.join(raw),
                _ => {
                    return Determination::undetermined(format!(
                        "the {tool} path {} is relative and the payload has no absolute `cwd`",
                        raw.display()
                    ))
                }
            }
        };
        return Determination::known(Request::Edit { tool, path });
    }
    if tool == "Bash" {
        let command = match input.and_then(|i| i.get("command")).and_then(Value::as_str) {
            Some(c) => c.to_string(),
            None => {
                return Determination::undetermined(
                    "the Bash payload has no string `tool_input.command`",
                )
            }
        };
        let cwd = match cwd {
            Some(c) if c.is_absolute() => c,
            _ => {
                return Determination::undetermined("the Bash payload has no absolute `cwd`");
            }
        };
        return Determination::known(Request::Bash { cwd, command });
    }
    Determination::known(Request::Other { tool })
}

/// Canonicalize the absolute path `p`, which need not exist: the longest
/// existing prefix is canonicalized (symlinks resolved) and the remaining,
/// not-yet-existing components are appended, `.` skipped and `..` popping one
/// component. Lexical handling of the rest is sound because none of it exists,
/// so none of it can be a symlink.
///
/// `Undetermined` when `p` is not absolute or any canonicalize error other
/// than `NotFound` occurs (permission denied, a file used as a directory, …).
#[track_caller]
pub fn canonical_lenient(p: &Path) -> Determination<PathBuf> {
    if !p.is_absolute() {
        return Determination::undetermined(format!("path {} is not absolute", p.display()));
    }
    let comps: Vec<Component<'_>> = p.components().collect();
    for n in (1..=comps.len()).rev() {
        let prefix: PathBuf = comps[..n].iter().collect();
        match std::fs::canonicalize(&prefix) {
            Ok(mut base) => {
                for c in &comps[n..] {
                    match c {
                        Component::Normal(name) => base.push(name),
                        Component::CurDir => {}
                        Component::ParentDir => {
                            base.pop();
                        }
                        Component::RootDir | Component::Prefix(_) => {
                            return Determination::undetermined(format!(
                                "path {} has a root component after its start",
                                p.display()
                            ))
                        }
                    }
                }
                return Determination::known(base);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                return Determination::undetermined(format!(
                    "path {} cannot be canonicalized: {e}",
                    prefix.display()
                ))
            }
        }
    }
    Determination::undetermined(format!(
        "no prefix of path {} could be canonicalized",
        p.display()
    ))
}

/// The area of a guarded target: canonical root, canonical work trees, and
/// the work tree paths as git printed them (for the Bash text match).
struct Area {
    canonical: Vec<PathBuf>,
    spelled: Vec<PathBuf>,
}

/// Root + work trees of `t`; see the module doc, step 5.
fn area(t: &ResolvedTarget) -> Determination<Area> {
    let wts = match git::worktree_paths(&t.root) {
        Determination::Known(w) => w,
        Determination::Undetermined(why) => {
            return Determination::undetermined(format!(
                "cannot list the work trees of registered target {} \
                 (if it was deleted, remove its fde state): {why}",
                t.root.display()
            ))
        }
    };
    let mut canonical = vec![t.root.clone()];
    for w in &wts {
        match canonical_lenient(w) {
            Determination::Known(c) => canonical.push(c),
            Determination::Undetermined(why) => return Determination::Undetermined(why),
        }
    }
    let mut spelled = vec![t.root.clone()];
    spelled.extend(wts);
    Determination::known(Area { canonical, spelled })
}

/// Whether the request touches `area` (`Some(description)`) or not (`None`).
fn touches(req: &Checked, area: &Area) -> Option<String> {
    match req {
        Checked::Path { tool, path } => area
            .canonical
            .iter()
            .find(|a| path.starts_with(a))
            .map(|a| format!("{tool} of {} lies in {}", path.display(), a.display())),
        Checked::Bash { cwd, command } => {
            if let Some(a) = area.canonical.iter().find(|a| cwd.starts_with(a)) {
                return Some(format!(
                    "Bash runs in {} inside {}",
                    cwd.display(),
                    a.display()
                ));
            }
            area.canonical
                .iter()
                .chain(area.spelled.iter())
                .filter_map(|a| a.to_str())
                .find(|a| command.contains(a))
                .map(|a| format!("the Bash command names {a}"))
        }
    }
}

/// A request with its path / cwd canonicalized.
enum Checked {
    Path { tool: String, path: PathBuf },
    Bash { cwd: PathBuf, command: String },
}

/// Judge one PreToolUse `payload` (the raw stdin JSON) against the targets
/// registered in the state directory ([`crate::target::state_dir`]).
///
/// * `Clean` — allow (exit 0).
/// * `Violation` — deny (exit 2): the call touches an unratified registered
///   target.
/// * `Undetermined` — deny (exit 2): see the module doc for every case.
#[track_caller]
pub fn evaluate(payload: &str) -> Verdict {
    let req = match parse(payload) {
        Determination::Known(r) => r,
        Determination::Undetermined(why) => return why.into_verdict(),
    };
    let state = match target::state_dir() {
        Determination::Known(s) => s,
        Determination::Undetermined(why) => return why.into_verdict(),
    };
    evaluate_with(req, &state)
}

fn evaluate_with(req: Request, state: &Path) -> Verdict {
    let targets = match ratify::registered_targets(state) {
        Determination::Known(t) => t,
        Determination::Undetermined(why) => return why.into_verdict(),
    };
    if targets.is_empty() {
        return Verdict::from_findings(Vec::new());
    }
    let checked = match req {
        Request::Other { tool } => {
            return Verdict::undetermined(format!(
                "tool {tool} is not one the fde gate judges, and targets are registered; \
                 it cannot tell whether {tool} edits one"
            ))
        }
        Request::Edit { tool, path } => match canonical_lenient(&path) {
            Determination::Known(p) => Checked::Path { tool, path: p },
            Determination::Undetermined(why) => return why.into_verdict(),
        },
        Request::Bash { cwd, command } => match canonical_lenient(&cwd) {
            Determination::Known(c) => Checked::Bash { cwd: c, command },
            Determination::Undetermined(why) => return why.into_verdict(),
        },
    };

    let mut verdicts = Vec::new();
    for t in &targets {
        let unratified_reason = match ratify::status(t, state) {
            Determination::Known(ratify::Status::Ratified { .. }) => continue,
            Determination::Known(ratify::Status::Unratified { reason }) => Ok(reason),
            Determination::Undetermined(why) => Err(why),
        };
        // Fast path: inside the root itself needs no git probe.
        let root_only = Area {
            canonical: vec![t.root.clone()],
            spelled: vec![t.root.clone()],
        };
        let hit = match touches(&checked, &root_only) {
            Some(h) => Some(h),
            None => match area(t) {
                Determination::Known(a) => touches(&checked, &a),
                Determination::Undetermined(why) => {
                    verdicts.push(why.into_verdict());
                    continue;
                }
            },
        };
        let Some(hit) = hit else { continue };
        verdicts.push(match unratified_reason {
            Ok(reason) => Verdict::violation(format!(
                "{hit}: registered target {} is not ratified ({reason}); \
                 ratify its spec with `fde ratify` first",
                t.root.display()
            )),
            Err(why) => Verdict::undetermined(format!(
                "{hit}: the ratification of registered target {} cannot be determined: {why}",
                t.root.display()
            )),
        });
    }
    if verdicts.is_empty() {
        return Verdict::from_findings(Vec::new());
    }
    Verdict::worst_of(verdicts)
}

/// The PreToolUse decision JSON for `verdict`: `allow` for `Clean`, `deny`
/// for `Violation` and `Undetermined`.
pub fn decision_json(verdict: &Verdict) -> String {
    let (decision, reason) = match verdict {
        Verdict::Clean(_) => (
            "allow",
            "fde: the call touches no unratified registered target".to_string(),
        ),
        Verdict::Violation(why) => ("deny", format!("fde: denied: {why}")),
        Verdict::Undetermined(why) => ("deny", format!("fde: undetermined, so denied: {why}")),
    };
    serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": decision,
            "permissionDecisionReason": reason,
        }
    })
    .to_string()
}
