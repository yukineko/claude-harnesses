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
//!    "head": "<40 lowercase hex>", "dirty": false, "ratified_head": "<40 lowercase hex>"},
//!   ...
//! ]}
//! ```
//!
//! `head` is the target HEAD commit ([`crate::git::head`]) and `dirty` whether
//! the target work tree differed from it ([`crate::git::is_dirty`]: untracked
//! files count, ignored files do not), both observed immediately **before**
//! `CMD` ran. `ratified_head` is the ratification head `R` in force when the
//! record was written (its **evidence epoch**). A ledger written by an earlier
//! fde (`schema: 1`, records without `head` / `dirty` / `ratified_head`) is
//! still read; its records keep no `head` (and are carried over without one
//! when the ledger is next rewritten as schema 2). In schema 2 a record has
//! `head` and `dirty` together with `ratified_head`, `head` and `dirty`
//! without it (written by an fde before epochs), or none of the three
//! (carried over); anything else, or an unknown schema, makes the ledger
//! `Undetermined`.
//!
//! Records are append-only and kept in recording order. A record **counts**
//! only when its `fingerprint` equals the *current* ratified fingerprint
//! **and** its `ratified_head` equals the *current* ratification head. Every
//! other record is **stale**: kept on disk, but ignored by recording
//! preconditions, `verify` and `report` (which counts them on its `ignored:`
//! line). A spec change, and every re-ratification at a different HEAD —
//! after a reset to another commit, but also at a descendant of the old head
//! — therefore starts a new evidence epoch: the earlier evidence leaves every
//! answer and the requirements must be measured again. Re-ratifying at the
//! same HEAD keeps the epoch (and the records counting). That a new epoch
//! discards earlier evidence even at a descendant HEAD is kept as is by user
//! ruling (2026-11-04). A record that
//! matches the current fingerprint but has no `ratified_head` (written by an
//! earlier fde) cannot be placed in an epoch; no epoch is guessed for it, and
//! recording, `verify` and `report` answer `Undetermined` while one exists.
//! The file is rewritten atomically (temp file + rename,
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
//! 4. The ledger is read; unreadable, corrupt, unknown schema, internally
//!    inconsistent (see `evaluate`), or holding a current-fingerprint record
//!    without `ratified_head` is `Undetermined`.
//! 5. Preconditions over the counted (current-epoch) records only
//!    (`Violation` when unmet, without running `CMD`): GREEN needs an earlier
//!    RED for the same requirement **and the same `CMD` text**; PASS needs
//!    that the requirement has no RED at all (a requirement with a RED is
//!    closed by its GREEN, not by a PASS). RED has no precondition. A stale
//!    RED neither enables a GREEN nor blocks a PASS.
//! 6. Immediately before `CMD` runs, the target HEAD and dirty flag are
//!    observed (they become the record's `head` / `dirty`) and the step-1
//!    ancestry check is repeated against that HEAD; any failure is
//!    `Undetermined` and `CMD` does not run.
//!    `CMD` runs **confined** (spec `fde-evidence-confine`) as
//!    `sandbox-exec -p PROFILE /bin/sh -c CMD` (macOS `/usr/bin/sandbox-exec`)
//!    with cwd = the target root, stdin null, the repo-relocating `GIT_*`
//!    variables removed, `TMPDIR` set to a fresh private per-invocation
//!    directory (mode 0700, under the parent temp dir), bounded by
//!    [`CMD_TIMEOUT`]. The generated profile is `(allow default)` plus
//!    `(deny file-write*)`, re-allowing writes only to this allow-set (paths
//!    symlink-resolved, since the sandbox matches real paths): the target
//!    root, the private TMPDIR, `/dev/null`, `/dev/tty`, `$CARGO_TARGET_DIR`
//!    when set, `$CARGO_HOME/registry` and `$CARGO_HOME/git` (else
//!    `$HOME/.cargo/registry` / `$HOME/.cargo/git`), and `$RUSTUP_HOME` (else
//!    `$HOME/.rustup`). Every other write — the parent `$TMPDIR`,
//!    `/private/tmp`, `/private/var/folders`, `$HOME` — is denied; reads and
//!    network are not restricted. Before **every** `CMD` the preflight
//!    `sandbox-exec -p PROFILE /usr/bin/true` runs; a refusal — the private
//!    TMPDIR or the profile cannot be made, sandbox-exec is missing or cannot
//!    be spawned, the probe exits non-zero, or the host is not macOS — is
//!    `Undetermined` (exit 2, reason on stderr), `CMD` does not run and
//!    nothing is recorded. The non-macOS refusal is compile-checked only
//!    (`#[cfg(not(target_os = "macos"))]`); it has not been observed running
//!    on this (macOS) development host. There is no env var or flag to change
//!    or skip the confinement (the test overrides exist only under
//!    `#[cfg(test)]`). A write the sandbox denies is part of `CMD`'s
//!    environment: `CMD` simply sees the error and its exit status is judged
//!    as usual. The private TMPDIR is removed after `CMD`; a failed removal is
//!    a stderr warning and changes neither the answer nor the record.
//!    The **exit status** decides: RED needs a non-zero
//!    exit, GREEN and PASS need exit 0; the other outcome is `Violation`. A
//!    spawn failure, a signal (no exit code), a timeout, unreadable output, or
//!    exit 126/127 (the shell could not execute / find the command, so nothing
//!    was tested) is `Undetermined` and records nothing.
//! 7. Right after `CMD` exits — before its exit status is judged — the target
//!    HEAD is observed again (user ruling 2026-11-04). If it cannot be
//!    observed, or differs in any way from the step-6 HEAD (moved back, moved
//!    forward to a new descendant commit, or anything else), the answer is
//!    `Undetermined`, naming both commits and saying HEAD changed while `CMD`
//!    ran, and nothing is recorded — whatever the exit status was. Forward
//!    movement is refused too (coordinator decision under that ruling): the
//!    record's `head` would no longer describe the tree `CMD`'s result belongs
//!    to, and the history rule (`R <= H <= C`) could otherwise be satisfied by
//!    a `CMD` that commits its own fix. Only these two observations are
//!    compared: a `CMD` that moves HEAD and then restores it (commit then
//!    `reset --hard` back, checkout away and back) is not detected.
//! 8. Ratification is observed again: if it is no longer ratified with the
//!    same fingerprint and the same recorded HEAD / dirty flag, nothing is
//!    recorded (`Violation`).
//! 9. The ledger is re-read, the step-4 / step-5 checks repeated against it,
//!    the record (with `ratified_head` = the ratification head) appended, and
//!    the file written atomically. Only a successful write is `Clean`.
//!
//! # Completeness (`verify`, `report`)
//!
//! Per requirement, over the counted (current-epoch) records in order: a requirement
//! is **unresolved** while some RED `CMD` has no later GREEN with the same
//! `CMD`; otherwise it is **complete** when it has a GREEN (after its RED);
//! otherwise it is **PASS-only** when it has a PASS; otherwise it is
//! **unmeasured**. A PASS-only requirement is **not** complete (user ruling
//! 2026-11-04): `fde evidence pass` still records the PASS, but only a later
//! RED then GREEN completes the requirement (PASS → RED → GREEN is complete).
//! `verify` is `Clean` only when every requirement of the ratified spec is
//! complete; otherwise it is a `Violation` naming each incomplete requirement
//! (`(PASS only, never RED then GREEN)` for a PASS-only one).
//!
//! `report` applies the same bar (user ruling 2026-11-03: "Exit 0 only when
//! it is ratified and every requirement is verified RED→GREEN"), and its
//! `complete: N of M` line counts only GREEN after RED. The two differ only
//! for an unratified target: a determined `Violation` for `report`, while
//! `verify` answers it `Undetermined`.
//!
//! # History (`verify`, `report`)
//!
//! Before either answers, the evidence must lie on the ratified history
//! (forward history only): with `R` the ratification head, `C` the current
//! target HEAD and `H` the `head` of each counted (current-epoch) record, `R`
//! must be an ancestor of or equal to `C`, and `R <= H <= C` for every counted
//! record. Stale records are not checked (they do not count). A legacy
//! ratification (no `R`), a current-fingerprint record without
//! `ratified_head`, a counted record without `head`, a `C` that cannot be
//! observed, any "not an ancestor" answer, or any failed comparison (e.g. a
//! commit unknown to the repository) is `Undetermined`: `verify` exits 2 and
//! `report` exits 2 without text. After a reset off the ratified line, a
//! human re-ratifying at the new HEAD starts a new epoch, so the answer
//! becomes "unmeasured" (1) until evidence is recorded again.
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
    /// The ratification head `R` in force when the record was written (its
    /// evidence epoch). `None` only for a record written by an earlier fde;
    /// present only together with `head` / `dirty`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ratified_head: Option<String>,
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
/// `head` / `dirty` / `ratified_head`) is returned as schema 2 with head-less
/// records, so the next write upgrades it without inventing a head. In schema
/// 2 a record with only one of `head` / `dirty`, a `ratified_head` without
/// both of them, or a `head` / `ratified_head` that is not 40 lowercase hex,
/// is `Undetermined`.
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
                let well_formed = match (&rec.head, rec.dirty, &rec.ratified_head) {
                    (Some(h), Some(_), Some(r)) => {
                        crate::git::is_commit_id(h) && crate::git::is_commit_id(r)
                    }
                    (Some(h), Some(_), None) => crate::git::is_commit_id(h),
                    (None, None, None) => true,
                    _ => false,
                };
                if !well_formed {
                    return Determination::undetermined(format!(
                        "evidence ledger {} is corrupt: record #{i} has head {:?}, dirty {:?} and \
                         ratified_head {:?} (expected: head and dirty both present with a \
                         40-digit lowercase hex head, optionally with a 40-digit lowercase hex \
                         ratified_head; or none of the three)",
                        path.display(),
                        rec.head,
                        rec.dirty,
                        rec.ratified_head
                    ));
                }
            }
        }
        LEGACY_SCHEMA => {
            if let Some(i) = ledger
                .records
                .iter()
                .position(|r| r.head.is_some() || r.dirty.is_some() || r.ratified_head.is_some())
            {
                return Determination::undetermined(format!(
                    "evidence ledger {} has schema {LEGACY_SCHEMA}, but record #{i} carries \
                     head/dirty/ratified_head fields that schema never had",
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
    /// No counted (current-epoch) record at all.
    Unmeasured,
}

impl ReqState {
    /// Only GREEN after RED is complete (user ruling 2026-11-04: a PASS-only
    /// requirement is not complete).
    fn complete(&self) -> bool {
        matches!(self, ReqState::GreenAfterRed { .. })
    }
}

/// Why requirement `id` in state `st` is not complete, or `None` when it is
/// (shared by `verify` and `report`, so both name the same reasons).
fn incomplete_reason(id: &str, st: &ReqState) -> Option<String> {
    match st {
        ReqState::GreenAfterRed { .. } => None,
        ReqState::Unresolved { open } => Some(format!("{id} (RED without GREEN: {open:?})")),
        ReqState::Pass { .. } => Some(format!("{id} (PASS only, never RED then GREEN)")),
        ReqState::Unmeasured => Some(format!("{id} (no evidence)")),
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
    /// Counted (current-epoch) records per requirement, in ledger
    /// order; parallel to `states`.
    records: Vec<Vec<RecordAt>>,
    /// Records from an earlier ratification (another fingerprint, or another
    /// ratification head), ignored.
    stale: usize,
    /// Ledger positions of current-fingerprint records without a
    /// `ratified_head` (written by an earlier fde): their epoch is unknown, so
    /// they are neither counted nor ignored. Callers answer `Undetermined`
    /// while this is non-empty.
    unplaced: Vec<usize>,
}

/// Whether `rec` belongs to the current evidence epoch: recorded against
/// `fingerprint` under the ratification head `ratified_head`.
fn in_epoch(rec: &Record, fingerprint: &str, ratified_head: &str) -> bool {
    rec.fingerprint == fingerprint && rec.ratified_head.as_deref() == Some(ratified_head)
}

/// Evaluate `ledger` for `reqs` in the evidence epoch (`fingerprint`,
/// `ratified_head`). Records of another fingerprint or another ratification
/// head are stale (counted in `stale`); current-fingerprint records without a
/// `ratified_head` are listed in `unplaced`. A counted record that recording
/// would never have written — a requirement outside the spec, a GREEN without
/// an earlier RED of the same `CMD`, a PASS after a RED of its requirement —
/// makes the ledger inconsistent: `Undetermined`.
#[track_caller]
fn evaluate(
    reqs: &[String],
    ledger: &Ledger,
    fingerprint: &str,
    ratified_head: &str,
) -> Determination<Evaluation> {
    let mut accs: Vec<Acc> = reqs.iter().map(|_| Acc::default()).collect();
    let mut records: Vec<Vec<RecordAt>> = reqs.iter().map(|_| Vec::new()).collect();
    let mut stale = 0usize;
    let mut unplaced = Vec::new();
    for (i, rec) in ledger.records.iter().enumerate() {
        if rec.fingerprint == fingerprint && rec.ratified_head.is_none() {
            unplaced.push(i);
            continue;
        }
        if !in_epoch(rec, fingerprint, ratified_head) {
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
        unplaced,
    })
}

/// `Undetermined` when `ev` holds current-fingerprint records without a
/// `ratified_head`: no epoch is guessed for them.
#[track_caller]
fn require_placed(ev: &Evaluation) -> Determination<()> {
    if ev.unplaced.is_empty() {
        return Determination::known(());
    }
    let list: Vec<String> = ev.unplaced.iter().map(|i| format!("#{i}")).collect();
    Determination::undetermined(format!(
        "evidence record(s) {} match the ratified spec fingerprint but were written by an \
         earlier fde without the ratification HEAD (ratified_head), so it cannot be decided \
         whether they belong to the current ratification; no epoch is guessed. A human must \
         decide what to do with the ledger (e.g. drop the target state and re-begin)",
        list.join(", ")
    ))
}

/// Step-5 precondition for recording `kind` of `cmd` for `req`. `None` = met.
fn precondition_unmet(
    kind: Kind,
    req: &str,
    cmd: &str,
    ledger: &Ledger,
    fingerprint: &str,
    ratified_head: &str,
) -> Option<String> {
    let mut current = ledger
        .records
        .iter()
        .filter(|r| in_epoch(r, fingerprint, ratified_head) && r.req == req);
    match kind {
        Kind::Red => None,
        Kind::Green => {
            if current.any(|r| r.kind == Kind::Red && r.cmd == cmd) {
                None
            } else {
                Some(format!(
                    "refused: no RED is recorded for requirement {req} with the command {cmd:?} \
                     under the current ratification; a GREEN must follow a recorded RED of the \
                     same requirement and command"
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
/// no current-fingerprint record without `ratified_head`, `R <= C` and
/// `R <= H <= C` for every counted record. Any violation or failure to
/// compare is `Undetermined`.
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
    if let Determination::Undetermined(why) = require_placed(ev) {
        return Determination::Undetermined(why);
    }
    let c = match head_on_ratified_line(&r, &target.root) {
        Determination::Known(c) => c,
        Determination::Undetermined(why) => return Determination::Undetermined(why),
    };
    let mut checked: BTreeSet<String> = BTreeSet::new();
    for ((id, _), recs) in ev.states.iter().zip(&ev.records) {
        for rec in recs {
            let Some(h) = &rec.head else {
                return Determination::undetermined(format!(
                    "evidence record #{} ({} for {id}) carries no target HEAD, so it cannot be \
                     placed on the ratified history",
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

/// The macOS sandbox launcher every evidence `CMD` runs under.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

/// Upper bound on the preflight probe (`sandbox-exec -p PROFILE /usr/bin/true`).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const PROBE_TIMEOUT: Duration = Duration::from_secs(60);

/// One writable entry of the confinement allow-set.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Writable {
    /// Exactly this path (`(literal "...")`).
    Literal(PathBuf),
    /// This path and everything below it (`(subpath "...")`).
    Subpath(PathBuf),
}

/// An environment path variable: unset and empty are both "not present". A
/// relative value is taken relative to the target root (the `CMD`'s cwd,
/// where cargo would resolve it too). The result is canonicalized leniently
/// (the longest existing prefix symlink-resolved) because the sandbox matches
/// real paths (`/tmp` and `/var` are symlinks to `/private/...` on macOS).
#[track_caller]
fn env_path(
    env: &dyn Fn(&str) -> Option<std::ffi::OsString>,
    key: &str,
    root: &Path,
) -> Determination<Option<PathBuf>> {
    let Some(v) = env(key).filter(|v| !v.is_empty()) else {
        return Determination::known(None);
    };
    let p = PathBuf::from(v);
    let abs = if p.is_absolute() { p } else { root.join(p) };
    match crate::gate::canonical_lenient(&abs) {
        Determination::Known(c) => Determination::known(Some(c)),
        Determination::Undetermined(why) => Determination::undetermined(format!(
            "${key} ({}) cannot be resolved for the evidence CMD write allow-set: {}",
            abs.display(),
            why.as_str()
        )),
    }
}

/// The write allow-set for one evidence `CMD` (spec fde-evidence-confine R1):
/// the target root, the private TMPDIR, `/dev/null`, `/dev/tty`,
/// `$CARGO_TARGET_DIR` when set, `$CARGO_HOME/{registry,git}` (else
/// `$HOME/.cargo/{registry,git}`) and `$RUSTUP_HOME` (else `$HOME/.rustup`).
/// Entries whose variables are absent are left out (fewer writes, never
/// more). A path that cannot be resolved is `Undetermined`.
#[track_caller]
fn allow_set(
    root: &Path,
    private_tmp: &Path,
    env: &dyn Fn(&str) -> Option<std::ffi::OsString>,
) -> Determination<Vec<Writable>> {
    let canon = |p: &Path, what: &str| -> Determination<PathBuf> {
        match p.canonicalize() {
            Ok(c) => Determination::known(c),
            Err(e) => Determination::undetermined(format!(
                "the {what} {} cannot be canonicalized for the evidence CMD write allow-set: {e}",
                p.display()
            )),
        }
    };
    let mut set = Vec::new();
    for (p, what) in [(root, "target root"), (private_tmp, "private TMPDIR")] {
        match canon(p, what) {
            Determination::Known(c) => set.push(Writable::Subpath(c)),
            Determination::Undetermined(why) => return Determination::Undetermined(why),
        }
    }
    set.push(Writable::Literal(PathBuf::from("/dev/null")));
    set.push(Writable::Literal(PathBuf::from("/dev/tty")));
    let var = |key: &str| env_path(env, key, root);
    let home = match var("HOME") {
        Determination::Known(h) => h,
        Determination::Undetermined(why) => return Determination::Undetermined(why),
    };
    match var("CARGO_TARGET_DIR") {
        Determination::Known(Some(t)) => set.push(Writable::Subpath(t)),
        Determination::Known(None) => {}
        Determination::Undetermined(why) => return Determination::Undetermined(why),
    }
    let cargo_home = match var("CARGO_HOME") {
        Determination::Known(Some(c)) => Some(c),
        Determination::Known(None) => home.as_ref().map(|h| h.join(".cargo")),
        Determination::Undetermined(why) => return Determination::Undetermined(why),
    };
    if let Some(c) = cargo_home {
        set.push(Writable::Subpath(c.join("registry")));
        set.push(Writable::Subpath(c.join("git")));
    }
    match var("RUSTUP_HOME") {
        Determination::Known(Some(r)) => set.push(Writable::Subpath(r)),
        Determination::Known(None) => {
            if let Some(h) = &home {
                set.push(Writable::Subpath(h.join(".rustup")));
            }
        }
        Determination::Undetermined(why) => return Determination::Undetermined(why),
    }
    Determination::known(set)
}

/// `p` as an SBPL string literal (`"` and `\` escaped). A path that is not
/// UTF-8 or holds a control character is `Undetermined`: it cannot be written
/// into the profile without guessing.
#[track_caller]
fn sbpl_string(p: &Path) -> Determination<String> {
    let Some(s) = p.to_str() else {
        return Determination::undetermined(format!(
            "path {} is not UTF-8 and cannot be written into the sandbox profile",
            p.display()
        ));
    };
    if s.chars().any(char::is_control) {
        return Determination::undetermined(format!(
            "path {s:?} holds a control character and cannot be written into the sandbox profile"
        ));
    }
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        if ch == '"' || ch == '\\' {
            out.push('\\');
        }
        out.push(ch);
    }
    out.push('"');
    Determination::known(out)
}

/// The sandbox profile for `set`: everything allowed except file writes,
/// which are denied and then re-allowed for exactly the entries of `set`
/// (later SBPL rules win). Reads and network are not restricted.
#[track_caller]
fn profile(set: &[Writable]) -> Determination<String> {
    let mut p = String::from("(version 1)\n(allow default)\n(deny file-write*)\n");
    for w in set {
        let (filter, path) = match w {
            Writable::Literal(path) => ("literal", path),
            Writable::Subpath(path) => ("subpath", path),
        };
        match sbpl_string(path) {
            Determination::Known(s) => {
                p.push_str(&format!("(allow file-write* ({filter} {s}))\n"));
            }
            Determination::Undetermined(why) => return Determination::Undetermined(why),
        }
    }
    Determination::known(p)
}

/// The sandbox-exec binary to run. Tests may point it elsewhere through
/// [`seam`]; a release build has no override (spec R8).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn sandbox_exec_path() -> PathBuf {
    #[cfg(test)]
    if let Some(p) = seam::SANDBOX_EXEC.with(|c| c.borrow().clone()) {
        return p;
    }
    PathBuf::from(SANDBOX_EXEC)
}

/// The profile to load: the generated one. Tests may replace it through
/// [`seam`]; a release build has no override (spec R8).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn effective_profile(generated: String) -> String {
    #[cfg(test)]
    if let Some(p) = seam::PROFILE.with(|c| c.borrow().clone()) {
        return p;
    }
    generated
}

/// Print a warning on stderr (a failed stderr write changes nothing).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn warn(msg: &str) {
    use std::io::Write as _;
    #[cfg(test)]
    seam::WARNINGS.with(|w| w.borrow_mut().push(msg.to_owned()));
    let _ = writeln!(std::io::stderr(), "fde: warning: {msg}");
}

/// Create the private per-invocation TMPDIR (mode 0700) under the parent
/// temp dir. Any failure is `Undetermined`.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
#[track_caller]
fn make_private_tmp() -> Determination<PathBuf> {
    use std::os::unix::fs::DirBuilderExt as _;
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let parent = std::env::temp_dir();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut last = None;
    for _ in 0..16 {
        let dir = parent.join(format!(
            "fde-evidence-{}-{nanos}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
            Ok(()) => return Determination::known(dir),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => last = Some(e),
            Err(e) => {
                return Determination::undetermined(format!(
                    "the private TMPDIR {} for the evidence CMD cannot be created: {e}",
                    dir.display()
                ))
            }
        }
    }
    Determination::undetermined(format!(
        "no fresh private TMPDIR for the evidence CMD could be created under {}: {}",
        parent.display(),
        last.map(|e| e.to_string()).unwrap_or_default()
    ))
}

/// Remove the private TMPDIR. Tests may make this fail through [`seam`].
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn remove_private_tmp(dir: &Path) -> std::io::Result<()> {
    #[cfg(test)]
    if seam::FAIL_REMOVE.with(std::cell::Cell::get) {
        return Err(std::io::Error::other(
            "removal failure injected by a unit test",
        ));
    }
    std::fs::remove_dir_all(dir)
}

/// Run `cmd` at the target root, confined (spec fde-evidence-confine): a
/// private TMPDIR is created, the write allow-set profile generated, the
/// preflight probe `sandbox-exec -p PROFILE /usr/bin/true` run, and only
/// when it exits 0 `cmd` runs as `sandbox-exec -p PROFILE /bin/sh -c CMD`
/// with `TMPDIR` = the private dir. The private dir is removed afterwards;
/// a failed removal is a stderr warning and changes nothing else.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
#[track_caller]
fn run_confined(
    cmd: &str,
    root: &Path,
    env: &dyn Fn(&str) -> Option<std::ffi::OsString>,
) -> Determination<boundary::CommandOutput> {
    let private = match make_private_tmp() {
        Determination::Known(d) => d,
        Determination::Undetermined(why) => return Determination::Undetermined(why),
    };
    let out = run_in_sandbox(cmd, root, &private, env);
    if let Err(e) = remove_private_tmp(&private) {
        warn(&format!(
            "could not remove the private TMPDIR {} of the evidence CMD: {e}",
            private.display()
        ));
    }
    out
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
#[track_caller]
fn run_in_sandbox(
    cmd: &str,
    root: &Path,
    private: &Path,
    env: &dyn Fn(&str) -> Option<std::ffi::OsString>,
) -> Determination<boundary::CommandOutput> {
    let set = match allow_set(root, private, env) {
        Determination::Known(s) => s,
        Determination::Undetermined(why) => return Determination::Undetermined(why),
    };
    let prof = match profile(&set) {
        Determination::Known(p) => effective_profile(p),
        Determination::Undetermined(why) => return Determination::Undetermined(why),
    };
    let exec = sandbox_exec_path();
    let build = |c: &mut Command| {
        harness_core::git_env::scrub(c);
        c.env("TMPDIR", private)
            .current_dir(root)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
    };

    // Preflight, before every CMD: the profile must load under this
    // sandbox-exec, or the CMD does not run at all.
    let mut probe = Command::new(&exec);
    probe.arg("-p").arg(&prof).arg("/usr/bin/true");
    build(&mut probe);
    match boundary::run_with_timeout(&mut probe, PROBE_TIMEOUT) {
        Determination::Known(o) if o.code() == 0 => {}
        Determination::Known(o) => {
            let tail: String = o.stderr().chars().take(400).collect();
            return Determination::undetermined(format!(
                "the confinement preflight `{} -p <profile> /usr/bin/true` exited {}, so the \
                 evidence CMD {cmd:?} was not run and nothing is recorded. stderr: {}",
                exec.display(),
                o.code(),
                if tail.is_empty() { "<empty>" } else { &tail }
            ));
        }
        Determination::Undetermined(why) => {
            return Determination::undetermined(format!(
                "the confinement preflight could not run sandbox-exec ({}), so the evidence CMD \
                 {cmd:?} was not run and nothing is recorded: {}",
                exec.display(),
                why.as_str()
            ))
        }
    }

    let mut c = Command::new(&exec);
    c.arg("-p").arg(&prof).arg("/bin/sh").arg("-c").arg(cmd);
    build(&mut c);
    boundary::run_with_timeout(&mut c, CMD_TIMEOUT)
}

#[cfg(target_os = "macos")]
#[track_caller]
fn confined(cmd: &str, root: &Path) -> Determination<boundary::CommandOutput> {
    run_confined(cmd, root, &|k| std::env::var_os(k))
}

/// Not macOS: there is no sandbox-exec to confine `cmd`, so it is not run.
/// This path is compile-checked only; it has not been observed running on
/// the macOS development host.
#[cfg(not(target_os = "macos"))]
#[track_caller]
fn confined(cmd: &str, _root: &Path) -> Determination<boundary::CommandOutput> {
    Determination::undetermined(format!(
        "the evidence CMD {cmd:?} was not run: its write confinement needs macOS \
         sandbox-exec, and this host is not macOS"
    ))
}

/// Run `cmd` at the target root under the write confinement (module docs,
/// step 6). `Known(output)` only for a process that ran to completion with an
/// exit code other than 126/127. A refusal by the confinement — the private
/// TMPDIR or the profile cannot be made, sandbox-exec is missing or cannot be
/// spawned, the preflight probe exits non-zero, or the host is not macOS —
/// is `Undetermined` and `cmd` is not run.
#[track_caller]
fn run_cmd(cmd: &str, root: &Path) -> Determination<boundary::CommandOutput> {
    let out = match confined(cmd, root) {
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
/// * `Undetermined` — store/spec/ledger unreadable or corrupt, a
///   current-fingerprint record without `ratified_head`, a legacy
///   ratification without a recorded HEAD, a target HEAD that cannot be
///   observed or is not a descendant of (or equal to) the ratification head,
///   a dirty flag that cannot be observed, the write confinement refused
///   (module docs, step 6: no sandbox-exec, the preflight probe failed, not
///   macOS), `cmd` could not be run to an exit code, or the target HEAD changed while `cmd` ran (or cannot be observed
///   after it; checked before the exit status). Nothing is recorded.
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
    match evaluate(&reqs, &ledger, &spec.fingerprint, &ratified_head) {
        Determination::Known(ev) => {
            if let Determination::Undetermined(why) = require_placed(&ev) {
                return why.into_verdict();
            }
        }
        Determination::Undetermined(why) => return why.into_verdict(),
    }
    if let Some(why) =
        precondition_unmet(kind, req, cmd, &ledger, &spec.fingerprint, &ratified_head)
    {
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
    // The record's `head` must describe the tree the CMD's result belongs
    // to: any HEAD movement while CMD ran (back, forward, or unobservable) is
    // refused before the exit status is looked at (user ruling 2026-11-04;
    // forward movement too, see the module docs, step 7).
    match crate::git::head(&target.root) {
        Determination::Known(after) if after == head => {}
        Determination::Known(after) => {
            return Verdict::undetermined(format!(
                "{} not recorded for {req}: the target HEAD changed while the command ran \
                 (before {head}, after {after}); the result cannot be placed on one commit, so \
                 nothing is recorded. Run a CMD that does not move HEAD",
                kind.label()
            ))
        }
        Determination::Undetermined(why) => {
            return Verdict::undetermined(format!(
                "{} not recorded for {req}: the target HEAD cannot be observed after the command \
                 ran (before it was {head}), so it cannot be shown that HEAD did not change while \
                 the command ran: {}",
                kind.label(),
                why.as_str()
            ))
        }
    }
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
    match evaluate(&reqs, &ledger, &spec.fingerprint, &ratified_head) {
        Determination::Known(ev) => {
            if let Determination::Undetermined(why) = require_placed(&ev) {
                return why.into_verdict();
            }
        }
        Determination::Undetermined(why) => return why.into_verdict(),
    }
    if let Some(why) =
        precondition_unmet(kind, req, cmd, &ledger, &spec.fingerprint, &ratified_head)
    {
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
        ratified_head: Some(ratified_head),
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
    let (ratified_head, _) = match ratification_head(&spec) {
        Determination::Known(x) => x,
        Determination::Undetermined(why) => return Determination::Undetermined(why),
    };
    let ledger = match read_ledger(&ledger_path(target, state_dir)) {
        Determination::Known(l) => l,
        Determination::Undetermined(why) => return Determination::Undetermined(why),
    };
    match evaluate(&reqs, &ledger, &spec.fingerprint, &ratified_head) {
        Determination::Known(ev) => Determination::known((spec, ev)),
        Determination::Undetermined(why) => Determination::Undetermined(why),
    }
}

/// Check `target`'s ledger against its ratified spec.
///
/// * `Clean` — every requirement is complete (GREEN after RED; user ruling
///   2026-11-04: PASS-only is not complete).
/// * `Violation` — some requirement is unresolved, unmeasured or PASS-only
///   (named).
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
        .filter_map(|(id, st)| incomplete_reason(id, st))
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
            "\nignored: {} record(s) from an earlier ratification (different spec fingerprint \
             or ratification HEAD)",
            ev.stale
        ));
    }
    let not_red_green: Vec<String> = ev
        .states
        .iter()
        .filter_map(|(id, st)| incomplete_reason(id, st))
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

/// Test-only overrides of the confinement (spec R8: none of this exists in a
/// release build, so no env var or flag can change or skip the confinement).
#[cfg(test)]
mod seam {
    use std::cell::{Cell, RefCell};
    use std::path::PathBuf;

    thread_local! {
        /// Replaces [`super::SANDBOX_EXEC`] for this thread.
        pub static SANDBOX_EXEC: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
        /// Replaces the generated profile for this thread.
        pub static PROFILE: RefCell<Option<String>> = const { RefCell::new(None) };
        /// Makes the private TMPDIR removal fail for this thread.
        pub static FAIL_REMOVE: Cell<bool> = const { Cell::new(false) };
        /// Every warning printed on this thread.
        pub static WARNINGS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    }
}

// Test assertions panic by design; production keeps the workspace deny.
#[cfg(test)]
#[allow(clippy::panic)]
mod confine_tests {
    use super::*;
    use std::ffi::OsString;
    use std::fs;
    use tempfile::TempDir;

    fn git(dir: &Path, args: &[&str]) {
        let mut c = Command::new("git");
        harness_core::git_env::scrub(&mut c);
        let out = c.current_dir(dir).args(args).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {out:?}");
    }

    /// A ratified target with requirement R1, its state dir, and the temp
    /// dirs keeping them alive.
    struct Fixture {
        _dirs: Vec<TempDir>,
        target: ResolvedTarget,
        state: PathBuf,
    }

    fn tmp(prefix: &str) -> (TempDir, PathBuf) {
        let d = tempfile::Builder::new().prefix(prefix).tempdir().unwrap();
        let p = d.path().canonicalize().unwrap();
        (d, p)
    }

    fn ratified() -> Fixture {
        let (rd, root) = tmp("fde-unit-target-");
        git(&root, &["init", "-q"]);
        git(&root, &["config", "user.name", "fde-test"]);
        git(&root, &["config", "user.email", "fde-test@example.invalid"]);
        git(&root, &["config", "commit.gpgsign", "false"]);
        fs::write(root.join("README.md"), "fixture\n").unwrap();
        git(&root, &["add", "README.md"]);
        git(&root, &["commit", "-q", "-m", "initial"]);
        let (sd, state) = tmp("fde-unit-state-");
        let (pd, spec_dir) = tmp("fde-unit-spec-");
        let spec = spec_dir.join("spec.toml");
        fs::write(
            &spec,
            "[spec]\nid = \"unit\"\n\n[[requirement]]\nid = \"R1\"\nstatement = \"s\"\n",
        )
        .unwrap();
        let target = match crate::target::resolve(Some(&root), &root) {
            Determination::Known(t) => t,
            Determination::Undetermined(why) => panic!("resolve: {}", why.as_str()),
        };
        assert!(matches!(
            ratify::begin(&target, &spec, &state),
            Verdict::Clean(_)
        ));
        assert!(matches!(
            ratify::ratify(&target, Some(&spec), &state),
            Verdict::Clean(_)
        ));
        Fixture {
            _dirs: vec![rd, sd, pd],
            target,
            state,
        }
    }

    impl Fixture {
        fn root(&self) -> &Path {
            &self.target.root
        }
        fn ledger(&self) -> Option<Vec<u8>> {
            fs::read(ledger_path(&self.target, &self.state)).ok()
        }
        fn red(&self, cmd: &str) -> Verdict {
            record(Kind::Red, "R1", cmd, &self.target, &self.state)
        }
    }

    fn reason(v: &Verdict) -> String {
        match v {
            Verdict::Undetermined(why) => why.as_str().to_owned(),
            other => panic!("expected Undetermined, got {other:?}"),
        }
    }

    /// A wrapper around the real sandbox-exec that logs, per invocation, the
    /// program it is asked to run (`$3`, after `-p PROFILE`), then execs it.
    fn logging_sandbox_exec(dir: &Path) -> (PathBuf, PathBuf) {
        use std::os::unix::fs::PermissionsExt as _;
        let log = dir.join("sandbox-exec.log");
        let script = dir.join("sandbox-exec-wrapper");
        let body = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$3\" >> '{}'\nexec {SANDBOX_EXEC} \"$@\"\n",
            log.display()
        );
        fs::write(&script, body).unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        (script, log)
    }

    fn reset_seam() {
        seam::SANDBOX_EXEC.with(|c| *c.borrow_mut() = None);
        seam::PROFILE.with(|c| *c.borrow_mut() = None);
        seam::FAIL_REMOVE.with(|c| c.set(false));
        seam::WARNINGS.with(|w| w.borrow_mut().clear());
    }

    /// R3: sandbox-exec missing -> Undetermined (exit 2), CMD not run,
    /// reason names sandbox-exec, ledger byte-identical.
    #[test]
    fn missing_sandbox_exec_is_undetermined_and_records_nothing() {
        reset_seam();
        let f = ratified();
        assert!(matches!(f.red("false"), Verdict::Clean(_)));
        let before = f.ledger();
        assert!(before.is_some());
        seam::SANDBOX_EXEC.with(|c| *c.borrow_mut() = Some("/nonexistent/sandbox-exec".into()));
        let side = f.root().join("side-effect");
        let v = f.red(&format!("touch {} && false", side.display()));
        reset_seam();
        let why = reason(&v);
        assert!(why.contains("sandbox-exec"), "{why}");
        assert!(!side.exists(), "CMD ran without sandbox-exec");
        assert_eq!(f.ledger(), before, "ledger changed after a refusal");
        assert_eq!(crate::verdict_exit(v), 2);
    }

    /// R3: an invalid profile makes the probe exit 65; the CMD is not run.
    #[test]
    fn probe_failure_exit_65_is_undetermined_and_cmd_not_run() {
        reset_seam();
        let f = ratified();
        let (_d, dir) = tmp("fde-unit-wrap-");
        let (wrapper, log) = logging_sandbox_exec(&dir);
        let before = f.ledger();
        seam::SANDBOX_EXEC.with(|c| *c.borrow_mut() = Some(wrapper));
        seam::PROFILE.with(|c| *c.borrow_mut() = Some("(version 1)(no-such-rule".into()));
        let side = f.root().join("side-effect");
        let v = f.red(&format!("touch {} && false", side.display()));
        reset_seam();
        let why = reason(&v);
        assert!(why.contains("exited 65"), "{why}");
        assert!(!side.exists());
        let calls = fs::read_to_string(&log).unwrap();
        assert_eq!(calls, "/usr/bin/true\n", "only the probe may run");
        assert_eq!(f.ledger(), before);
        assert_eq!(crate::verdict_exit(v), 2);
    }

    /// R3: the probe runs before every CMD, not once per process.
    #[test]
    fn probe_runs_before_every_cmd() {
        reset_seam();
        let f = ratified();
        let (_d, dir) = tmp("fde-unit-wrap-");
        let (wrapper, log) = logging_sandbox_exec(&dir);
        seam::SANDBOX_EXEC.with(|c| *c.borrow_mut() = Some(wrapper));
        let a = f.red("false");
        let b = f.red("exit 3");
        reset_seam();
        assert!(matches!(a, Verdict::Clean(_)), "{a:?}");
        assert!(matches!(b, Verdict::Clean(_)), "{b:?}");
        let calls = fs::read_to_string(&log).unwrap();
        assert_eq!(calls, "/usr/bin/true\n/bin/sh\n/usr/bin/true\n/bin/sh\n");
    }

    /// R7: a failed TMPDIR removal warns (naming the dir) and changes neither
    /// the verdict nor the record.
    #[test]
    fn tmpdir_removal_failure_only_warns() {
        reset_seam();
        let f = ratified();
        let ok = f.red("false");
        assert!(matches!(ok, Verdict::Clean(_)));
        assert!(seam::WARNINGS.with(|w| w.borrow().is_empty()));
        seam::FAIL_REMOVE.with(|c| c.set(true));
        let failed = f.red("false");
        seam::FAIL_REMOVE.with(|c| c.set(false));
        let warnings = seam::WARNINGS.with(|w| w.borrow().clone());
        reset_seam();
        assert!(matches!(failed, Verdict::Clean(_)), "{failed:?}");
        assert_eq!(crate::verdict_exit(ok), crate::verdict_exit(failed));
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        let w = &warnings[0];
        let dir = w
            .split_whitespace()
            .find(|t| t.contains("fde-evidence-"))
            .unwrap_or_else(|| panic!("warning names no dir: {w}"));
        let dir = PathBuf::from(dir);
        assert!(dir.is_dir(), "the dir the warning names: {w}");
        fs::remove_dir_all(&dir).unwrap();
        let ledger: Ledger = serde_json::from_slice(&f.ledger().unwrap()).unwrap();
        assert_eq!(ledger.records.len(), 2);
        assert_eq!(ledger.records[0], ledger.records[1]);
    }

    fn env_of(pairs: Vec<(&'static str, PathBuf)>) -> impl Fn(&str) -> Option<OsString> {
        move |k| {
            pairs
                .iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| v.clone().into_os_string())
        }
    }

    /// R1/R2: deny file-write*, one allow per present entry, nothing else.
    #[test]
    fn profile_has_exactly_the_allow_set() {
        let (_a, root) = tmp("fde-unit-root-");
        let (_b, priv_tmp) = tmp("fde-unit-priv-");
        let (_c, ch) = tmp("fde-unit-cargo-");
        let (_d, rh) = tmp("fde-unit-rustup-");
        let (_e, td) = tmp("fde-unit-target-dir-");
        let (_f, home) = tmp("fde-unit-home-");
        let env = env_of(vec![
            ("HOME", home.clone()),
            ("CARGO_HOME", ch.clone()),
            ("RUSTUP_HOME", rh.clone()),
            ("CARGO_TARGET_DIR", td.clone()),
        ]);
        let set = match allow_set(&root, &priv_tmp, &env) {
            Determination::Known(s) => s,
            Determination::Undetermined(why) => panic!("{}", why.as_str()),
        };
        let p = match profile(&set) {
            Determination::Known(p) => p,
            Determination::Undetermined(why) => panic!("{}", why.as_str()),
        };
        let mut want = vec![
            format!("(allow file-write* (subpath \"{}\"))", root.display()),
            format!("(allow file-write* (subpath \"{}\"))", priv_tmp.display()),
            "(allow file-write* (literal \"/dev/null\"))".to_owned(),
            "(allow file-write* (literal \"/dev/tty\"))".to_owned(),
            format!("(allow file-write* (subpath \"{}\"))", td.display()),
            format!(
                "(allow file-write* (subpath \"{}/registry\"))",
                ch.display()
            ),
            format!("(allow file-write* (subpath \"{}/git\"))", ch.display()),
            format!("(allow file-write* (subpath \"{}\"))", rh.display()),
        ];
        let mut got: Vec<String> = p
            .lines()
            .filter(|l| l.starts_with("(allow file-write*"))
            .map(str::to_owned)
            .collect();
        want.sort();
        got.sort();
        assert_eq!(got, want, "{p}");
        let denies: Vec<&str> = p.lines().filter(|l| l.contains("deny")).collect();
        assert_eq!(denies, ["(deny file-write*)"], "{p}");
        assert!(!p.contains(&home.display().to_string()), "{p}");

        // Without CARGO_HOME / RUSTUP_HOME / CARGO_TARGET_DIR: the HOME fallbacks.
        let env = env_of(vec![("HOME", home.clone())]);
        let Determination::Known(set) = allow_set(&root, &priv_tmp, &env) else {
            panic!("allow_set");
        };
        assert_eq!(
            set[4..],
            [
                Writable::Subpath(home.join(".cargo/registry")),
                Writable::Subpath(home.join(".cargo/git")),
                Writable::Subpath(home.join(".rustup")),
            ]
        );
        // No HOME either: only the four fixed entries.
        let env = env_of(vec![]);
        let Determination::Known(set) = allow_set(&root, &priv_tmp, &env) else {
            panic!("allow_set");
        };
        assert_eq!(set.len(), 4);
    }

    #[test]
    fn sbpl_strings_escape_quote_and_backslash() {
        let Determination::Known(s) = sbpl_string(Path::new("/a\"b\\c")) else {
            panic!("sbpl_string");
        };
        assert_eq!(s, "\"/a\\\"b\\\\c\"");
        assert!(matches!(
            sbpl_string(Path::new("/a\nb")),
            Determination::Undetermined(_)
        ));
    }

    /// R1 under the real sandbox: cargo/rustup dirs and CARGO_TARGET_DIR are
    /// writable, $CARGO_HOME/bin, $HOME and the parent TMPDIR are not.
    #[test]
    fn real_sandbox_allows_exactly_the_cargo_dirs() {
        let (_a, root) = tmp("fde-unit-root-");
        let (_c, ch) = tmp("fde-unit-cargo-");
        let (_d, rh) = tmp("fde-unit-rustup-");
        let (_e, td) = tmp("fde-unit-target-dir-");
        for d in ["registry", "git", "bin"] {
            fs::create_dir_all(ch.join(d)).unwrap();
        }
        let env = env_of(vec![
            ("CARGO_HOME", ch.clone()),
            ("RUSTUP_HOME", rh.clone()),
            ("CARGO_TARGET_DIR", td.clone()),
        ]);
        let run = |cmd: &str| match run_confined(cmd, &root, &env) {
            Determination::Known(o) => o.code(),
            Determination::Undetermined(why) => panic!("{}", why.as_str()),
        };
        let ok = format!(
            "touch {0}/registry/x {0}/git/x {1}/x {2}/x in-root && echo x > /dev/null",
            ch.display(),
            rh.display(),
            td.display()
        );
        assert_eq!(run(&ok), 0);
        assert!(root.join("in-root").exists());
        assert_ne!(run(&format!("touch {}/bin/x", ch.display())), 0);
        assert!(!ch.join("bin/x").exists());

        let (_h, home) = tmp("fde-unit-home-");
        fs::create_dir_all(home.join(".cargo/registry")).unwrap();
        fs::create_dir_all(home.join(".cargo/bin")).unwrap();
        let env = env_of(vec![("HOME", home.clone())]);
        let run = |cmd: &str| match run_confined(cmd, &root, &env) {
            Determination::Known(o) => o.code(),
            Determination::Undetermined(why) => panic!("{}", why.as_str()),
        };
        assert_eq!(
            run(&format!("touch {}/.cargo/registry/x", home.display())),
            0
        );
        assert_ne!(run(&format!("touch {}/.cargo/bin/x", home.display())), 0);
        assert_ne!(run(&format!("touch {}/x", home.display())), 0);
        assert!(!home.join("x").exists());

        let parent = std::env::temp_dir().canonicalize().unwrap();
        let escaped = parent.join(format!("fde-unit-escape-{}", std::process::id()));
        assert_ne!(run(&format!("touch {}", escaped.display())), 0);
        assert!(!escaped.exists());
    }
}
