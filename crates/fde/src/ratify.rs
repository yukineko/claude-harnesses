//! Ratification store: a spec registered and ratified for a target, kept in
//! the state directory (never inside the target, never inside the spec), and
//! identified by a content fingerprint.
//!
//! # State layout
//!
//! Per target, under `<state_dir>/targets/<state_key>/` (see
//! [`crate::state::target_dir`]):
//!
//! * `registration.json` — `{schema, root, spec}`: written by [`begin`]. `spec`
//!   is the canonical absolute path of the draft spec, which lives outside the
//!   target.
//! * `ratification.json` — `{schema: 2, spec, fingerprint, head, dirty}`:
//!   written by [`ratify`]. `fingerprint` is the lowercase hex SHA-256 of the
//!   spec file bytes at ratification time; `spec` repeats the registered spec
//!   path so a ratification left over from a different registration never
//!   counts. `head` is the target HEAD commit at ratification time (40
//!   lowercase hex, [`crate::git::head`]) and `dirty` whether the target work
//!   tree differed from it then ([`crate::git::is_dirty`]: untracked files
//!   count, ignored files do not). When either cannot be determined (e.g. a
//!   repository without commits), nothing is written and `ratify` is
//!   `Undetermined`.
//!
//! [`forget`] (`fde forget`, human-only — the gate denies it to the model)
//! removes the whole per-target directory, evidence included, without parsing
//! anything in it; it is the only way out for a corrupt registration and for
//! a registered target whose root was deleted ("gone"). [`registered_at`]
//! reads the one registration recorded for an exact root (used by
//! `fde status` to recognise a gone root).
//!
//! Both files are written atomically (temp file + rename). [`begin`] removes
//! any previous `ratification.json` *before* writing the registration, so a
//! (re-)registered target is unratified even if the registration write fails
//! afterwards.
//!
//! A ratification written by an earlier fde (`schema: 1`, `{schema, spec,
//! fingerprint}`, no `head` / `dirty`) is still read: [`status`] answers it
//! exactly as before, and [`observe`] returns it with
//! [`Provenance::Legacy`]. Every evidence operation (recording, `verify`,
//! `report`) refuses a legacy ratification as `Undetermined` and asks the
//! human to re-ratify, because its HEAD is unknown.
//!
//! # Answers
//!
//! Every IO error and every parse error (including an unknown `schema`, a
//! schema-2 ratification without a well-formed `head` / `dirty`, a schema-1
//! one that carries them, a registration whose `root` is not the target, or a
//! malformed fingerprint) is `Undetermined` — never `Ratified`, never
//! `Unratified`.

use std::path::{Path, PathBuf};

use harness_core::verdict::{Determination, Verdict};
use sha2::{Digest, Sha256};

use crate::state;
use crate::target::ResolvedTarget;

/// Schema version of `registration.json`.
const SCHEMA: u32 = 1;
/// Schema version of `ratification.json` as written now (with `head` /
/// `dirty`).
const RATIFICATION_SCHEMA: u32 = 2;
/// The earlier `ratification.json` schema (no `head` / `dirty`), read as
/// [`Provenance::Legacy`].
const LEGACY_RATIFICATION_SCHEMA: u32 = 1;
const REGISTRATION_FILE: &str = "registration.json";
const RATIFICATION_FILE: &str = "ratification.json";

/// Ratification state of a target, as observed.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub enum Status {
    /// A ratified spec is recorded for the target and its fingerprint matches
    /// the current spec bytes.
    Ratified {
        /// Content fingerprint of the ratified spec.
        fingerprint: String,
    },
    /// Determined not ratified: the target is not registered, is registered
    /// but not ratified, or the spec changed since ratification.
    Unratified {
        /// Why the target is not ratified (for stderr).
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Registration {
    schema: u32,
    root: PathBuf,
    spec: PathBuf,
}

/// `ratification.json` of either schema: `head` / `dirty` are present in
/// schema 2 and absent in schema 1 (checked by [`read_ratification`]).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Ratification {
    schema: u32,
    spec: PathBuf,
    fingerprint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    head: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    dirty: Option<bool>,
}

