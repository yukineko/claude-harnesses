//! Evidence ledger: per-requirement RED / GREEN / PASS records for a target,
//! and the checks over them (`verify`, `report`).
//!
//! # Ledger layout
//!
//! One file per target, `<state_dir>/targets/<state_key>/evidence.json` (next
//! to `registration.json` / `ratification.json`; never inside the target):
//!
//! ```text
//! {"schema": 2, "records": [
//!   {"fingerprint": "<spec sha256>", "req": "R1", "kind": "red", "cmd": "...", "exit_code": 1,
//!    "head": "<40 lowercase hex>", "dirty": false},
//!   ...
//! ]}
//! ```
//!
//! `head` is the target HEAD commit ([`crate::git::head`]) and `dirty` whether
//! the target work tree differed from it ([`crate::git::is_dirty`]: untracked
//! files count, ignored files do not), both observed immediately **before**
//! `CMD` ran. A ledger written by an earlier fde (`schema: 1`, records without
//! `head` / `dirty`) is still read; its records keep no `head` (and are
//! carried over without one when the ledger is next rewritten as schema 2). In
//! schema 2 a record has both `head` and `dirty` or neither (carried over);
//! anything else, or an unknown schema, makes the ledger `Undetermined`.
//!
//! Records are append-only and kept in recording order. Every record carries
//! the fingerprint of the ratified spec it was recorded against; only records
//! whose fingerprint equals the *current* ratified fingerprint count. A spec
//! change (and re-ratification) therefore leaves earlier evidence on disk but
//! out of every answer. The file is rewritten atomically (temp file + rename,
//! [`crate::state::write_json_atomic`]); two concurrent recorders can lose one
//! of the two appends (last rename wins), which can only make the ledger
//! *less* complete, never more.
//!
//! # Recording (`record`)
//!
//! In this order, the first answer wins:
//!
//! 1. Ratification is observed ([`crate::ratify::observe`]). Not registered,
//!    registered but unratified, or spec changed since ratification is a
//!    determined refusal (`Violation`); an unreadable store/spec is
//!    `Undetermined`. A legacy ratification without a recorded HEAD
//!    ([`crate::ratify::Provenance::Legacy`]) is `Undetermined` (re-ratify).
//!    The current target HEAD is then observed and the ratification head `R`
//!    must be an ancestor of, or equal to, it (`git merge-base --is-ancestor`);
//!    "not an ancestor" (reset, branch switch, rewritten history) or any
//!    failure to observe or compare is `Undetermined`, naming both commits.
//! 2. The ratified spec bytes are parsed as specforge Spec IR TOML
//!    (`[spec] id`, `[[requirement]] id`). Unparsable, a missing/empty id or a
//!    duplicate requirement id is `Undetermined`. A `--req` that is not a
//!    requirement of the spec, or an empty `--cmd`, is `Violation`.
//! 3. The state dir must lie outside the target (`Violation` otherwise).
//! 4. The ledger is read; unreadable, corrupt, unknown schema, or internally
//!    inconsistent (see `evaluate`) is `Undetermined`.
//! 5. Preconditions (`Violation` when unmet, without running `CMD`):
//!    GREEN needs an earlier RED for the same requirement **and the same
//!    `CMD` text**; PASS needs that the requirement has no RED at all (a
//!    requirement with a RED is closed by its GREEN, not by a PASS). RED has
//!    no precondition.
//! 6. Immediately before `CMD` runs, the target HEAD and dirty flag are
//!    observed (they become the record's `head` / `dirty`) and the step-1
//!    ancestry check is repeated against that HEAD; any failure is
//!    `Undetermined` and `CMD` does not run.
//!    `CMD` runs as `/bin/sh -c CMD` with cwd = the target root, stdin null,
//!    the repo-relocating `GIT_*` variables removed, bounded by
//!    [`CMD_TIMEOUT`]. The **exit status** decides: RED needs a non-zero
//!    exit, GREEN and PASS need exit 0; the other outcome is `Violation`. A
//!    spawn failure, a signal (no exit code), a timeout, unreadable output, or
//!    exit 126/127 (the shell could not execute / find the command, so nothing
//!    was tested) is `Undetermined` and records nothing.
//! 7. Ratification is observed again: if it is no longer ratified with the
//!    same fingerprint and the same recorded HEAD / dirty flag, nothing is
//!    recorded (`Violation`).
//! 8. The ledger is re-read, the step-5 precondition re-checked against it,
//!    the record appended, and the file written atomically. Only a successful
//!    write is `Clean`.
//!
//! # Completeness (`verify`, `report`)
//!
//! Per requirement, over current-fingerprint records in order: a requirement
//! is **unresolved** while some RED `CMD` has no later GREEN with the same
//! `CMD`; otherwise it is **complete** when it has a GREEN (after its RED) or a
//! PASS; otherwise it is **unmeasured**. `verify` is `Clean` only when every
//! requirement of the ratified spec is complete.
//!
//! `report` applies a **stricter** bar (user ruling 2026-11-03: "Exit 0 only
//! when it is ratified and every requirement is verified RED→GREEN"): its
//! verdict is `Clean` only when every requirement is GREEN after RED. A
//! PASS-only requirement counts as complete for `verify` but not for
//! `report`, and an unratified target is a determined `Violation` for
//! `report` (while `verify` answers it `Undetermined`).
//!
//! # History (`verify`, `report`)
//!
//! Before either answers, the evidence must lie on the ratified history
//! (forward history only): with `R` the ratification head, `C` the current
//! target HEAD and `H` the `head` of each counted (current-fingerprint)
//! record, `R` must be an ancestor of or equal to `C`, and `R <= H <= C` for
//! every counted record. A legacy ratification (no `R`), a counted record
//! without `head`, a `C` that cannot be observed, any "not an ancestor"
//! answer, or any failed comparison (e.g. a commit unknown to the repository)
//! is `Undetermined`: `verify` exits 2 and `report` exits 2 without text.
//! `dirty` is recorded and shown by `report` but is not by itself
//! disqualifying.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use harness_core::boundary;
use harness_core::verdict::{Determination, Verdict};

