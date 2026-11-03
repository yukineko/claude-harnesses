//! Ratification store: a spec ratified for a target, kept in the state
//! directory (never inside the target), identified by a content fingerprint.
//!
//! SCAFFOLD: every function here answers `Undetermined`.

use std::path::Path;

use harness_core::verdict::{Determination, Verdict};

use crate::target::ResolvedTarget;

/// Ratification state of a target, as observed.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[expect(
    dead_code,
    reason = "scaffold: constructed by status once implemented; this expect then fails and must be removed"
)]
pub enum Status {
    /// A ratified spec is recorded for the target and its fingerprint matches.
    Ratified {
        /// Content fingerprint of the ratified spec.
        fingerprint: String,
    },
    /// No (or no longer valid) ratification is recorded for the target.
    Unratified,
}

/// Record `spec` as the ratified spec of `target` under `state_dir`.
/// `Clean` = recorded; anything that could not be read or written is
/// `Undetermined`.
///
/// SCAFFOLD: always `Undetermined`.
#[track_caller]
pub fn ratify(target: &ResolvedTarget, spec: &Path, state_dir: &Path) -> Verdict {
    let _ = (target, spec, state_dir);
    Verdict::undetermined("fde: ratify is not implemented (scaffold)")
}

/// Observe the ratification state of `target` under `state_dir`. An unreadable
/// or unparsable store is `Undetermined`, never `Unratified` or `Ratified`.
///
/// SCAFFOLD: always `Undetermined`.
#[track_caller]
pub fn status(target: &ResolvedTarget, state_dir: &Path) -> Determination<Status> {
    let _ = (target, state_dir);
    Determination::undetermined("fde: status is not implemented (scaffold)")
}
