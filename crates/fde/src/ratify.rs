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
//! * `ratification.json` — `{schema, spec, fingerprint}`: written by
//!   [`ratify`]. `fingerprint` is the lowercase hex SHA-256 of the spec file
//!   bytes at ratification time; `spec` repeats the registered spec path so a
//!   ratification left over from a different registration never counts.
//!
//! Both files are written atomically (temp file + rename). [`begin`] removes
//! any previous `ratification.json` *before* writing the registration, so a
//! (re-)registered target is unratified even if the registration write fails
//! afterwards.
//!
//! The target HEAD commit is **not** recorded at ratification time (not part
//! of this record yet).
//!
//! # Answers
//!
//! Every IO error and every parse error (including an unknown `schema`, a
//! registration whose `root` is not the target, or a malformed fingerprint) is
//! `Undetermined` — never `Ratified`, never `Unratified`.

use std::path::{Path, PathBuf};

use harness_core::verdict::{Determination, Verdict};
use sha2::{Digest, Sha256};

use crate::state;
use crate::target::ResolvedTarget;

/// Schema version of both state records.
const SCHEMA: u32 = 1;
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

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Ratification {
    schema: u32,
    spec: PathBuf,
    fingerprint: String,
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
fn state_outside_target(target: &ResolvedTarget, state_dir: &Path) -> Verdict {
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

fn read_registration(
    target: &ResolvedTarget,
    state_dir: &Path,
) -> Determination<Option<Registration>> {
    let path = state::target_dir(state_dir, &target.state_key).join(REGISTRATION_FILE);
    let reg: Registration = match state::read_json(&path) {
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
) -> Determination<Option<Ratification>> {
    let path = state::target_dir(state_dir, &target.state_key).join(RATIFICATION_FILE);
    let rat: Ratification = match state::read_json(&path) {
        Determination::Known(Some(r)) => r,
        Determination::Known(None) => return Determination::known(None),
        Determination::Undetermined(why) => return Determination::Undetermined(why),
    };
    if rat.schema != SCHEMA {
        return Determination::undetermined(format!(
            "ratification {} has unknown schema {}",
            path.display(),
            rat.schema
        ));
    }
    if !is_fingerprint(&rat.fingerprint) {
        return Determination::undetermined(format!(
            "ratification {} holds a malformed fingerprint {:?}",
            path.display(),
            rat.fingerprint
        ));
    }
    Determination::known(Some(rat))
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
/// the fingerprint of its current bytes. When `spec` is given it must name the
/// registered spec.
///
/// * `Clean` — recorded.
/// * `Violation` — refused: the target is not registered, or `spec` is not the
///   registered spec (or does not exist).
/// * `Undetermined` — anything that could not be read, decided or written.
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
        schema: SCHEMA,
        spec: reg.spec,
        fingerprint: fingerprint(&bytes),
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
    let reg = match read_registration(target, state_dir) {
        Determination::Known(Some(r)) => r,
        Determination::Known(None) => {
            return Determination::known(Status::Unratified {
                reason: format!("target {} is not registered", target.root.display()),
            })
        }
        Determination::Undetermined(why) => return Determination::Undetermined(why),
    };
    let rat = match read_ratification(target, state_dir) {
        Determination::Known(Some(r)) => r,
        Determination::Known(None) => {
            return Determination::known(Status::Unratified {
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
        return Determination::known(Status::Unratified {
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
        return Determination::known(Status::Unratified {
            reason: format!(
                "spec {} changed since ratification (fingerprint {} now {current})",
                reg.spec.display(),
                rat.fingerprint
            ),
        });
    }
    Determination::known(Status::Ratified {
        fingerprint: current,
    })
}