use crate::ratify::{self, Observed, Provenance, RatifiedSpec};
use crate::state;
use crate::target::ResolvedTarget;

/// Schema version of the ledger file as written now (records carry `head` /
/// `dirty`).
const SCHEMA: u32 = 2;
/// The earlier ledger schema (records without `head` / `dirty`), still read.
const LEGACY_SCHEMA: u32 = 1;
const LEDGER_FILE: &str = "evidence.json";

/// Upper bound on one evidence command. A command that has not exited by
/// then is undetermined (its process group is killed), never a pass or a fail.
pub const CMD_TIMEOUT: Duration = Duration::from_secs(60 * 60);

/// Which observation an `evidence` record claims.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// `CMD` was run and observed failing (the RED half of F->P).
    Red,
    /// `CMD` was run and observed passing after a recorded RED.
    Green,
    /// `CMD` was run and observed passing (no RED claimed).
    Pass,
}

impl Kind {
    fn label(self) -> &'static str {
        match self {
            Kind::Red => "RED",
            Kind::Green => "GREEN",
            Kind::Pass => "PASS",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    fingerprint: String,
    req: String,
    kind: Kind,
    cmd: String,
    exit_code: i32,
    /// Target HEAD observed before `CMD` ran; `None` only for a record
    /// written by an earlier fde (schema 1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    head: Option<String>,
    /// Whether the target work tree was dirty then; present exactly when
    /// `head` is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    dirty: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Ledger {
    schema: u32,
    records: Vec<Record>,
}

/// The parts of a specforge Spec IR TOML fde reads. Other keys are ignored.
#[derive(Debug, serde::Deserialize)]
struct SpecDoc {
    spec: SpecHeader,
    #[serde(default)]
    requirement: Vec<SpecRequirement>,
}

#[derive(Debug, serde::Deserialize)]
struct SpecHeader {
    id: String,
}

#[derive(Debug, serde::Deserialize)]
struct SpecRequirement {
    id: String,
}

/// Requirement ids of the ratified spec, in spec order. `Undetermined` when
/// the bytes are not UTF-8 / not the expected TOML, an id is empty, or a
/// requirement id repeats. An empty list is returned as `Known(vec![])`; the
/// callers decide what an empty set means (it is never "complete").
#[track_caller]
fn requirement_ids(spec: &RatifiedSpec) -> Determination<Vec<String>> {
    let text = match std::str::from_utf8(&spec.bytes) {
        Ok(t) => t,
        Err(e) => {
            return Determination::undetermined(format!(
                "ratified spec {} is not UTF-8: {e}",
                spec.path.display()
            ))
        }
    };
    let doc: SpecDoc = match toml::from_str(text) {
        Ok(d) => d,
        Err(e) => {
            return Determination::undetermined(format!(
                "ratified spec {} is not a readable Spec IR TOML: {e}",
                spec.path.display()
            ))
        }
    };
    if doc.spec.id.trim().is_empty() {
        return Determination::undetermined(format!(
            "ratified spec {} has an empty [spec] id",
            spec.path.display()
        ));
    }
    let mut seen = BTreeSet::new();
    let mut ids = Vec::new();
    for r in doc.requirement {
        if r.id.trim().is_empty() {
            return Determination::undetermined(format!(
                "ratified spec {} has a requirement with an empty id",
                spec.path.display()
            ));
        }
        if !seen.insert(r.id.clone()) {
            return Determination::undetermined(format!(
                "ratified spec {} lists requirement {} more than once",
                spec.path.display(),
                r.id
            ));
        }
        ids.push(r.id);
    }
    Determination::known(ids)
}

fn ledger_path(target: &ResolvedTarget, state_dir: &Path) -> PathBuf {
    state::target_dir(state_dir, &target.state_key).join(LEDGER_FILE)
}

/// Read the ledger: absent is the empty ledger; unreadable, corrupt or an
/// unknown schema is `Undetermined`. A schema-1 ledger (no record carries
/// `head` / `dirty`) is returned as schema 2 with head-less records, so the
/// next write upgrades it without inventing a head. In schema 2 a record with
/// only one of `head` / `dirty`, or a `head` that is not 40 lowercase hex, is
/// `Undetermined`.
#[track_caller]
fn read_ledger(path: &Path) -> Determination<Ledger> {
    let mut ledger: Ledger = match state::read_json(path) {
        Determination::Known(Some(l)) => l,
        Determination::Known(None) => {
            return Determination::known(Ledger {
                schema: SCHEMA,
                records: Vec::new(),
            })
        }
        Determination::Undetermined(why) => return Determination::Undetermined(why),
    };
    match ledger.schema {
        SCHEMA => {
            for (i, rec) in ledger.records.iter().enumerate() {
                let well_formed = match (&rec.head, rec.dirty) {
                    (Some(h), Some(_)) => crate::git::is_commit_id(h),
                    (None, None) => true,
                    _ => false,
                };
                if !well_formed {
                    return Determination::undetermined(format!(
                        "evidence ledger {} is corrupt: record #{i} has head {:?} and dirty {:?} \
                         (both, with a 40-digit lowercase hex head, or neither are expected)",
                        path.display(),
                        rec.head,
                        rec.dirty
                    ));
                }
            }
        }
        LEGACY_SCHEMA => {
            if let Some(i) = ledger
                .records
                .iter()
                .position(|r| r.head.is_some() || r.dirty.is_some())
            {
                return Determination::undetermined(format!(
                    "evidence ledger {} has schema {LEGACY_SCHEMA}, but record #{i} carries \
                     head/dirty fields that schema never had",
                    path.display()
                ));
            }
            ledger.schema = SCHEMA;
        }
        other => {
            return Determination::undetermined(format!(
                "evidence ledger {} has unknown schema {other}",
                path.display()
            ))
        }
    }
    Determination::known(ledger)
}

/// Evidence state of one requirement.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ReqState {
    /// Some RED `CMD`s have no later GREEN with the same `CMD`.
    Unresolved { open: Vec<String> },
    /// Every RED is followed by its GREEN, and there is at least one GREEN.
    GreenAfterRed { cmds: Vec<String> },
    /// No RED; at least one PASS.
    Pass { cmds: Vec<String> },
    /// No current-fingerprint record at all.
    Unmeasured,
}