/// The target commit a ratification was taken at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Provenance {
    /// Recorded at ratification time (schema 2).
    Recorded {
        /// Target HEAD commit (40 lowercase hex).
        head: String,
        /// Whether the target work tree differed from `head` then.
        dirty: bool,
    },
    /// A schema-1 ratification written before HEAD was recorded: its commit is
    /// unknown, so no evidence can be placed on the ratified history.
    Legacy,
}

/// Lowercase hex SHA-256 of `bytes`.
pub fn fingerprint(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut hex = String::with_capacity(digest.len() * 2);
    for b in digest {
        hex.push_str(&format!("{b:02x}"));
    }
    hex
}

/// `Clean`: the check/write ran to completion with nothing to refuse. Only
/// called on the observed-success path.
fn recorded() -> Verdict {
    Verdict::from_findings(Vec::new())
}

fn is_fingerprint(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Canonicalize a user-supplied spec path. `Known(None)` = it does not exist
/// (a determined refusal for the caller); other failures are `Undetermined`.
fn canonical_spec(spec: &Path) -> Determination<Option<PathBuf>> {
    match std::fs::canonicalize(spec) {
        Ok(p) => Determination::known(Some(p)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Determination::known(None),
        Err(e) => Determination::undetermined(format!(
            "spec {} cannot be canonicalized: {e}",
            spec.display()
        )),
    }
}

/// Refuse to write state when the state directory is inside the target
/// (`Violation`); `Undetermined` when that cannot be decided.
pub(crate) fn state_outside_target(target: &ResolvedTarget, state_dir: &Path) -> Verdict {
    let effective = match std::fs::canonicalize(state_dir) {
        Ok(p) => p,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => state_dir.to_path_buf(),
        Err(e) => {
            return Verdict::undetermined(format!(
                "state directory {} cannot be canonicalized: {e}",
                state_dir.display()
            ))
        }
    };
    if effective.starts_with(&target.root) {
        return Verdict::violation(format!(
            "refused: the state directory {} is inside the target {}",
            effective.display(),
            target.root.display()
        ));
    }
    recorded()
}

/// Read the registration file `path`: `Known(None)` when absent, `Known(Some)`
/// when it parses with the current schema, `Undetermined` otherwise.
fn read_registration_file(path: &Path) -> Determination<Option<Registration>> {
    let reg: Registration = match state::read_json(path) {
        Determination::Known(Some(r)) => r,
        Determination::Known(None) => return Determination::known(None),
        Determination::Undetermined(why) => return Determination::Undetermined(why),
    };
    if reg.schema != SCHEMA {
        return Determination::undetermined(format!(
            "registration {} has unknown schema {}",
            path.display(),
            reg.schema
        ));
    }
    Determination::known(Some(reg))
}

/// Every target registered (by [`begin`]) under `state_dir`, sorted by root.
///
/// Enumerates `<state_dir>/targets/*/registration.json`.
///
/// * `Known(vec![])` — the `targets` directory does not exist (`NotFound`),
///   i.e. nothing was ever registered; or it holds no registration.
/// * `Known(targets)` — every per-target directory was read; a directory
///   without `registration.json` is not registered and is skipped, and a
///   non-directory entry of `targets/` is skipped (registrations only live
///   in per-target directories).
/// * `Undetermined` — the directory listing (or any entry's metadata) cannot
///   be read, or ANY registration is unreadable, corrupt, has an unknown
///   schema, a non-absolute root, or sits under a directory whose name is not
///   the state key of its root. One bad registration makes the whole answer
///   undetermined: its root is unknown, so no path can be shown to lie
///   outside it. Such a registration is removed only by [`forget`]
///   (`fde forget`), which does not parse it.
///
/// A registered target whose root no longer exists ("gone") is listed like
/// any other; whether its root exists is not checked here (the gate does it
/// with [`crate::target::presence`]).
#[track_caller]
pub fn registered_targets(state_dir: &Path) -> Determination<Vec<ResolvedTarget>> {
    let dir = state_dir.join("targets");
    let listing = match std::fs::read_dir(&dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Determination::known(vec![]),
        Err(e) => {
            return Determination::undetermined(format!(
                "cannot list registered targets in {}: {e}",
                dir.display()
            ))
        }
    };
    let mut targets = Vec::new();
    for entry in listing {
        let entry = match entry {
            Ok(en) => en,
            Err(e) => {
                return Determination::undetermined(format!(
                    "cannot list registered targets in {}: {e}",
                    dir.display()
                ))
            }
        };
        let path = entry.path();
        match std::fs::metadata(&path) {
            Ok(m) if m.is_dir() => {}
            Ok(_) => continue,
            Err(e) => {
                return Determination::undetermined(format!(
                    "cannot stat state entry {}: {e}",
                    path.display()
                ))
            }
        }
        let reg_path = path.join(REGISTRATION_FILE);
        let reg = match read_registration_file(&reg_path) {
            Determination::Known(Some(r)) => r,
            Determination::Known(None) => continue,
            Determination::Undetermined(why) => return Determination::Undetermined(why),
        };
        if !reg.root.is_absolute() {
            return Determination::undetermined(format!(
                "registration {} names a non-absolute root {}",
                reg_path.display(),
                reg.root.display()
            ));
        }
        let key = crate::target::state_key(&reg.root);
        if entry.file_name().to_str() != Some(key.as_str()) {
            return Determination::undetermined(format!(
                "registration {} names root {}, whose state key is not its directory name",
                reg_path.display(),
                reg.root.display()
            ));
        }
        targets.push(ResolvedTarget {
            root: reg.root,
            state_key: key,
        });
    }
    targets.sort_by(|a, b| a.root.cmp(&b.root));
    Determination::known(targets)
}

/// The target registered with exactly the root `candidate`, read from
/// `<state_dir>/targets/<state_key(candidate)>/registration.json` alone (no
/// listing, no other registration consulted).
///
/// * `Known(None)` — no registration under that key.
/// * `Known(Some(t))` — registered, and its recorded root is `candidate`.
/// * `Undetermined` — unreadable / corrupt / unknown schema, or a recorded
///   root that is not `candidate`.
#[track_caller]
pub fn registered_at(state_dir: &Path, candidate: &Path) -> Determination<Option<ResolvedTarget>> {
    let key = crate::target::state_key(candidate);
    let path = state::target_dir(state_dir, &key).join(REGISTRATION_FILE);
    match read_registration_file(&path) {
        Determination::Known(None) => Determination::known(None),
        Determination::Known(Some(reg)) if reg.root == candidate => {
            Determination::known(Some(ResolvedTarget {
                root: reg.root,
                state_key: key,
            }))
        }
        Determination::Known(Some(reg)) => Determination::undetermined(format!(
            "registration {} names root {}, not {}",
            path.display(),
            reg.root.display(),
            candidate.display()
        )),
        Determination::Undetermined(why) => Determination::Undetermined(why),
    }
}

/// Directory (under the state directory, outside `targets/`) a forgotten
/// target's state is moved into before it is deleted.
const FORGOTTEN_DIR: &str = ".forgotten";

/// Remove the whole state directory `<state_dir>/targets/<state_key>/` of
/// `root` (registration, ratification, evidence) — `fde forget`. The files
/// in it are not parsed, so a corrupt registration is removable too. Nothing
/// outside the state directory is touched (never the target).
///
/// The directory is first renamed (atomically) to
/// `<state_dir>/.forgotten/<state_key>.<pid>.<nanos>` and then deleted, so a
/// failure either leaves the registration fully in place or leaves it
/// unregistered; a half-deleted registration is never left under `targets/`.
///
/// * `Clean` — removed.
/// * `Violation` — refused: no state directory for that key (not registered).
/// * `Undetermined` — the state directory cannot be stat'ed, is not a
///   directory, cannot be moved (nothing was removed), or was moved out of
///   `targets/` (so it is no longer registered) but could not be deleted;
///   the reason says which.
#[track_caller]
pub fn forget(state_dir: &Path, root: &Path, state_key: &str) -> Verdict {
    let dir = state::target_dir(state_dir, state_key);
    match std::fs::symlink_metadata(&dir) {
        Ok(m) if m.is_dir() => {}
        Ok(_) => {
            return Verdict::undetermined(format!(
                "state entry {} for {} is not a directory; nothing was removed",
                dir.display(),
                root.display()
            ))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Verdict::violation(format!(
                "refused: target {} is not registered (no state directory {})",
                root.display(),
                dir.display()
            ))
        }
        Err(e) => {
            return Verdict::undetermined(format!(
                "cannot stat state directory {}: {e}; nothing was removed",
                dir.display()
            ))
        }
    }
    let trash_parent = state_dir.join(FORGOTTEN_DIR);
    if let Err(e) = std::fs::create_dir_all(&trash_parent) {
        return Verdict::undetermined(format!(
            "cannot create {}: {e}; nothing was removed",
            trash_parent.display()
        ));
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let trash = trash_parent.join(format!("{state_key}.{}.{nanos}", std::process::id()));
    if let Err(e) = std::fs::rename(&dir, &trash) {
        // Best-effort cleanup of the (possibly just created) empty parent; it
        // holds nothing, so its fate does not change the answer.
        let _ = std::fs::remove_dir(&trash_parent);
        return Verdict::undetermined(format!(
            "cannot remove the state directory {} of {}: {e}; nothing was removed, \
             the target is still registered",
            dir.display(),
            root.display()
        ));
    }
    if let Err(e) = std::fs::remove_dir_all(&trash) {
        return Verdict::undetermined(format!(
            "the state of {} was moved out of the registered targets to {} (it is no \
             longer registered), but deleting it failed: {e}; delete that directory by hand",
            root.display(),
            trash.display()
        ));
    }
    // Same best-effort cleanup: only succeeds when the parent is empty.
    let _ = std::fs::remove_dir(&trash_parent);
    recorded()
}

fn read_registration(
    target: &ResolvedTarget,
    state_dir: &Path,
) -> Determination<Option<Registration>> {
    let path = state::target_dir(state_dir, &target.state_key).join(REGISTRATION_FILE);
    let reg = match read_registration_file(&path) {
        Determination::Known(Some(r)) => r,
        Determination::Known(None) => return Determination::known(None),
        Determination::Undetermined(why) => return Determination::Undetermined(why),
    };
    if reg.root != target.root {
        return Determination::undetermined(format!(
            "registration {} names root {}, not the target {}",
            path.display(),
            reg.root.display(),
            target.root.display()
        ));
    }
    Determination::known(Some(reg))
}

fn read_ratification(
    target: &ResolvedTarget,
    state_dir: &Path,
) -> Determination<Option<(Ratification, Provenance)>> {
    let path = state::target_dir(state_dir, &target.state_key).join(RATIFICATION_FILE);
    let rat: Ratification = match state::read_json(&path) {
        Determination::Known(Some(r)) => r,
        Determination::Known(None) => return Determination::known(None),
        Determination::Undetermined(why) => return Determination::Undetermined(why),
    };
    let provenance = match (rat.schema, &rat.head, rat.dirty) {
        (RATIFICATION_SCHEMA, Some(head), Some(dirty)) if crate::git::is_commit_id(head) => {
            Provenance::Recorded {
                head: head.clone(),
                dirty,
            }
        }
        (RATIFICATION_SCHEMA, _, _) => {
            return Determination::undetermined(format!(
                "ratification {} (schema {RATIFICATION_SCHEMA}) lacks a well-formed head \
                 (40 lowercase hex) and dirty flag: head {:?}, dirty {:?}",
                path.display(),
                rat.head,
                rat.dirty
            ))
        }
        (LEGACY_RATIFICATION_SCHEMA, None, None) => Provenance::Legacy,
        (LEGACY_RATIFICATION_SCHEMA, _, _) => {
            return Determination::undetermined(format!(
                "ratification {} has schema {LEGACY_RATIFICATION_SCHEMA} but carries head/dirty \
                 fields that schema never had",
                path.display()
            ))
        }
        (other, _, _) => {
            return Determination::undetermined(format!(
                "ratification {} has unknown schema {other}",
                path.display()
            ))
        }
    };
    if !is_fingerprint(&rat.fingerprint) {
        return Determination::undetermined(format!(
            "ratification {} holds a malformed fingerprint {:?}",
            path.display(),
            rat.fingerprint
        ));
    }
    Determination::known(Some((rat, provenance)))
}

/// Register `target` with the draft `spec` under `state_dir`; the target is
/// unratified afterwards.
///
/// * `Clean` — registered.
/// * `Violation` — refused: the spec does not exist, is not a regular file,
///   lives inside the target, or the state directory is inside the target.
/// * `Undetermined` — anything that could not be read, decided or written.
#[track_caller]
pub fn begin(target: &ResolvedTarget, spec: &Path, state_dir: &Path) -> Verdict {
    let spec = match canonical_spec(spec) {
        Determination::Known(Some(p)) => p,
        Determination::Known(None) => {
            return Verdict::violation(format!("refused: spec {} does not exist", spec.display()))
        }
        Determination::Undetermined(why) => return why.into_verdict(),
    };
    match std::fs::metadata(&spec) {
        Ok(m) if m.is_file() => {}
        Ok(_) => {
            return Verdict::violation(format!(
                "refused: spec {} is not a regular file",
                spec.display()
            ))
        }
        Err(e) => {
            return Verdict::undetermined(format!("cannot stat spec {}: {e}", spec.display()))
        }
    }
    if spec.starts_with(&target.root) {
        return Verdict::violation(format!(
            "refused: spec {} is inside the target {}; it must live outside it",
            spec.display(),
            target.root.display()
        ));
    }
    match state_outside_target(target, state_dir) {
        Verdict::Clean(_) => {}
        other => return other,
    }
    let dir = state::target_dir(state_dir, &target.state_key);
    // Unratify first: a failed registration write below then leaves the
    // target unratified, never ratified against a stale record.
    if let Determination::Undetermined(why) = state::remove_if_present(&dir.join(RATIFICATION_FILE))
    {
        return why.into_verdict();
    }
    let reg = Registration {
        schema: SCHEMA,
        root: target.root.clone(),
        spec,
    };
    match state::write_json_atomic(&dir.join(REGISTRATION_FILE), &reg) {
        Determination::Known(()) => recorded(),
        Determination::Undetermined(why) => why.into_verdict(),
    }
}

/// Record the registered spec of `target` as ratified under `state_dir`, with
/// the fingerprint of its current bytes and the target HEAD commit and dirty
/// flag observed now. When `spec` is given it must name the registered spec.
///
/// * `Clean` — recorded.
/// * `Violation` — refused: the target is not registered, or `spec` is not the
///   registered spec (or does not exist).
/// * `Undetermined` — anything that could not be read, decided or written,
///   including a target HEAD or dirty flag that cannot be determined (e.g. a
///   repository without commits). Nothing is written then.
#[track_caller]
pub fn ratify(target: &ResolvedTarget, spec: Option<&Path>, state_dir: &Path) -> Verdict {
    let reg = match read_registration(target, state_dir) {
        Determination::Known(Some(r)) => r,
        Determination::Known(None) => {
            return Verdict::violation(format!(
                "refused: target {} is not registered (run `fde begin` first)",
                target.root.display()
            ))
        }
        Determination::Undetermined(why) => return why.into_verdict(),
    };
    if let Some(given) = spec {
        match canonical_spec(given) {
            Determination::Known(Some(p)) if p == reg.spec => {}
            Determination::Known(Some(p)) => {
                return Verdict::violation(format!(
                    "refused: spec {} is not the registered spec {}",
                    p.display(),
                    reg.spec.display()
                ))
            }
            Determination::Known(None) => {
                return Verdict::violation(format!(
                    "refused: spec {} does not exist",
                    given.display()
                ))
            }
            Determination::Undetermined(why) => return why.into_verdict(),
        }
    }
    match state_outside_target(target, state_dir) {
        Verdict::Clean(_) => {}
        other => return other,
    }
    let head = match crate::git::head(&target.root) {
        Determination::Known(h) => h,
        Determination::Undetermined(why) => {
            return Verdict::undetermined(format!(
                "cannot ratify {}: the target HEAD commit cannot be determined (a ratification \
                 needs a commit to record): {}",
                target.root.display(),
                why.as_str()
            ))
        }
    };
    let dirty = match crate::git::is_dirty(&target.root) {
        Determination::Known(d) => d,
        Determination::Undetermined(why) => {
            return Verdict::undetermined(format!(
                "cannot ratify {}: whether the target work tree is dirty cannot be determined: {}",
                target.root.display(),
                why.as_str()
            ))
        }
    };
    let bytes = match std::fs::read(&reg.spec) {
        Ok(b) => b,
        Err(e) => {
            return Verdict::undetermined(format!(
                "cannot read the registered spec {}: {e}",
                reg.spec.display()
            ))
        }
    };
    let rat = Ratification {
        schema: RATIFICATION_SCHEMA,
        spec: reg.spec,
        fingerprint: fingerprint(&bytes),
        head: Some(head),
        dirty: Some(dirty),
    };
    let path = state::target_dir(state_dir, &target.state_key).join(RATIFICATION_FILE);
    match state::write_json_atomic(&path, &rat) {
        Determination::Known(()) => recorded(),
        Determination::Undetermined(why) => why.into_verdict(),
    }
}

/// Observe the ratification state of `target` under `state_dir`.
///
/// * `Ratified` — registered, ratified, and the recorded fingerprint equals
///   the SHA-256 of the registered spec's current bytes.
/// * `Unratified` — not registered (reason says "not registered"), registered
///   but not ratified, ratified for a different spec path, or the spec bytes
///   changed since ratification.
/// * `Undetermined` — an unreadable or corrupt store, or (once a ratification
///   exists) an unreadable or missing spec.
#[track_caller]
pub fn status(target: &ResolvedTarget, state_dir: &Path) -> Determination<Status> {
    match observe(target, state_dir) {
        Determination::Known(Observed::Ratified(spec)) => Determination::known(Status::Ratified {
            fingerprint: spec.fingerprint,
        }),
        Determination::Known(Observed::Unratified { reason }) => {
            Determination::known(Status::Unratified { reason })
        }
        Determination::Undetermined(why) => Determination::Undetermined(why),
    }
}

/// The ratified spec of a target, as read in one observation: the bytes
/// whose SHA-256 is `fingerprint` are exactly `bytes` (no second read).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RatifiedSpec {
    /// Canonical path of the registered (and ratified) spec.
    pub path: PathBuf,
    /// Lowercase hex SHA-256 of `bytes`, equal to the recorded ratification.
    pub fingerprint: String,
    /// The spec bytes that were read and fingerprinted.
    pub bytes: Vec<u8>,
    /// The target commit the ratification was taken at.
    pub provenance: Provenance,
}

/// [`status`] with the ratified spec bytes attached, for callers (the
/// evidence ledger) that must interpret the very bytes that were checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Observed {
    /// See [`Status::Ratified`].
    Ratified(RatifiedSpec),
    /// See [`Status::Unratified`].
    Unratified {
        /// Why the target is not ratified (for stderr).
        reason: String,
    },
}

/// Same answers as [`status`], carrying the ratified spec bytes and the
/// ratification's [`Provenance`] on `Ratified`. A legacy (schema-1)
/// ratification is `Ratified` with [`Provenance::Legacy`]; deciding what that
/// means for evidence is the caller's job.
#[track_caller]
pub fn observe(target: &ResolvedTarget, state_dir: &Path) -> Determination<Observed> {
    let reg = match read_registration(target, state_dir) {
        Determination::Known(Some(r)) => r,
        Determination::Known(None) => {
            return Determination::known(Observed::Unratified {
                reason: format!("target {} is not registered", target.root.display()),
            })
        }
        Determination::Undetermined(why) => return Determination::Undetermined(why),
    };
    let (rat, provenance) = match read_ratification(target, state_dir) {
        Determination::Known(Some(r)) => r,
        Determination::Known(None) => {
            return Determination::known(Observed::Unratified {
                reason: format!(
                    "target {} is registered with spec {} but not ratified",
                    target.root.display(),
                    reg.spec.display()
                ),
            })
        }
        Determination::Undetermined(why) => return Determination::Undetermined(why),
    };
    if rat.spec != reg.spec {
        return Determination::known(Observed::Unratified {
            reason: format!(
                "the ratification is for spec {}, but the registered spec is {}",
                rat.spec.display(),
                reg.spec.display()
            ),
        });
    }
    let bytes = match std::fs::read(&reg.spec) {
        Ok(b) => b,
        Err(e) => {
            return Determination::undetermined(format!(
                "cannot read the ratified spec {}: {e}",
                reg.spec.display()
            ))
        }
    };
    let current = fingerprint(&bytes);
    if current != rat.fingerprint {
        return Determination::known(Observed::Unratified {
            reason: format!(
                "spec {} changed since ratification (fingerprint {} now {current})",
                reg.spec.display(),
                rat.fingerprint
            ),
        });
    }
    Determination::known(Observed::Ratified(RatifiedSpec {
        path: reg.spec,
        fingerprint: current,
        bytes,
        provenance,
    }))
}