impl ReqState {
    fn complete(&self) -> bool {
        matches!(self, ReqState::GreenAfterRed { .. } | ReqState::Pass { .. })
    }
}

/// Per-requirement accumulation over the ledger.
#[derive(Default)]
struct Acc {
    red_cmds: BTreeSet<String>,
    open: Vec<String>,
    greens: Vec<String>,
    passes: Vec<String>,
}

fn push_unique(v: &mut Vec<String>, s: &str) {
    if !v.iter().any(|x| x == s) {
        v.push(s.to_owned());
    }
}

/// Where one counted record was taken.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RecordAt {
    /// Position in the ledger (for messages).
    index: usize,
    kind: Kind,
    /// `None` only for a record written by an earlier fde.
    head: Option<String>,
    dirty: Option<bool>,
}

/// The evaluated ledger against the requirements of one ratified spec.
struct Evaluation {
    states: Vec<(String, ReqState)>,
    /// Counted (current-fingerprint) records per requirement, in ledger
    /// order; parallel to `states`.
    records: Vec<Vec<RecordAt>>,
    /// Records from other fingerprints (earlier ratifications), ignored.
    stale: usize,
}

/// Evaluate `ledger` for `reqs` under `fingerprint`. A current-fingerprint
/// record that recording would never have written — a requirement outside
/// the spec, a GREEN without an earlier RED of the same `CMD`, a PASS after a
/// RED of its requirement — makes the ledger inconsistent: `Undetermined`.
#[track_caller]
fn evaluate(reqs: &[String], ledger: &Ledger, fingerprint: &str) -> Determination<Evaluation> {
    let mut accs: Vec<Acc> = reqs.iter().map(|_| Acc::default()).collect();
    let mut records: Vec<Vec<RecordAt>> = reqs.iter().map(|_| Vec::new()).collect();
    let mut stale = 0usize;
    for (i, rec) in ledger.records.iter().enumerate() {
        if rec.fingerprint != fingerprint {
            stale += 1;
            continue;
        }
        let Some(idx) = reqs.iter().position(|r| *r == rec.req) else {
            return Determination::undetermined(format!(
                "evidence ledger is inconsistent: record #{i} names requirement {:?}, \
                 which is not in the ratified spec",
                rec.req
            ));
        };
        let (Some(acc), Some(at)) = (accs.get_mut(idx), records.get_mut(idx)) else {
            return Determination::undetermined(format!(
                "evidence ledger evaluation lost requirement {:?}",
                rec.req
            ));
        };
        at.push(RecordAt {
            index: i,
            kind: rec.kind,
            head: rec.head.clone(),
            dirty: rec.dirty,
        });
        match rec.kind {
            Kind::Red => {
                acc.red_cmds.insert(rec.cmd.clone());
                push_unique(&mut acc.open, &rec.cmd);
            }
            Kind::Green => {
                if !acc.red_cmds.contains(&rec.cmd) {
                    return Determination::undetermined(format!(
                        "evidence ledger is inconsistent: record #{i} is a GREEN for {} \
                         without an earlier RED of the same command",
                        rec.req
                    ));
                }
                acc.open.retain(|c| *c != rec.cmd);
                push_unique(&mut acc.greens, &rec.cmd);
            }
            Kind::Pass => {
                if !acc.red_cmds.is_empty() {
                    return Determination::undetermined(format!(
                        "evidence ledger is inconsistent: record #{i} is a PASS for {} \
                         after a RED of that requirement",
                        rec.req
                    ));
                }
                push_unique(&mut acc.passes, &rec.cmd);
            }
        }
    }
    let states = reqs
        .iter()
        .zip(accs)
        .map(|(id, acc)| {
            let st = if !acc.open.is_empty() {
                ReqState::Unresolved { open: acc.open }
            } else if !acc.greens.is_empty() {
                ReqState::GreenAfterRed { cmds: acc.greens }
            } else if !acc.passes.is_empty() {
                ReqState::Pass { cmds: acc.passes }
            } else {
                ReqState::Unmeasured
            };
            (id.clone(), st)
        })
        .collect();
    Determination::known(Evaluation {
        states,
        records,
        stale,
    })
}

/// Step-5 precondition for recording `kind` of `cmd` for `req`. `None` = met.
fn precondition_unmet(
    kind: Kind,
    req: &str,
    cmd: &str,
    ledger: &Ledger,
    fingerprint: &str,
) -> Option<String> {
    let mut current = ledger
        .records
        .iter()
        .filter(|r| r.fingerprint == fingerprint && r.req == req);
    match kind {
        Kind::Red => None,
        Kind::Green => {
            if current.any(|r| r.kind == Kind::Red && r.cmd == cmd) {
                None
            } else {
                Some(format!(
                    "refused: no RED is recorded for requirement {req} with the command {cmd:?}; \
                     a GREEN must follow a recorded RED of the same requirement and command"
                ))
            }
        }
        Kind::Pass => {
            if current.any(|r| r.kind == Kind::Red) {
                Some(format!(
                    "refused: requirement {req} has a recorded RED; close it with a GREEN of \
                     that command, not a PASS"
                ))
            } else {
                None
            }
        }
    }
}

/// The ratified spec of `target` and its requirement ids, or the verdict to
/// return instead (`Violation` for a determined "not ratified").
fn ratified(
    target: &ResolvedTarget,
    state_dir: &Path,
) -> Result<(RatifiedSpec, Vec<String>), Verdict> {
    let spec = match ratify::observe(target, state_dir) {
        Determination::Known(Observed::Ratified(s)) => s,
        Determination::Known(Observed::Unratified { reason }) => {
            return Err(Verdict::violation(format!(
                "refused: target {} is not ratified ({reason}); evidence is only recorded \
                 against a ratified spec",
                target.root.display()
            )))
        }
        Determination::Undetermined(why) => return Err(why.into_verdict()),
    };
    match requirement_ids(&spec) {
        Determination::Known(ids) => Ok((spec, ids)),
        Determination::Undetermined(why) => Err(why.into_verdict()),
    }
}

/// The recorded ratification head and dirty flag of `spec`. A legacy
/// ratification has none: `Undetermined`, asking the human to re-ratify.
#[track_caller]
fn ratification_head(spec: &RatifiedSpec) -> Determination<(String, bool)> {
    match &spec.provenance {
        Provenance::Recorded { head, dirty } => Determination::known((head.clone(), *dirty)),
        Provenance::Legacy => Determination::undetermined(format!(
            "the ratification of {} was written by an earlier fde without the target HEAD \
             commit, so evidence cannot be placed on the ratified history; a human must \
             re-ratify it (`fde ratify`) to record the HEAD",
            spec.path.display()
        )),
    }
}

/// `Known(())` when `ancestor` is an ancestor of, or equal to, `descendant`;
/// `Undetermined` (with `what` and both commits named) when it is not or the
/// comparison failed.
#[track_caller]
fn require_ancestor(
    ancestor: &str,
    descendant: &str,
    root: &Path,
    what: &str,
) -> Determination<()> {
    match crate::git::is_ancestor(ancestor, descendant, root) {
        Determination::Known(true) => Determination::known(()),
        Determination::Known(false) => Determination::undetermined(format!(
            "{what}: commit {ancestor} is not an ancestor of (or equal to) commit {descendant} \
             in {} (HEAD reset, switched to another branch, or history rewritten); only forward \
             history from the ratified commit counts",
            root.display()
        )),
        Determination::Undetermined(why) => Determination::undetermined(format!(
            "{what}: cannot decide whether commit {ancestor} is an ancestor of commit \
             {descendant} in {}: {}",
            root.display(),
            why.as_str()
        )),
    }
}

/// Observe the current target HEAD and require the ratification head `r` to
/// be an ancestor of, or equal to, it. `Known(head)` only then.
#[track_caller]
fn head_on_ratified_line(r: &str, root: &Path) -> Determination<String> {
    let c = match crate::git::head(root) {
        Determination::Known(c) => c,
        Determination::Undetermined(why) => {
            return Determination::undetermined(format!(
                "the current target HEAD cannot be determined: {}",
                why.as_str()
            ))
        }
    };
    match require_ancestor(
        r,
        &c,
        root,
        &format!("the current target HEAD {c} is off the ratified history (ratified at {r})"),
    ) {
        Determination::Known(()) => Determination::known(c),
        Determination::Undetermined(why) => Determination::Undetermined(why),
    }
}

/// The commits a judged ledger sits on: the ratification head (and whether
/// the tree was dirty then) and the current target HEAD.
struct History {
    ratified: String,
    ratified_dirty: bool,
    current: String,
}

/// Apply the history rule (module docs, "History") to an evaluated ledger:
/// `R <= C` and `R <= H <= C` for every counted record. Any violation or
/// failure to compare is `Undetermined`.
#[track_caller]
fn check_history(
    target: &ResolvedTarget,
    spec: &RatifiedSpec,
    ev: &Evaluation,
) -> Determination<History> {
    let (r, r_dirty) = match ratification_head(spec) {
        Determination::Known(x) => x,
        Determination::Undetermined(why) => return Determination::Undetermined(why),
    };
    let c = match head_on_ratified_line(&r, &target.root) {
        Determination::Known(c) => c,
        Determination::Undetermined(why) => return Determination::Undetermined(why),
    };
    let mut checked: BTreeSet<String> = BTreeSet::new();
    for ((id, _), recs) in ev.states.iter().zip(&ev.records) {
        for rec in recs {
            let Some(h) = &rec.head else {
                return Determination::undetermined(format!(
                    "evidence record #{} ({} for {id}) was written by an earlier fde without the \
                     target HEAD, so it cannot be placed on the ratified history; it stays \
                     counted while the ratified spec fingerprint is unchanged",
                    rec.index,
                    rec.kind.label()
                ));
            };
            if !checked.insert(h.clone()) {
                continue;
            }
            let what = format!(
                "evidence record #{} ({} for {id}) at {h} is off the ratified history \
                 (ratified at {r}, current HEAD {c})",
                rec.index,
                rec.kind.label()
            );
            for (a, b) in [(r.as_str(), h.as_str()), (h.as_str(), c.as_str())] {
                if let Determination::Undetermined(why) =
                    require_ancestor(a, b, &target.root, &what)
                {
                    return Determination::Undetermined(why);
                }
            }
        }
    }
    Determination::known(History {
        ratified: r,
        ratified_dirty: r_dirty,
        current: c,
    })
}

/// Run `cmd` at the target root. `Known(output)` only for a process that
/// ran to completion with an exit code other than 126/127.
#[track_caller]
fn run_cmd(cmd: &str, root: &Path) -> Determination<boundary::CommandOutput> {
    let mut c = Command::new("/bin/sh");
    harness_core::git_env::scrub(&mut c);
    c.arg("-c")
        .arg(cmd)
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = match boundary::run_with_timeout(&mut c, CMD_TIMEOUT) {
        Determination::Known(o) => o,
        Determination::Undetermined(why) => return Determination::Undetermined(why),
    };
    if matches!(out.code(), 126 | 127) {
        let tail: String = out.stderr().chars().take(400).collect();
        return Determination::undetermined(format!(
            "the shell could not execute {cmd:?} (exit {}); nothing was tested. stderr: {}",
            out.code(),
            if tail.is_empty() { "<empty>" } else { &tail }
        ));
    }
    Determination::known(out)
}

/// Run `cmd` for requirement `req` and record the observation of kind `kind`
/// in `target`'s ledger under `state_dir` (see the module docs for the order
/// of checks).
///
/// * `Clean` — the observation matched `kind` and was written.
/// * `Violation` — a determined refusal: not ratified (incl. spec changed),
///   `req` not in the ratified spec, empty `cmd`, state dir inside the target,
///   unmet precondition, or the exit status contradicts `kind`. Nothing is
///   recorded.
/// * `Undetermined` — store/spec/ledger unreadable or corrupt, a legacy
///   ratification without a recorded HEAD, a target HEAD that cannot be
///   observed or is not a descendant of (or equal to) the ratification head,
///   a dirty flag that cannot be observed, or `cmd` could not be run to an
///   exit code. Nothing is recorded.
#[track_caller]
pub fn record(
    kind: Kind,
    req: &str,
    cmd: &str,
    target: &ResolvedTarget,
    state_dir: &Path,
) -> Verdict {
    let (spec, reqs) = match ratified(target, state_dir) {
        Ok(x) => x,
        Err(v) => return v,
    };
    let (ratified_head, _) = match ratification_head(&spec) {
        Determination::Known(x) => x,
        Determination::Undetermined(why) => return why.into_verdict(),
    };
    if let Determination::Undetermined(why) = head_on_ratified_line(&ratified_head, &target.root) {
        return Verdict::undetermined(format!(
            "{} not recorded for {req}: {}",
            kind.label(),
            why.as_str()
        ));
    }
    if !reqs.iter().any(|r| r == req) {
        return Verdict::violation(format!(
            "refused: {req:?} is not a requirement of the ratified spec {} (requirements: {})",
            spec.path.display(),
            if reqs.is_empty() {
                "<none>".to_owned()
            } else {
                reqs.join(", ")
            }
        ));
    }
    if cmd.trim().is_empty() {
        return Verdict::violation("refused: --cmd is empty; there is nothing to observe");
    }
    match ratify::state_outside_target(target, state_dir) {
        Verdict::Clean(_) => {}
        other => return other,
    }
    let path = ledger_path(target, state_dir);
    let ledger = match read_ledger(&path) {
        Determination::Known(l) => l,
        Determination::Undetermined(why) => return why.into_verdict(),
    };
    if let Determination::Undetermined(why) = evaluate(&reqs, &ledger, &spec.fingerprint) {
        return why.into_verdict();
    }
    if let Some(why) = precondition_unmet(kind, req, cmd, &ledger, &spec.fingerprint) {
        return Verdict::violation(why);
    }

    // Provenance of this record: observed immediately before CMD runs, so it
    // describes the tree CMD ran on (not what CMD left behind).
    let head = match head_on_ratified_line(&ratified_head, &target.root) {
        Determination::Known(h) => h,
        Determination::Undetermined(why) => {
            return Verdict::undetermined(format!(
                "{} not recorded for {req}: {}",
                kind.label(),
                why.as_str()
            ))
        }
    };
    let dirty = match crate::git::is_dirty(&target.root) {
        Determination::Known(d) => d,
        Determination::Undetermined(why) => {
            return Verdict::undetermined(format!(
                "{} not recorded for {req}: whether the target work tree is dirty cannot be \
                 determined: {}",
                kind.label(),
                why.as_str()
            ))
        }
    };

    let out = match run_cmd(cmd, &target.root) {
        Determination::Known(o) => o,
        Determination::Undetermined(why) => return why.into_verdict(),
    };
    let code = out.code();
    let matches_kind = match kind {
        Kind::Red => code != 0,
        Kind::Green | Kind::Pass => code == 0,
    };
    if !matches_kind {
        let tail: String = out.stderr().chars().take(400).collect();
        let want = match kind {
            Kind::Red => "a non-zero exit",
            Kind::Green | Kind::Pass => "exit 0",
        };
        return Verdict::violation(format!(
            "{} not recorded for {req}: {cmd:?} exited {code}, but {} needs {want}. stderr: {}",
            kind.label(),
            kind.label(),
            if tail.is_empty() { "<empty>" } else { &tail }
        ));
    }

    // The spec may have changed while the command ran.
    match ratify::observe(target, state_dir) {
        Determination::Known(Observed::Ratified(now))
            if now.fingerprint == spec.fingerprint && now.provenance == spec.provenance => {}
        Determination::Known(_) => {
            return Verdict::violation(format!(
                "{} not recorded for {req}: the ratification of {} changed while the command ran",
                kind.label(),
                spec.path.display()
            ))
        }
        Determination::Undetermined(why) => return why.into_verdict(),
    }
    let mut ledger = match read_ledger(&path) {
        Determination::Known(l) => l,
        Determination::Undetermined(why) => return why.into_verdict(),
    };
    if let Determination::Undetermined(why) = evaluate(&reqs, &ledger, &spec.fingerprint) {
        return why.into_verdict();
    }
    if let Some(why) = precondition_unmet(kind, req, cmd, &ledger, &spec.fingerprint) {
        return Verdict::violation(why);
    }
    ledger.records.push(Record {
        fingerprint: spec.fingerprint,
        req: req.to_owned(),
        kind,
        cmd: cmd.to_owned(),
        exit_code: code,
        head: Some(head),
        dirty: Some(dirty),
    });
    match state::write_json_atomic(&path, &ledger) {
        Determination::Known(()) => Verdict::from_findings(Vec::new()),
        Determination::Undetermined(why) => why.into_verdict(),
    }
}

/// The ratified spec, its requirements (non-empty) and their evaluation, or
/// the reason it cannot be determined. Unratified is `Undetermined` here:
/// there is no ratified requirement set to judge the evidence against.
#[track_caller]
fn evaluated(
    target: &ResolvedTarget,
    state_dir: &Path,
) -> Determination<(RatifiedSpec, Evaluation)> {
    let (spec, reqs) = match ratified(target, state_dir) {
        Ok(x) => x,
        Err(Verdict::Violation(why)) => {
            return Determination::undetermined(format!(
                "evidence cannot be judged: {}",
                why.as_str()
            ))
        }
        Err(Verdict::Undetermined(why)) => return Determination::Undetermined(why),
        Err(Verdict::Clean(_)) => {
            return Determination::undetermined(
                "evidence cannot be judged: the ratified spec could not be read",
            )
        }
    };
    if reqs.is_empty() {
        return Determination::undetermined(format!(
            "the ratified spec {} has no requirements; zero requirements cannot be complete",
            spec.path.display()
        ));
    }
    let ledger = match read_ledger(&ledger_path(target, state_dir)) {
        Determination::Known(l) => l,
        Determination::Undetermined(why) => return Determination::Undetermined(why),
    };
    match evaluate(&reqs, &ledger, &spec.fingerprint) {
        Determination::Known(ev) => Determination::known((spec, ev)),
        Determination::Undetermined(why) => Determination::Undetermined(why),
    }
}

/// Check `target`'s ledger against its ratified spec.
///
/// * `Clean` — every requirement is complete (GREEN after RED, or PASS).
/// * `Violation` — some requirement is unresolved or unmeasured (named).
/// * `Undetermined` — not ratified (incl. spec changed), a spec with zero
///   requirements, an unreadable/corrupt/inconsistent ledger or store, or the
///   history rule (module docs, "History") violated or undecidable.
#[track_caller]
pub fn verify(target: &ResolvedTarget, state_dir: &Path) -> Verdict {
    let (spec, ev) = match evaluated(target, state_dir) {
        Determination::Known(x) => x,
        Determination::Undetermined(why) => return why.into_verdict(),
    };
    if let Determination::Undetermined(why) = check_history(target, &spec, &ev) {
        return why.into_verdict();
    }
    let missing: Vec<String> = ev
        .states
        .iter()
        .filter(|(_, st)| !st.complete())
        .map(|(id, st)| match st {
            ReqState::Unresolved { open } => format!("{id} (RED without GREEN: {open:?})"),
            _ => format!("{id} (no evidence)"),
        })
        .collect();
    if missing.is_empty() {
        return Verdict::from_findings(Vec::new());
    }
    Verdict::violation(format!(
        "evidence incomplete for {}: {} of {} requirement(s) lack complete evidence: {}",
        target.root.display(),
        missing.len(),
        ev.states.len(),
        missing.join("; ")
    ))
}

/// The rendered report and its completion verdict.
///
/// `text` is printed in every determined case, including an unratified
/// target. `verdict` is never `Undetermined` (an undetermined report is a
/// [`Determination::Undetermined`] with no text at all):
///
/// * `Clean` — ratified, and **every** requirement of the ratified spec is
///   RED then GREEN (no open RED).
/// * `Violation` — determined not done, with a reason naming why: the target
///   is unratified (not registered, registered but not ratified, spec changed
///   since ratification), or some requirement is unresolved (RED without
///   GREEN), unmeasured (no evidence) or PASS-only (named).
#[derive(Debug)]
pub struct Report {
    /// The human-readable report.
    pub text: String,
    /// Whether the report shows the target done (see the type docs).
    pub verdict: Verdict,
}

/// Render a human-readable report of `target`'s ratification and evidence,
/// with its completion verdict (see [`Report`]).
///
/// `Known` for a ratified target with a readable ledger on the ratified
/// history, and also for a target that is determined not ratified (the text
/// then says so, evaluates no evidence, and the verdict is `Violation`).
/// `Undetermined` — never an empty or partial report — for an unreadable
/// store/spec/ledger, an inconsistent ledger, a spec with zero requirements,
/// or the history rule (module docs, "History") violated or undecidable
/// (incl. a legacy ratification without a recorded HEAD).
///
/// The text shows the ratification head with `clean` / `dirty`, the current
/// target HEAD, and per requirement the kind, head and `clean` / `dirty` of
/// each counted record.
#[track_caller]
pub fn report(target: &ResolvedTarget, state_dir: &Path) -> Determination<Report> {
    if let Determination::Known(Observed::Unratified { reason }) =
        ratify::observe(target, state_dir)
    {
        let text = format!(
            "fde report: target {}\nratification: UNRATIFIED ({reason})\n\
             evidence: not evaluated (no ratified spec to judge it against)",
            target.root.display()
        );
        let verdict = Verdict::violation(format!(
            "report: not done: target {} is unratified ({reason})",
            target.root.display()
        ));
        return Determination::known(Report { text, verdict });
    }
    let (spec, ev) = match evaluated(target, state_dir) {
        Determination::Known(x) => x,
        Determination::Undetermined(why) => return Determination::Undetermined(why),
    };
    let history = match check_history(target, &spec, &ev) {
        Determination::Known(h) => h,
        Determination::Undetermined(why) => return Determination::Undetermined(why),
    };
    let tree = |dirty: bool| if dirty { "dirty" } else { "clean" };
    let mut out = format!(
        "fde report: target {}\nratification: ratified spec {} (fingerprint {})\n\
         ratified at HEAD {} ({})\ncurrent HEAD: {}\nrequirements: {}\n",
        target.root.display(),
        spec.path.display(),
        spec.fingerprint,
        history.ratified,
        tree(history.ratified_dirty),
        history.current,
        ev.states.len()
    );
    for ((id, st), recs) in ev.states.iter().zip(&ev.records) {
        let mut line = match st {
            ReqState::GreenAfterRed { cmds } => format!("  {id}: RED then GREEN (cmd {cmds:?})"),
            ReqState::Pass { cmds } => {
                format!("  {id}: PASS, already conforming (cmd {cmds:?})")
            }
            ReqState::Unresolved { open } => {
                format!("  {id}: RED recorded, GREEN missing (cmd {open:?})")
            }
            ReqState::Unmeasured => format!("  {id}: no evidence recorded"),
        };
        if !recs.is_empty() {
            // `check_history` has refused every counted record without a
            // head or dirty flag, so both are present here.
            let at: Vec<String> = recs
                .iter()
                .map(|r| match (&r.head, r.dirty) {
                    (Some(h), Some(d)) => format!("{} at {h} ({})", r.kind.label(), tree(d)),
                    _ => format!("{} at unknown HEAD", r.kind.label()),
                })
                .collect();
            line.push_str(&format!("; records: {}", at.join(", ")));
        }
        out.push_str(&line);
        out.push('\n');
    }
    let ids_where = |f: fn(&ReqState) -> bool| -> String {
        let ids: Vec<&str> = ev
            .states
            .iter()
            .filter(|(_, st)| f(st))
            .map(|(id, _)| id.as_str())
            .collect();
        if ids.is_empty() {
            "(none)".to_owned()
        } else {
            ids.join(", ")
        }
    };
    out.push_str(&format!(
        "unresolved (RED without GREEN): {}\n",
        ids_where(|s| matches!(s, ReqState::Unresolved { .. }))
    ));
    out.push_str(&format!(
        "unmeasured (no evidence): {}\n",
        ids_where(|s| matches!(s, ReqState::Unmeasured))
    ));
    let complete = ev.states.iter().filter(|(_, st)| st.complete()).count();
    out.push_str(&format!(
        "complete: {complete} of {} requirement(s)",
        ev.states.len()
    ));
    if ev.stale > 0 {
        out.push_str(&format!(
            "\nignored: {} record(s) from an earlier ratification (different spec fingerprint)",
            ev.stale
        ));
    }
    let not_red_green: Vec<String> = ev
        .states
        .iter()
        .filter_map(|(id, st)| match st {
            ReqState::GreenAfterRed { .. } => None,
            ReqState::Unresolved { open } => Some(format!("{id} (RED without GREEN: {open:?})")),
            ReqState::Pass { .. } => Some(format!("{id} (PASS only, never RED then GREEN)")),
            ReqState::Unmeasured => Some(format!("{id} (no evidence)")),
        })
        .collect();
    let verdict = if not_red_green.is_empty() {
        Verdict::from_findings(Vec::new())
    } else {
        Verdict::violation(format!(
            "report: not done for {}: {} of {} requirement(s) are not verified RED then GREEN: {}",
            target.root.display(),
            not_red_green.len(),
            ev.states.len(),
            not_red_green.join("; ")
        ))
    };
    Determination::known(Report { text: out, verdict })
}
